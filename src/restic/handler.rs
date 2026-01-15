//! Restic REST API v2 handlers.

use axum::{
    Router,
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, head, post},
};
use serde::Deserialize;
use std::sync::Arc;

use super::types::FileEntryV2;
use crate::error::{AppError, Result};
use crate::open115::{Open115Client, ResticFileType};

/// Application state shared across handlers.
#[derive(Clone)]
pub struct AppState {
    pub client: Open115Client,
}

/// Query parameters for repository creation.
#[derive(Debug, Deserialize)]
pub struct CreateQuery {
    #[serde(default)]
    pub create: Option<bool>,
}

/// Restic REST API v2 content type.
const V2_CONTENT_TYPE: &str = "application/vnd.x.restic.rest.v2";

/// Create the Axum router with all routes.
pub fn create_router(client: Open115Client) -> Router {
    let state = Arc::new(AppState { client });

    Router::new()
        .route("/", post(create_repository).delete(delete_repository))
        .route(
            "/config",
            head(head_config).get(get_config).post(post_config),
        )
        .route("/:type/", get(list_files))
        .route(
            "/:type/:name",
            head(head_file)
                .get(get_file)
                .post(post_file)
                .delete(delete_file),
        )
        .with_state(state)
}

// ============================================================================
// Repository Operations
// ============================================================================

async fn create_repository(
    State(state): State<Arc<AppState>>,
    Query(query): Query<CreateQuery>,
) -> Result<impl IntoResponse> {
    if query.create != Some(true) {
        return Err(AppError::BadRequest(
            "Missing create=true parameter".to_string(),
        ));
    }

    tracing::info!("Creating repository");
    state.client.init_repository().await?;
    Ok(StatusCode::OK)
}

async fn delete_repository() -> impl IntoResponse {
    StatusCode::NOT_IMPLEMENTED
}

// ============================================================================
// Config Operations
// ============================================================================

async fn head_config(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse> {
    // Read-only: do NOT create directories on HEAD/GET.
    let dir_id = state
        .client
        .find_type_dir_id(ResticFileType::Config)
        .await?
        .ok_or_else(|| AppError::NotFound("config".to_string()))?;

    // After upload, search indexing can lag. Repo root is small; allow listing fallback.
    match state
        .client
        .get_file_info_with_fallback(&dir_id, "config", true)
        .await?
    {
        Some(file) => {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_LENGTH,
                file.size.to_string().parse().unwrap(),
            );
            Ok((StatusCode::OK, headers))
        }
        None => Err(AppError::NotFound("config".to_string())),
    }
}

async fn get_config(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse> {
    // Read-only: do NOT create directories on HEAD/GET.
    let dir_id = state
        .client
        .find_type_dir_id(ResticFileType::Config)
        .await?
        .ok_or_else(|| AppError::NotFound("config".to_string()))?;

    let file = state
        .client
        .get_file_info_with_fallback(&dir_id, "config", true)
        .await?
        .ok_or_else(|| AppError::NotFound("config".to_string()))?;

    let data = state.client.download_file(&file.pick_code, None).await?;

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "application/octet-stream".parse().unwrap(),
    );
    headers.insert(
        header::CONTENT_LENGTH,
        data.len().to_string().parse().unwrap(),
    );

    Ok((headers, data))
}

async fn post_config(
    State(state): State<Arc<AppState>>,
    body: axum::body::Body,
) -> Result<impl IntoResponse> {
    let body = axum::body::to_bytes(body, 1024 * 1024 * 1024)
        .await
        .map_err(|e| AppError::BadRequest(format!("Failed to read request body: {}", e)))?;

    tracing::info!("Saving config ({} bytes)", body.len());
    let dir_id = state.client.get_type_dir_id(ResticFileType::Config).await?;
    // Config is immediately read by restic; wait for visibility (via search API, no dir listing).
    state
        .client
        .upload_file(&dir_id, "config", body, false)
        .await?;
    Ok(StatusCode::OK)
}

// ============================================================================
// File Listing
// ============================================================================

async fn list_files(
    State(state): State<Arc<AppState>>,
    Path(type_str): Path<String>,
) -> Result<Response> {
    let file_type = ResticFileType::from_str(&type_str)
        .ok_or_else(|| AppError::BadRequest(format!("Invalid type: {}", type_str)))?;

    if file_type.is_config() {
        return Err(AppError::BadRequest(
            "Use /config endpoint for config".to_string(),
        ));
    }

    let files = if file_type == ResticFileType::Data {
        state.client.list_all_data_files().await?
    } else {
        // Read-only listing: if the repo/type dir doesn't exist yet, return empty list.
        match state.client.find_type_dir_id(file_type).await? {
            Some(dir_id) => state.client.list_files(&dir_id).await?,
            None => Vec::new(),
        }
    };

    let entries: Vec<FileEntryV2> = files
        .iter()
        .filter(|f| !f.is_dir)
        .map(|f| FileEntryV2 {
            name: f.filename.clone(),
            size: f.size as u64,
        })
        .collect();

    let body = serde_json::to_string(&entries)?;

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, V2_CONTENT_TYPE)
        .body(Body::from(body))
        .unwrap())
}

// ============================================================================
// Individual File Operations
// ============================================================================

async fn head_file(
    State(state): State<Arc<AppState>>,
    Path((type_str, name)): Path<(String, String)>,
) -> Result<impl IntoResponse> {
    let file_type = ResticFileType::from_str(&type_str)
        .ok_or_else(|| AppError::BadRequest(format!("Invalid type: {}", type_str)))?;

    // Read-only: do NOT create directories on HEAD/GET/DELETE.
    let dir_id = if file_type == ResticFileType::Data {
        state
            .client
            .find_data_file_dir_id(&name)
            .await?
            .ok_or_else(|| AppError::NotFound(name.clone()))?
    } else {
        state
            .client
            .find_type_dir_id(file_type)
            .await?
            .ok_or_else(|| AppError::NotFound(name.clone()))?
    };

    // Avoid listing inside data hash subdirs; allow listing fallback for non-data dirs only.
    let allow_list_fallback = file_type != ResticFileType::Data;
    match state
        .client
        .get_file_info_with_fallback(&dir_id, &name, allow_list_fallback)
        .await?
    {
        Some(file) => {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_LENGTH,
                file.size.to_string().parse().unwrap(),
            );
            Ok((StatusCode::OK, headers))
        }
        None => Err(AppError::NotFound(name)),
    }
}

#[derive(Debug)]
enum RangeParseError {
    Invalid,
    Unsatisfiable,
}

fn parse_range(
    header_val: &str,
    file_size: u64,
) -> std::result::Result<(u64, u64), RangeParseError> {
    // Only a single range is supported (sufficient for restic).
    let range_spec = header_val
        .strip_prefix("bytes=")
        .ok_or(RangeParseError::Invalid)?;
    let parts: Vec<&str> = range_spec.split('-').collect();
    if parts.len() != 2 {
        return Err(RangeParseError::Invalid);
    }
    // For empty files, any byte range is unsatisfiable.
    if file_size == 0 {
        return Err(RangeParseError::Unsatisfiable);
    }

    let start: u64 = if parts[0].is_empty() {
        // bytes=-N means last N bytes
        let suffix_len: u64 = parts[1].parse().map_err(|_| RangeParseError::Invalid)?;
        file_size.saturating_sub(suffix_len)
    } else {
        parts[0].parse().map_err(|_| RangeParseError::Invalid)?
    };
    let end: u64 = if parts[1].is_empty() {
        file_size - 1
    } else {
        parts[1].parse().map_err(|_| RangeParseError::Invalid)?
    };

    if start <= end && start < file_size {
        Ok((start, end.min(file_size - 1)))
    } else {
        Err(RangeParseError::Unsatisfiable)
    }
}

async fn get_file(
    State(state): State<Arc<AppState>>,
    Path((type_str, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<impl IntoResponse> {
    let file_type = ResticFileType::from_str(&type_str)
        .ok_or_else(|| AppError::BadRequest(format!("Invalid type: {}", type_str)))?;

    // Read-only: do NOT create directories on HEAD/GET/DELETE.
    let dir_id = if file_type == ResticFileType::Data {
        state
            .client
            .find_data_file_dir_id(&name)
            .await?
            .ok_or_else(|| AppError::NotFound(name.clone()))?
    } else {
        state
            .client
            .find_type_dir_id(file_type)
            .await?
            .ok_or_else(|| AppError::NotFound(name.clone()))?
    };

    let file = state
        .client
        .get_file_info_with_fallback(&dir_id, &name, file_type != ResticFileType::Data)
        .await?
        .ok_or_else(|| AppError::NotFound(name.clone()))?;

    let file_size = file.size as u64;

    let range_hdr = headers.get(header::RANGE).and_then(|v| v.to_str().ok());

    if let Some(r) = range_hdr {
        let (start, end) = match parse_range(r, file_size) {
            Ok(v) => v,
            Err(RangeParseError::Invalid) => {
                return Err(AppError::BadRequest("Invalid Range header".to_string()));
            }
            Err(RangeParseError::Unsatisfiable) => {
                let mut resp_headers = HeaderMap::new();
                resp_headers.insert(header::ACCEPT_RANGES, "bytes".parse().unwrap());
                resp_headers.insert(
                    header::CONTENT_RANGE,
                    format!("bytes */{}", file_size).parse().unwrap(),
                );
                resp_headers.insert(header::CONTENT_LENGTH, "0".parse().unwrap());
                return Ok((
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    resp_headers,
                    Bytes::new(),
                )
                    .into_response());
            }
        };
        let data = state
            .client
            .download_file(&file.pick_code, Some((start, end)))
            .await?;

        let content_range = format!("bytes {}-{}/{}", start, end, file_size);
        let mut resp_headers = HeaderMap::new();
        resp_headers.insert(
            header::CONTENT_TYPE,
            "application/octet-stream".parse().unwrap(),
        );
        resp_headers.insert(header::ACCEPT_RANGES, "bytes".parse().unwrap());
        resp_headers.insert(
            header::CONTENT_LENGTH,
            data.len().to_string().parse().unwrap(),
        );
        resp_headers.insert(header::CONTENT_RANGE, content_range.parse().unwrap());
        Ok((StatusCode::PARTIAL_CONTENT, resp_headers, data).into_response())
    } else {
        let data = state.client.download_file(&file.pick_code, None).await?;
        let mut resp_headers = HeaderMap::new();
        resp_headers.insert(
            header::CONTENT_TYPE,
            "application/octet-stream".parse().unwrap(),
        );
        resp_headers.insert(header::ACCEPT_RANGES, "bytes".parse().unwrap());
        resp_headers.insert(
            header::CONTENT_LENGTH,
            data.len().to_string().parse().unwrap(),
        );
        Ok((StatusCode::OK, resp_headers, data).into_response())
    }
}

async fn post_file(
    State(state): State<Arc<AppState>>,
    Path((type_str, name)): Path<(String, String)>,
    body: axum::body::Body,
) -> Result<impl IntoResponse> {
    let body = axum::body::to_bytes(body, 1024 * 1024 * 1024)
        .await
        .map_err(|e| AppError::BadRequest(format!("Failed to read request body: {}", e)))?;

    let file_type = ResticFileType::from_str(&type_str)
        .ok_or_else(|| AppError::BadRequest(format!("Invalid type: {}", type_str)))?;

    tracing::info!("Uploading {}/{} ({} bytes)", type_str, name, body.len());

    let dir_id = if file_type == ResticFileType::Data {
        state.client.get_data_file_dir_id(&name).await?
    } else {
        state.client.get_type_dir_id(file_type).await?
    };

    state
        .client
        .upload_file(&dir_id, &name, body, false)
        .await?;
    Ok(StatusCode::OK)
}

async fn delete_file(
    State(state): State<Arc<AppState>>,
    Path((type_str, name)): Path<(String, String)>,
) -> Result<impl IntoResponse> {
    let file_type = ResticFileType::from_str(&type_str)
        .ok_or_else(|| AppError::BadRequest(format!("Invalid type: {}", type_str)))?;

    tracing::info!("Deleting {}/{}", type_str, name);

    // Read-only: do NOT create directories on HEAD/GET/DELETE.
    let dir_id = if file_type == ResticFileType::Data {
        match state.client.find_data_file_dir_id(&name).await? {
            Some(id) => id,
            None => return Ok(StatusCode::OK),
        }
    } else {
        match state.client.find_type_dir_id(file_type).await? {
            Some(id) => id,
            None => return Ok(StatusCode::OK),
        }
    };

    if let Some(file) = state
        .client
        .get_file_info_with_fallback(&dir_id, &name, file_type != ResticFileType::Data)
        .await?
    {
        // Best-effort: clear in-memory hint cache for this file.

        state.client.delete_file(&dir_id, &file.file_id).await?;
    }

    Ok(StatusCode::OK)
}
