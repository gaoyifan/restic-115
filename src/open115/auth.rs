//! Token management for 115 Open Platform authentication.

use chrono::{DateTime, Duration, Utc};
use parking_lot::RwLock;
use reqwest::Client;
use std::sync::Arc;

use super::database::entities::tokens;
use super::types::RefreshTokenResponse;
use crate::error::{AppError, Result};
use sea_orm::{ActiveModelTrait, DatabaseConnection, EntityTrait, Set};

const REFRESH_URL: &str = "https://passportapi.115.com/open/refreshToken";

const MAX_RATE_LIMIT_RETRIES: usize = 6;

fn is_refresh_rate_limited(code: i64) -> bool {
    // See docs/115/接入指南/授权错误码.md
    code == 40140117
}

async fn backoff_sleep(attempt: usize) {
    // Exponential backoff with a cap.
    // attempt starts at 1.
    // Keep the cap small so refresh can't stall the process for minutes.
    let secs = (1u64 << (attempt - 1)).min(16);
    tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
}

#[derive(Debug, Clone)]
struct TokenInfo {
    access_token: String,
    refresh_token: String,
    expires_at: Option<DateTime<Utc>>,
}

impl TokenInfo {
    fn is_expired(&self) -> bool {
        let Some(expires_at) = self.expires_at else {
            return false;
        };
        Utc::now() + Duration::minutes(5) >= expires_at
    }
}

/// Token manager that handles refresh and provides a valid access token.
#[derive(Clone)]
pub struct TokenManager {
    http_client: Client,
    db: DatabaseConnection,
    token: Arc<RwLock<Option<TokenInfo>>>,
}

impl TokenManager {
    pub async fn new(
        db: DatabaseConnection,
        access_token: Option<String>,
        refresh_token: Option<String>,
    ) -> Result<Self> {
        let http_client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client");

        let this = Self {
            http_client,
            db,
            token: Arc::new(RwLock::new(None)),
        };

        // Try load from DB
        let db_token = tokens::Entity::find_by_id(1)
            .one(&this.db)
            .await
            .map_err(|e| AppError::Internal(format!("DB error loading tokens: {e}")))?;

        let (a, r) = if let Some(t) = db_token {
            (t.access_token, t.refresh_token)
        } else if let (Some(a), Some(r)) = (access_token, refresh_token) {
            // No DB token, but have env tokens; store them
            let am = tokens::ActiveModel {
                id: Set(1),
                access_token: Set(a.clone()),
                refresh_token: Set(r.clone()),
                updated_at: Set(Utc::now()),
            };
            am.insert(&this.db)
                .await
                .map_err(|e| AppError::Internal(format!("DB error saving tokens: {e}")))?;
            (a, r)
        } else {
            return Ok(this);
        };

        {
            let mut guard = this.token.write();
            *guard = Some(TokenInfo {
                access_token: a,
                refresh_token: r,
                expires_at: None,
            });
        }

        Ok(this)
    }

    pub fn http_client(&self) -> &Client {
        &self.http_client
    }

    pub fn refresh_token_value(&self) -> Option<String> {
        self.token.read().as_ref().map(|t| t.refresh_token.clone())
    }

    pub fn access_token_value(&self) -> Option<String> {
        self.token.read().as_ref().map(|t| t.access_token.clone())
    }

    pub async fn get_token(&self) -> Result<String> {
        {
            let guard = self.token.read();
            if let Some(t) = guard.as_ref()
                && !t.is_expired()
            {
                return Ok(t.access_token.clone());
            }
        }
        self.refresh_token().await
    }

    pub async fn refresh_token(&self) -> Result<String> {
        let refresh = {
            let guard = self.token.read();
            guard
                .as_ref()
                .map(|t| t.refresh_token.clone())
                .ok_or_else(|| {
                    AppError::Auth(
                        "Missing refresh token. Obtain tokens via callback server and set OPEN115_ACCESS_TOKEN/OPEN115_REFRESH_TOKEN."
                            .to_string(),
                    )
                })?
        };

        tracing::info!("Refreshing 115 access token");
        let body: RefreshTokenResponse = {
            let mut last_err: Option<AppError> = None;
            let mut ok_body: Option<RefreshTokenResponse> = None;
            for attempt in 1..=MAX_RATE_LIMIT_RETRIES {
                let response = self
                    .http_client
                    .post(REFRESH_URL)
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .form(&[("refresh_token", refresh.as_str())])
                    .send()
                    .await;

                let response = match response {
                    Ok(r) => r,
                    Err(e) => {
                        // Network/timeout: treat as retryable with backoff.
                        if attempt < MAX_RATE_LIMIT_RETRIES {
                            tracing::warn!(
                                "refreshToken network error, backing off attempt {}/{}: {}",
                                attempt,
                                MAX_RATE_LIMIT_RETRIES,
                                e
                            );
                            last_err = Some(AppError::HttpClient(e));
                            backoff_sleep(attempt).await;
                            continue;
                        }
                        return Err(AppError::HttpClient(e));
                    }
                };

                let parsed = response.json::<RefreshTokenResponse>().await;
                let body = match parsed {
                    Ok(b) => b,
                    Err(e) => {
                        if attempt < MAX_RATE_LIMIT_RETRIES {
                            tracing::warn!(
                                "refreshToken JSON parse error, backing off attempt {}/{}: {}",
                                attempt,
                                MAX_RATE_LIMIT_RETRIES,
                                e
                            );
                            last_err = Some(AppError::HttpClient(e));
                            backoff_sleep(attempt).await;
                            continue;
                        }
                        return Err(AppError::HttpClient(e));
                    }
                };

                let ok = body.state.unwrap_or(false);
                let code = body.code.unwrap_or(-1);
                if ok && code == 0 {
                    ok_body = Some(body);
                    break;
                }

                if is_refresh_rate_limited(code) && attempt < MAX_RATE_LIMIT_RETRIES {
                    tracing::warn!(
                        "refreshToken rate limited (code={}), backing off attempt {}/{}",
                        code,
                        attempt,
                        MAX_RATE_LIMIT_RETRIES
                    );
                    last_err = Some(AppError::Auth(format!(
                        "Failed to refresh token: code={}, message={}",
                        code,
                        body.message.clone().unwrap_or_default()
                    )));
                    backoff_sleep(attempt).await;
                    continue;
                }

                return Err(AppError::Auth(format!(
                    "Failed to refresh token: code={}, message={}",
                    code,
                    body.message.unwrap_or_default()
                )));
            }
            ok_body.ok_or_else(|| {
                last_err.unwrap_or_else(|| {
                    AppError::Auth("Failed to refresh token: exhausted retries".to_string())
                })
            })?
        };

        let data = body
            .data
            .ok_or_else(|| AppError::Auth("No data in refresh token response".to_string()))?;

        let access_token = data.access_token.ok_or_else(|| {
            AppError::Auth("Refresh token succeeded but missing access_token".to_string())
        })?;
        let refresh_token = data.refresh_token.ok_or_else(|| {
            AppError::Auth("Refresh token succeeded but missing refresh_token".to_string())
        })?;

        let expires_at = data.expires_in.map(|s| Utc::now() + Duration::seconds(s));

        {
            let mut guard = self.token.write();
            *guard = Some(TokenInfo {
                access_token: access_token.clone(),
                refresh_token: refresh_token.clone(),
                expires_at,
            });
        }

        // Persist refreshed tokens to DB
        let am = tokens::ActiveModel {
            id: Set(1),
            access_token: Set(access_token.clone()),
            refresh_token: Set(refresh_token.clone()),
            updated_at: Set(Utc::now()),
        };
        tokens::Entity::insert(am)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(tokens::Column::Id)
                    .update_columns([
                        tokens::Column::AccessToken,
                        tokens::Column::RefreshToken,
                        tokens::Column::UpdatedAt,
                    ])
                    .to_owned(),
            )
            .exec(&self.db)
            .await
            .map_err(|e| AppError::Internal(format!("DB error updating tokens: {e}")))?;

        Ok(access_token)
    }
}

impl std::fmt::Debug for TokenManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenManager")
            .field("has_token", &self.token.read().is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    // Tests for persist_tokens_to_file were removed as the function is removed.
    // Database logic is better tested in integration tests.
}