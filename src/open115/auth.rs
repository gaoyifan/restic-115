//! Token management for 115 Open Platform authentication.

use chrono::{DateTime, Duration, Utc};
use parking_lot::RwLock;
use reqwest::Client;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::types::RefreshTokenResponse;
use crate::error::{AppError, Result};

const REFRESH_URL: &str = "https://passportapi.115.com/open/refreshToken";
const DEFAULT_TOKEN_STORE_PATH: &str = ".env";

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

fn parse_boolish(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "t" | "yes" | "y" | "on" => Some(true),
        "0" | "false" | "f" | "no" | "n" | "off" => Some(false),
        _ => None,
    }
}

fn should_persist_tokens() -> bool {
    std::env::var("OPEN115_PERSIST_TOKENS")
        .ok()
        .and_then(|v| parse_boolish(&v))
        .unwrap_or(false)
}

fn token_store_path() -> PathBuf {
    std::env::var("OPEN115_TOKEN_STORE_PATH")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_TOKEN_STORE_PATH))
}

fn upsert_env_var_line(line: &str, key: &str, value: &str) -> Option<String> {
    // Preserve comments and unrelated lines; replace only a plain `KEY=...` line.
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix(key) {
        let rest = rest.trim_start();
        if rest.starts_with('=') {
            return Some(format!("{key}={value}"));
        }
    }
    None
}

fn persist_tokens_to_file(
    path: &Path,
    access_token: &str,
    refresh_token: &str,
) -> std::io::Result<()> {
    use std::fs;
    use std::io::Write;

    let content = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };

    // Normalize line endings to '\n' for editing; preserve trailing newline.
    let had_trailing_newline = content.ends_with('\n');
    let mut lines: Vec<String> = content.split('\n').map(|s| s.to_string()).collect();

    // When file ends with '\n', split() gives last empty entry; keep it for stable rewrite.
    let mut seen_access = false;
    let mut seen_refresh = false;

    for l in &mut lines {
        if let Some(new) = upsert_env_var_line(l, "OPEN115_ACCESS_TOKEN", access_token) {
            *l = new;
            seen_access = true;
            continue;
        }
        if let Some(new) = upsert_env_var_line(l, "OPEN115_REFRESH_TOKEN", refresh_token) {
            *l = new;
            seen_refresh = true;
            continue;
        }
    }

    // Append missing keys (avoid duplicating if file was empty).
    if !seen_access {
        lines.push(format!("OPEN115_ACCESS_TOKEN={access_token}"));
    }
    if !seen_refresh {
        lines.push(format!("OPEN115_REFRESH_TOKEN={refresh_token}"));
    }

    // Rebuild content
    let mut new_content = lines.join("\n");
    if had_trailing_newline && !new_content.ends_with('\n') {
        new_content.push('\n');
    } else if !had_trailing_newline && new_content.ends_with('\n') {
        // fine
    }

    // Atomic-ish write: write temp then rename.
    let tmp_path = path.with_extension("tmp.restic-115");
    {
        let mut f = fs::File::create(&tmp_path)?;
        f.write_all(new_content.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp_path, path)?;
    Ok(())
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
    token: Arc<RwLock<Option<TokenInfo>>>,
}

impl TokenManager {
    pub fn new(access_token: Option<String>, refresh_token: Option<String>) -> Self {
        let http_client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client");

        let token = match (access_token, refresh_token) {
            (Some(a), Some(r)) => Some(TokenInfo {
                access_token: a,
                refresh_token: r,
                expires_at: None,
            }),
            _ => None,
        };

        Self {
            http_client,
            token: Arc::new(RwLock::new(token)),
        }
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
            if let Some(t) = guard.as_ref() {
                if !t.is_expired() {
                    return Ok(t.access_token.clone());
                }
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
                    AppError::Auth(format!(
                        "Missing refresh token. Obtain tokens via callback server and set OPEN115_ACCESS_TOKEN/OPEN115_REFRESH_TOKEN."
                    ))
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

        // Persist refreshed tokens for future runs (opt-in).
        if should_persist_tokens() {
            let path = token_store_path();
            match persist_tokens_to_file(&path, &access_token, &refresh_token) {
                Ok(()) => {
                    tracing::info!(
                        "Persisted refreshed tokens to {:?} (OPEN115_PERSIST_TOKENS enabled)",
                        path
                    );
                }
                Err(e) => {
                    tracing::warn!("Failed to persist refreshed tokens to {:?}: {}", path, e);
                }
            }
        }

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
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_upsert_env_var_line() {
        // Normal case
        assert_eq!(
            upsert_env_var_line("OPEN115_ACCESS_TOKEN=old", "OPEN115_ACCESS_TOKEN", "new"),
            Some("OPEN115_ACCESS_TOKEN=new".to_string())
        );
        // With spaces (the fix)
        assert_eq!(
            upsert_env_var_line("OPEN115_ACCESS_TOKEN = old", "OPEN115_ACCESS_TOKEN", "new"),
            Some("OPEN115_ACCESS_TOKEN=new".to_string())
        );
        assert_eq!(
            upsert_env_var_line(
                "  OPEN115_ACCESS_TOKEN  =  old  ",
                "OPEN115_ACCESS_TOKEN",
                "new"
            ),
            Some("OPEN115_ACCESS_TOKEN=new".to_string())
        );
        // Comments should be ignored
        assert_eq!(
            upsert_env_var_line("# OPEN115_ACCESS_TOKEN=old", "OPEN115_ACCESS_TOKEN", "new"),
            None
        );
        // Unrelated lines should be ignored
        assert_eq!(
            upsert_env_var_line("SOME_OTHER_VAR=val", "OPEN115_ACCESS_TOKEN", "new"),
            None
        );
    }

    #[test]
    fn test_persist_tokens_to_file() -> std::io::Result<()> {
        let mut tmp = NamedTempFile::new()?;
        let path = tmp.path().to_path_buf();

        // 1. Initial creation
        persist_tokens_to_file(&path, "acc1", "ref1")?;
        let content = std::fs::read_to_string(&path)?;
        assert!(content.contains("OPEN115_ACCESS_TOKEN=acc1"));
        assert!(content.contains("OPEN115_REFRESH_TOKEN=ref1"));

        // 2. Update existing (one with spaces)
        {
            let mut f = std::fs::File::create(&path)?;
            writeln!(f, "OPEN115_ACCESS_TOKEN = acc1")?;
            writeln!(f, "OPEN115_REFRESH_TOKEN=ref1")?;
            writeln!(f, "# comment")?;
        }
        persist_tokens_to_file(&path, "acc2", "ref2")?;
        let content = std::fs::read_to_string(&path)?;
        assert!(content.contains("OPEN115_ACCESS_TOKEN=acc2"));
        assert!(content.contains("OPEN115_REFRESH_TOKEN=ref2"));
        assert!(!content.contains("acc1"));
        assert!(!content.contains("ref1"));
        assert!(content.contains("# comment"));
        // Ensure no duplicates
        assert_eq!(content.matches("OPEN115_ACCESS_TOKEN=").count(), 1);
        assert_eq!(content.matches("OPEN115_REFRESH_TOKEN=").count(), 1);

        Ok(())
    }
}
