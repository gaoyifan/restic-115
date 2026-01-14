//! 115 Open Platform API client for file operations.

use base64::Engine;
use bytes::Bytes;
use chrono::Utc;
use hmac::{Hmac, Mac};
use parking_lot::RwLock;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::multipart::Form;
use serde_json::Value;
use sha1::Digest;
use std::collections::{HashMap, HashSet};
use std::env;
use std::sync::Arc;
use std::time::Duration;

use super::auth::TokenManager;
use super::types::*;
use super::ResticFileType;
use crate::config::Config;
use crate::error::{AppError, Result};

type HmacSha1 = Hmac<sha1::Sha1>;

const MAX_RATE_LIMIT_RETRIES: usize = 6;
const MAX_OSS_PUT_RESPONSE_LOG_BYTES: usize = 512 * 1024; // 512KiB, callback JSON should be tiny.

fn truthy_env(key: &str) -> bool {
    match env::var(key) {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            matches!(v.as_str(), "1" | "true" | "yes" | "y" | "on")
        }
        Err(_) => false,
    }
}

fn should_dump_oss_putobject_response() -> bool {
    // Force printing the full PutObject response even if RUST_LOG isn't debug.
    // Useful for debugging OSS callback responses that affect read-after-write semantics.
    truthy_env("OPEN115_DUMP_OSS_PUTOBJECT_RESPONSE")
}

fn is_access_token_invalid(code: i64) -> bool {
    // See docs/115/接入指南/授权错误码.md
    // 40140125: access_token 无效（已过期或者已解除授权） -> refresh via /open/refreshToken
    matches!(code, 40140123 | 40140124 | 40140125 | 40140126)
}

fn is_quota_limited(code: i64) -> bool {
    // Observed from 115: code=406, message="已达到当前访问上限..."
    code == 406
}

fn is_rate_limited(code: i64) -> bool {
    // Rate limit / quota / frequency control class errors.
    // See docs/115/接入指南/授权错误码.md:
    // - 40140117: access_token refresh too frequently
    is_quota_limited(code) || code == 40140117
}

async fn backoff_sleep(attempt: usize) {
    // Exponential backoff with a cap.
    // attempt starts at 1.
    // Keep the cap small so a single request can't block for minutes (tests enforce a 5min timeout).
    let secs = (1u64 << (attempt - 1)).min(16);
    tokio::time::sleep(Duration::from_secs(secs)).await;
}

#[derive(Debug, Clone)]
pub struct FileInfo {
    pub file_id: String,
    pub filename: String,
    pub is_dir: bool,
    pub size: i64,
    pub parent_id: String,
    pub pick_code: String,
    pub sha1: String,
}

#[derive(Clone)]
pub struct Open115Client {
    token_manager: TokenManager,
    api_base: String,
    repo_path: String,
    user_agent: String,

    /// Cache of directory IDs: path -> cid
    dir_cache: Arc<RwLock<HashMap<String, String>>>,
    /// Cache of directory listing: cid -> entries
    files_cache: Arc<RwLock<HashMap<String, Vec<FileInfo>>>>,
    /// Mark cached directories as dirty (contents changed) without forcing an immediate re-list.
    /// Only the REST list endpoints should trigger refresh.
    dirty_dirs: Arc<RwLock<HashSet<String>>>,
    /// Per-file hint cache populated from OSS callback results.
    /// This avoids listing/search-index delays for read-after-write within the same process.
    file_hint_cache: Arc<RwLock<HashMap<String, FileInfo>>>,
}

impl Open115Client {
    pub fn new(cfg: Config) -> Self {
        Self {
            token_manager: TokenManager::new(cfg.access_token.clone(), cfg.refresh_token.clone()),
            api_base: cfg.api_base.trim_end_matches('/').to_string(),
            repo_path: cfg.repo_path,
            user_agent: cfg.user_agent,
            dir_cache: Arc::new(RwLock::new(HashMap::new())),
            files_cache: Arc::new(RwLock::new(HashMap::new())),
            dirty_dirs: Arc::new(RwLock::new(HashSet::new())),
            file_hint_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn file_hint_key(cid: &str, filename: &str) -> String {
        format!("{}:{}", cid, filename)
    }

    fn get_file_hint(&self, cid: &str, filename: &str) -> Option<FileInfo> {
        let k = Self::file_hint_key(cid, filename);
        self.file_hint_cache.read().get(&k).cloned()
    }

    fn upsert_file_hint(&self, cid: &str, info: &FileInfo) {
        let k = Self::file_hint_key(cid, &info.filename);
        self.file_hint_cache.write().insert(k, info.clone());
    }

    pub fn remove_file_hint(&self, cid: &str, filename: &str) {
        let k = Self::file_hint_key(cid, filename);
        self.file_hint_cache.write().remove(&k);
    }

    fn require_tokens(&self) -> Result<()> {
        if self.token_manager.access_token_value().is_some()
            && self.token_manager.refresh_token_value().is_some()
        {
            return Ok(());
        }
        Err(AppError::Auth(
            "Missing 115 tokens. Obtain tokens via OpenList token tool, then set OPEN115_ACCESS_TOKEN and OPEN115_REFRESH_TOKEN. Callback server: https://api.oplist.org/115cloud/callback".to_string(),
        ))
    }

    fn auth_headers(&self, access_token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {}", access_token)).unwrap(),
        );
        headers.insert(
            "User-Agent",
            HeaderValue::from_str(&self.user_agent).unwrap(),
        );
        headers
    }

    /// Perform an authenticated GET with auto-refresh-on-401.
    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        query: &[(&str, String)],
    ) -> Result<T> {
        self.require_tokens()?;

        async fn send(
            this: &Open115Client,
            token: &str,
            url: &str,
            query: &[(&str, String)],
        ) -> Result<(reqwest::StatusCode, Bytes)> {
            let resp = this
                .token_manager
                .http_client()
                .get(url)
                .headers(this.auth_headers(token))
                .query(query)
                .send()
                .await?;
            let status = resp.status();
            let bytes = resp.bytes().await?;
            Ok((status, bytes))
        }

        // Retry loop for 115 quota / rate limits.
        for attempt in 1..=MAX_RATE_LIMIT_RETRIES {
            let token = self.token_manager.get_token().await?;
            let (status, bytes) = send(self, &token, url, query).await?;

            // HTTP-level 401: refresh and retry.
            if status.as_u16() == 401 {
                let token = self.token_manager.refresh_token().await?;
                let (_status2, bytes2) = send(self, &token, url, query).await?;
                return Ok(serde_json::from_slice::<T>(&bytes2)?);
            }

            // HTTP-level 429: backoff and retry.
            if status.as_u16() == 429 && attempt < MAX_RATE_LIMIT_RETRIES {
                tracing::warn!(
                    "HTTP 429 on GET {}, backing off attempt {}/{}",
                    url,
                    attempt,
                    MAX_RATE_LIMIT_RETRIES
                );
                backoff_sleep(attempt).await;
                continue;
            }

            // App-level token invalid / quota limit are encoded in JSON.
            if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
                if let Some(code) = v.get("code").and_then(|c| c.as_i64()) {
                    if is_access_token_invalid(code) {
                        let token = self.token_manager.refresh_token().await?;
                        let (_status2, bytes2) = send(self, &token, url, query).await?;
                        return Ok(serde_json::from_slice::<T>(&bytes2)?);
                    }
                    if is_rate_limited(code) && attempt < MAX_RATE_LIMIT_RETRIES {
                        tracing::warn!(
                            "115 rate limited (code={}) on GET {}, backing off attempt {}/{}",
                            code,
                            url,
                            attempt,
                            MAX_RATE_LIMIT_RETRIES
                        );
                        backoff_sleep(attempt).await;
                        continue;
                    }
                }
                return Ok(serde_json::from_value::<T>(v)?);
            }

            return Ok(serde_json::from_slice::<T>(&bytes)?);
        }

        unreachable!("loop either returns or continues")
    }

    /// Perform an authenticated POST (form) with auto-refresh-on-401.
    async fn post_form_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        form_builder: impl Fn() -> Form,
    ) -> Result<T> {
        self.require_tokens()?;

        async fn send(
            this: &Open115Client,
            token: &str,
            url: &str,
            form_builder: &impl Fn() -> Form,
        ) -> Result<(reqwest::StatusCode, Bytes)> {
            let resp = this
                .token_manager
                .http_client()
                .post(url)
                .headers(this.auth_headers(token))
                .multipart(form_builder())
                .send()
                .await?;
            let status = resp.status();
            let bytes = resp.bytes().await?;
            Ok((status, bytes))
        }

        for attempt in 1..=MAX_RATE_LIMIT_RETRIES {
            let token = self.token_manager.get_token().await?;
            let (status, bytes) = send(self, &token, url, &form_builder).await?;

            if status.as_u16() == 401 {
                let token = self.token_manager.refresh_token().await?;
                let (_status2, bytes2) = send(self, &token, url, &form_builder).await?;
                return Ok(serde_json::from_slice::<T>(&bytes2)?);
            }

            // HTTP-level 429: backoff and retry.
            if status.as_u16() == 429 && attempt < MAX_RATE_LIMIT_RETRIES {
                tracing::warn!(
                    "HTTP 429 on POST {}, backing off attempt {}/{}",
                    url,
                    attempt,
                    MAX_RATE_LIMIT_RETRIES
                );
                backoff_sleep(attempt).await;
                continue;
            }

            if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
                if let Some(code) = v.get("code").and_then(|c| c.as_i64()) {
                    if is_access_token_invalid(code) {
                        let token = self.token_manager.refresh_token().await?;
                        let (_status2, bytes2) = send(self, &token, url, &form_builder).await?;
                        return Ok(serde_json::from_slice::<T>(&bytes2)?);
                    }
                    if is_rate_limited(code) && attempt < MAX_RATE_LIMIT_RETRIES {
                        tracing::warn!(
                            "115 rate limited (code={}) on POST {}, backing off attempt {}/{}",
                            code,
                            url,
                            attempt,
                            MAX_RATE_LIMIT_RETRIES
                        );
                        backoff_sleep(attempt).await;
                        continue;
                    }
                }
                return Ok(serde_json::from_value::<T>(v)?);
            }

            return Ok(serde_json::from_slice::<T>(&bytes)?);
        }

        unreachable!("loop either returns or continues")
    }

    // =========================================================================
    // Directory operations
    // =========================================================================

    async fn search_in_dir(
        &self,
        cid: &str,
        search_value: &str,
        fc: Option<i64>,
    ) -> Result<SearchResponse> {
        let url = format!("{}/open/ufile/search", self.api_base);
        let mut query = vec![
            ("search_value", search_value.to_string()),
            ("limit", "100".to_string()),
            ("offset", "0".to_string()),
            ("cid", cid.to_string()),
        ];
        if let Some(fc) = fc {
            query.push(("fc", fc.to_string()));
        }
        self.get_json(&url, &query).await
    }

    /// Find a file/dir by exact name under a directory, without listing the whole directory.
    ///
    /// This uses `/open/ufile/search` to avoid paging through huge directories (like root),
    /// which can quickly exhaust 115 API quota and cause `code=406` errors.
    async fn find_file_fast(&self, cid: &str, name: &str) -> Result<Option<FileInfo>> {
        // Search both files and folders (fc not set), then filter exact match.
        let resp = self.search_in_dir(cid, name, None).await?;
        if resp.state == Some(false) || resp.code.unwrap_or(0) != 0 {
            return Err(AppError::Open115Api {
                code: resp.code.unwrap_or(-1),
                message: resp.message.unwrap_or_default(),
            });
        }

        let entry = resp
            .data
            .into_iter()
            .find(|e| e.file_name == name && (e.parent_id.is_empty() || e.parent_id == cid));

        let Some(e) = entry else {
            return Ok(None);
        };

        let is_dir = e.file_category == "0";
        let size = e.file_size.parse::<i64>().unwrap_or(0);
        Ok(Some(FileInfo {
            file_id: e.file_id,
            filename: e.file_name,
            is_dir,
            size,
            parent_id: if e.parent_id.is_empty() {
                cid.to_string()
            } else {
                e.parent_id
            },
            pick_code: e.pick_code,
            sha1: e.sha1,
        }))
    }

    pub async fn list_files(&self, cid: &str) -> Result<Vec<FileInfo>> {
        // Only REST list endpoints should call this, because it may trigger full directory pagination.
        // Non-list operations should prefer `find_file_fast()` (search API) to avoid expensive listing.

        // cache hit (only when not dirty)
        {
            let dirty = self.dirty_dirs.read();
            if !dirty.contains(cid) {
                let cache = self.files_cache.read();
                if let Some(v) = cache.get(cid) {
                    return Ok(v.clone());
                }
            }
        }

        let mut all = Vec::new();
        let mut offset = 0i64;
        let limit = 1150i64;
        let url = format!("{}/open/ufile/files", self.api_base);

        loop {
            let resp: FileListResponse = self
                .get_json(
                    &url,
                    &[
                        ("cid", cid.to_string()),
                        ("limit", limit.to_string()),
                        ("offset", offset.to_string()),
                        ("show_dir", "1".to_string()),
                        ("stdir", "1".to_string()),
                    ],
                )
                .await?;

            if resp.state == Some(false) || resp.code.unwrap_or(0) != 0 {
                return Err(AppError::Open115Api {
                    code: resp.code.unwrap_or(-1),
                    message: resp.message.unwrap_or_default(),
                });
            }

            let count = resp.count.unwrap_or(resp.data.len() as i64);
            for e in resp.data {
                all.push(FileInfo {
                    file_id: e.fid.clone(),
                    filename: e.name().to_string(),
                    is_dir: e.is_dir(),
                    size: e.fs,
                    parent_id: e.pid.clone(),
                    pick_code: e.pc.clone(),
                    sha1: e.sha1.clone(),
                });
            }

            offset += limit;
            if offset >= count {
                break;
            }
        }

        {
            let mut cache = self.files_cache.write();
            cache.insert(cid.to_string(), all.clone());
        }
        {
            let mut dirty = self.dirty_dirs.write();
            dirty.remove(cid);
        }

        Ok(all)
    }

    pub fn mark_dir_dirty(&self, cid: &str) {
        let mut dirty = self.dirty_dirs.write();
        dirty.insert(cid.to_string());
    }

    pub async fn find_file(&self, cid: &str, name: &str) -> Result<Option<FileInfo>> {
        // Always prefer the search API. Do NOT fall back to directory listing here, to keep
        // non-list operations from triggering expensive pagination.
        self.find_file_fast(cid, name).await
    }

    pub async fn create_directory(&self, pid: &str, name: &str) -> Result<String> {
        let url = format!("{}/open/folder/add", self.api_base);
        let pid_s = pid.to_string();
        let name_s = name.to_string();
        let resp: BoolResponse<MkdirData> = self
            .post_form_json(&url, move || {
                Form::new()
                    .text("pid", pid_s.clone())
                    .text("file_name", name_s.clone())
            })
            .await?;
        let ok = resp.state.unwrap_or(false);
        let code = resp.code.unwrap_or(-1);
        if !ok || code != 0 {
            // might already exist
            self.mark_dir_dirty(pid);
            if let Some(existing) = self.find_file_fast(pid, name).await? {
                if existing.is_dir {
                    return Ok(existing.file_id);
                }
            }
            return Err(AppError::Open115Api {
                code,
                message: resp.message.unwrap_or_default(),
            });
        }

        let id = resp
            .data
            .and_then(|d| d.file_id)
            .ok_or_else(|| AppError::Internal("mkdir succeeded but no file_id".to_string()))?;

        // update caches
        {
            let mut cache = self.files_cache.write();
            cache.entry(pid.to_string()).or_default().push(FileInfo {
                file_id: id.clone(),
                filename: name.to_string(),
                is_dir: true,
                size: 0,
                parent_id: pid.to_string(),
                pick_code: String::new(),
                sha1: String::new(),
            });
            cache.insert(id.clone(), Vec::new());
        }

        Ok(id)
    }

    pub async fn find_path_id(&self, path: &str) -> Result<Option<String>> {
        let path = path.trim_end_matches('/');
        if path.is_empty() || path == "/" {
            return Ok(Some("0".to_string()));
        }
        {
            let cache = self.dir_cache.read();
            if let Some(id) = cache.get(path) {
                return Ok(Some(id.clone()));
            }
        }

        let parts: Vec<&str> = path
            .trim_start_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();

        let mut current_id = "0".to_string();
        let mut current_path = String::new();

        for part in parts {
            current_path.push('/');
            current_path.push_str(part);

            if let Some(cached) = self.dir_cache.read().get(&current_path).cloned() {
                current_id = cached;
                continue;
            }

            let found = self.find_file_fast(&current_id, part).await?;
            let Some(info) = found else {
                return Ok(None);
            };
            if !info.is_dir {
                return Ok(None);
            }
            current_id = info.file_id.clone();
            self.dir_cache
                .write()
                .insert(current_path.clone(), current_id.clone());
        }

        Ok(Some(current_id))
    }

    pub async fn ensure_path(&self, path: &str) -> Result<String> {
        let parts: Vec<&str> = path
            .trim_start_matches('/')
            .trim_end_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();

        let mut current_id = "0".to_string();
        let mut current_path = String::new();

        for part in parts {
            current_path.push('/');
            current_path.push_str(part);

            if let Some(id) = self.dir_cache.read().get(&current_path).cloned() {
                current_id = id;
                continue;
            }

            // Create first; for brand-new repos this avoids an extra search/list call per component.
            // If it already exists, create_directory() will resolve the existing id via a cheap search.
            let new_id = self.create_directory(&current_id, part).await?;
            current_id = new_id.clone();
            self.dir_cache
                .write()
                .insert(current_path.clone(), current_id.clone());
        }

        Ok(current_id)
    }

    fn data_subdir_prefix(filename: &str) -> &str {
        &filename[..2.min(filename.len())]
    }

    pub async fn get_data_file_dir_id(&self, filename: &str) -> Result<String> {
        let prefix = Self::data_subdir_prefix(filename);
        let path = format!("{}/data/{}", self.repo_path, prefix);
        self.ensure_path(&path).await
    }

    pub async fn find_data_file_dir_id(&self, filename: &str) -> Result<Option<String>> {
        let prefix = Self::data_subdir_prefix(filename);
        let path = format!("{}/data/{}", self.repo_path, prefix);
        self.find_path_id(&path).await
    }

    pub async fn get_type_dir_id(&self, file_type: ResticFileType) -> Result<String> {
        if file_type.is_config() {
            self.ensure_path(&self.repo_path).await
        } else {
            self.ensure_path(&format!("{}/{}", self.repo_path, file_type.dirname()))
                .await
        }
    }

    pub async fn find_type_dir_id(&self, file_type: ResticFileType) -> Result<Option<String>> {
        if file_type.is_config() {
            self.find_path_id(&self.repo_path).await
        } else {
            self.find_path_id(&format!("{}/{}", self.repo_path, file_type.dirname()))
                .await
        }
    }

    // =========================================================================
    // File operations
    // =========================================================================

    pub async fn get_file_info(&self, cid: &str, filename: &str) -> Result<Option<FileInfo>> {
        self.find_file(cid, filename).await
    }

    /// Get file info with an optional directory-listing fallback.
    ///
    /// - Uses search API first (cheap).
    /// - Optionally falls back to listing the directory (use only for small dirs like repo root,
    ///   keys/index/snapshots/etc). **Do not** use for `data/{00..ff}` hash subdirs.
    pub async fn get_file_info_with_fallback(
        &self,
        cid: &str,
        filename: &str,
        allow_list_fallback: bool,
    ) -> Result<Option<FileInfo>> {
        // 1) hint cache (from OSS callback)
        if let Some(hint) = self.get_file_hint(cid, filename) {
            return Ok(Some(hint));
        }
        // 2) search API
        if let Some(found) = self.find_file_fast(cid, filename).await? {
            return Ok(Some(found));
        }
        if !allow_list_fallback {
            return Ok(None);
        }
        // 3) directory listing fallback (only for small dirs)
        let files = self.list_files(cid).await?;
        Ok(files.into_iter().find(|f| f.filename == filename))
    }

    pub async fn delete_file(&self, parent_id: &str, file_id: &str) -> Result<()> {
        let url = format!("{}/open/ufile/delete", self.api_base);
        let file_id_s = file_id.to_string();
        let parent_id_s = parent_id.to_string();
        let resp: BoolResponse<serde_json::Value> = self
            .post_form_json(&url, move || {
                Form::new()
                    .text("file_ids", file_id_s.clone())
                    .text("parent_id", parent_id_s.clone())
            })
            .await?;
        let ok = resp.state.unwrap_or(false);
        let code = resp.code.unwrap_or(0);
        if !ok || code != 0 {
            // Idempotent delete: treat as OK if already deleted/not found
            tracing::warn!(
                "Delete file failed (idempotent ok): code={}, message={}",
                code,
                resp.message.clone().unwrap_or_default()
            );
        }

        // update cache
        {
            let mut cache = self.files_cache.write();
            if let Some(files) = cache.get_mut(parent_id) {
                files.retain(|f| f.file_id != file_id);
            }
        }

        Ok(())
    }

    pub async fn get_download_url(&self, pick_code: &str) -> Result<String> {
        let url = format!("{}/open/ufile/downurl", self.api_base);
        let pick_code_s = pick_code.to_string();
        let resp: DownUrlResponse = self
            .post_form_json(&url, move || Form::new().text("pick_code", pick_code_s.clone()))
            .await?;
        if resp.state == Some(false) || resp.code.unwrap_or(0) != 0 {
            return Err(AppError::Open115Api {
                code: resp.code.unwrap_or(-1),
                message: resp.message.unwrap_or_default(),
            });
        }
        let data = resp
            .data
            .ok_or_else(|| AppError::Internal("downurl: missing data".to_string()))?;
        // data is a dict keyed by fid
        if let Some(obj) = data.as_object() {
            for (_k, v) in obj.iter() {
                if let Some(u) = v
                    .get("url")
                    .and_then(|x| x.get("url"))
                    .and_then(|x| x.as_str())
                {
                    return Ok(u.to_string());
                }
            }
        }
        Err(AppError::Internal("downurl: missing url".to_string()))
    }

    pub async fn download_file(
        &self,
        pick_code: &str,
        range: Option<(u64, u64)>,
    ) -> Result<Bytes> {
        let download_url = self.get_download_url(pick_code).await?;
        let mut req = self
            .token_manager
            .http_client()
            .get(&download_url)
            .header("User-Agent", &self.user_agent);
        if let Some((start, end)) = range {
            req = req.header("Range", format!("bytes={}-{}", start, end));
        }
        let resp = req.send().await?;
        if !resp.status().is_success() && resp.status().as_u16() != 206 {
            return Err(AppError::Internal(format!(
                "Download failed with status: {}",
                resp.status()
            )));
        }
        Ok(resp.bytes().await?)
    }

    fn sha1_hex_upper(data: &[u8]) -> String {
        hex::encode(sha1::Sha1::digest(data)).to_uppercase()
    }

    fn parse_sign_check(s: &str) -> Option<(usize, usize)> {
        let parts: Vec<&str> = s.split('-').collect();
        if parts.len() != 2 {
            return None;
        }
        let start: usize = parts[0].parse().ok()?;
        let end: usize = parts[1].parse().ok()?;
        Some((start, end))
    }

    async fn upload_init(
        &self,
        parent_id: &str,
        filename: &str,
        file_size: usize,
        fileid: &str,
        preid: &str,
        pick_code: Option<&str>,
        sign_key: Option<&str>,
        sign_val: Option<&str>,
    ) -> Result<serde_json::Value> {
        let url = format!("{}/open/upload/init", self.api_base);
        let filename = filename.to_string();
        let file_size = file_size.to_string();
        let target = format!("U_1_{}", parent_id);
        let fileid = fileid.to_string();
        let preid = preid.to_string();
        let pick_code = pick_code.map(|s| s.to_string());
        let sign_key = sign_key.map(|s| s.to_string());
        let sign_val = sign_val.map(|s| s.to_string());

        let resp: UploadInitResponse = self
            .post_form_json(&url, move || {
                let mut form = Form::new()
                    .text("file_name", filename.clone())
                    .text("file_size", file_size.clone())
                    .text("target", target.clone())
                    .text("fileid", fileid.clone())
                    .text("preid", preid.clone());

                if let Some(pc) = pick_code.as_ref() {
                    form = form.text("pick_code", pc.clone());
                }
                if let Some(sk) = sign_key.as_ref() {
                    form = form.text("sign_key", sk.clone());
                }
                if let Some(sv) = sign_val.as_ref() {
                    form = form.text("sign_val", sv.clone());
                }
                form
            })
            .await?;
        if resp.state == Some(false) || resp.code.unwrap_or(0) != 0 {
            return Err(AppError::Open115Api {
                code: resp.code.unwrap_or(-1),
                message: resp.message.unwrap_or_default(),
            });
        }
        resp.data
            .ok_or_else(|| AppError::Internal("upload init: missing data".to_string()))
    }

    async fn get_upload_token(&self) -> Result<UploadToken> {
        let url = format!("{}/open/upload/get_token", self.api_base);
        let resp: UploadTokenResponse = self.get_json(&url, &[]).await?;
        if resp.state == Some(false) || resp.code.unwrap_or(0) != 0 {
            return Err(AppError::Open115Api {
                code: resp.code.unwrap_or(-1),
                message: resp.message.unwrap_or_default(),
            });
        }
        let data = resp
            .data
            .ok_or_else(|| AppError::Internal("get_token: missing data".to_string()))?;

        // 115 docs vary; handle common shapes:
        // - data: [ { ..UploadToken.. }, ... ]
        // - data: { ..UploadToken.. }
        // - data: { "token": { ..UploadToken.. } } or { "<key>": { ..UploadToken.. } }
        if let Some(arr) = data.as_array() {
            let first = arr
                .first()
                .cloned()
                .ok_or_else(|| AppError::Internal("get_token: empty list".to_string()))?;
            return Ok(serde_json::from_value::<UploadToken>(first)?);
        }
        if data.is_object() {
            // If it already looks like an UploadToken object, deserialize directly.
            if data.get("AccessKeyId").is_some() || data.get("SecurityToken").is_some() {
                return Ok(serde_json::from_value::<UploadToken>(data)?);
            }
            // Otherwise, try common nesting keys or first value in map.
            if let Some(tok) = data.get("token").or_else(|| data.get("data")).cloned() {
                return Ok(serde_json::from_value::<UploadToken>(tok)?);
            }
            if let Some((_k, v)) = data.as_object().and_then(|m| m.iter().next()) {
                return Ok(serde_json::from_value::<UploadToken>(v.clone())?);
            }
        }

        Err(AppError::Internal(format!(
            "get_token: unexpected data shape: {}",
            data
        )))
    }

    fn extract_init_field<'a>(data: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
        for k in keys {
            if let Some(v) = data.get(*k).and_then(|x| x.as_str()) {
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
        None
    }

    fn extract_callback_pair(data: &serde_json::Value) -> Option<(String, String)> {
        // try callback.callback + callback_var
        let cb = data.get("callback")?;
        let candidates: Vec<&serde_json::Value> = if cb.is_array() {
            cb.as_array().unwrap().iter().collect()
        } else {
            vec![cb]
        };
        for c in candidates {
            // common shapes:
            // {callback:"...", callback_var:"..."}
            // {callback:{value:{callback:"...",callback_var:"..."}}}
            let direct_cb = c.get("callback").and_then(|x| x.as_str());
            let direct_var = c.get("callback_var").and_then(|x| x.as_str());
            if let (Some(a), Some(b)) = (direct_cb, direct_var) {
                return Some((a.to_string(), b.to_string()));
            }
            let nested = c.get("value").or_else(|| c.get("Value"));
            if let Some(n) = nested {
                let a = n
                    .get("callback")
                    .or_else(|| n.get("Callback"))
                    .and_then(|x| x.as_str());
                let b = n
                    .get("callback_var")
                    .or_else(|| n.get("callbackVar"))
                    .or_else(|| n.get("CallbackVar"))
                    .and_then(|x| x.as_str());
                if let (Some(a), Some(b)) = (a, b) {
                    return Some((a.to_string(), b.to_string()));
                }
            }
        }
        None
    }

    async fn oss_put_object(
        &self,
        endpoint: &str,
        access_key_id: &str,
        access_key_secret: &str,
        security_token: &str,
        bucket: &str,
        object: &str,
        callback: &str,
        callback_var: &str,
        body: Bytes,
    ) -> Result<Option<OssCallbackData>> {
        // Prefer virtual-hosted style URL:
        //   https://{bucket}.{endpoint_host}/{object}
        // Some OSS regions reject path-style addressing with:
        //   SecondLevelDomainForbidden: "must be addressed using OSS third level domain"
        //
        // Keep `canonicalized_resource` as `/{bucket}/{object}` for signing.
        let endpoint = endpoint.trim_end_matches('/');
        let endpoint_url = reqwest::Url::parse(endpoint).map_err(|e| {
            AppError::Internal(format!("Invalid OSS endpoint URL '{}': {}", endpoint, e))
        })?;
        let host = endpoint_url.host_str().ok_or_else(|| {
            AppError::Internal(format!("OSS endpoint missing host: {}", endpoint))
        })?;

        let object_path = object.trim_start_matches('/');
        let url = if host.starts_with(&format!("{bucket}.")) {
            format!("{}/{object_path}", endpoint)
        } else {
            // Insert bucket as third-level domain.
            // Preserve scheme and port if present.
            let scheme = endpoint_url.scheme();
            let port = endpoint_url.port();
            let host_with_bucket = format!("{bucket}.{host}");
            let authority = if let Some(p) = port {
                format!("{host_with_bucket}:{p}")
            } else {
                host_with_bucket
            };
            format!("{scheme}://{authority}/{object_path}")
        };

        let date = Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        let content_type = "application/octet-stream";

        let cb_b64 = base64::engine::general_purpose::STANDARD.encode(callback);
        let cb_var_b64 = base64::engine::general_purpose::STANDARD.encode(callback_var);

        // Canonicalized OSS headers
        let mut oss_headers = vec![
            ("x-oss-callback".to_string(), cb_b64.clone()),
            ("x-oss-callback-var".to_string(), cb_var_b64.clone()),
            ("x-oss-security-token".to_string(), security_token.to_string()),
        ];
        oss_headers.sort_by(|a, b| a.0.cmp(&b.0));
        let canonicalized_headers = oss_headers
            .iter()
            .map(|(k, v)| format!("{}:{}\n", k.to_lowercase(), v.trim()))
            .collect::<String>();

        let canonicalized_resource = format!("/{}/{}", bucket, object.trim_start_matches('/'));

        let string_to_sign = format!(
            "PUT\n\n{}\n{}\n{}{}",
            content_type, date, canonicalized_headers, canonicalized_resource
        );

        let mut mac = HmacSha1::new_from_slice(access_key_secret.as_bytes())
            .map_err(|e| AppError::Internal(format!("HMAC init failed: {}", e)))?;
        mac.update(string_to_sign.as_bytes());
        let signature = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
        let authorization = format!("OSS {}:{}", access_key_id, signature);

        let resp = self
            .token_manager
            .http_client()
            .put(&url)
            .header("Date", date)
            .header("Content-Type", content_type)
            .header("Authorization", authorization)
            .header("x-oss-security-token", security_token)
            .header("x-oss-callback", cb_b64)
            .header("x-oss-callback-var", cb_var_b64)
            .body(body)
            .send()
            .await?;

        let status = resp.status();
        let headers = resp.headers().clone();

        if !status.is_success() {
            let bytes = resp.bytes().await.unwrap_or_default();
            let body_text = String::from_utf8_lossy(&bytes).to_string();
            if should_dump_oss_putobject_response() {
                tracing::info!(
                    target: "open115::oss",
                    status = %status,
                    headers = ?headers,
                    body_len = bytes.len(),
                    body = %body_text,
                    "OSS PutObject error response (dump enabled)"
                );
            } else {
                tracing::debug!(
                    target: "open115::oss",
                    status = %status,
                    headers = ?headers,
                    body_len = bytes.len(),
                    body = %body_text,
                    "OSS PutObject error response"
                );
            }
            return Err(AppError::Internal(format!(
                "OSS put failed: status={}, body={}",
                status, body_text
            )));
        }
        // On success, OSS may return callback result JSON (which can include file_id/pick_code/cid).
        let bytes = resp.bytes().await.unwrap_or_default();
        if !bytes.is_empty() {
            let mut log_body = bytes.clone();
            let truncated = log_body.len() > MAX_OSS_PUT_RESPONSE_LOG_BYTES;
            if truncated {
                log_body.truncate(MAX_OSS_PUT_RESPONSE_LOG_BYTES);
            }

            // Prefer pretty JSON if possible; otherwise log as UTF-8 lossy.
            let body_to_log = match serde_json::from_slice::<serde_json::Value>(&log_body) {
                Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_else(|_| {
                    String::from_utf8_lossy(&log_body).to_string()
                }),
                Err(_) => String::from_utf8_lossy(&log_body).to_string(),
            };

            if should_dump_oss_putobject_response() {
                tracing::info!(
                    target: "open115::oss",
                    status = %status,
                    headers = ?headers,
                    body_len = bytes.len(),
                    truncated = truncated,
                    body = %body_to_log,
                    "OSS PutObject success response (dump enabled)"
                );
            } else {
                tracing::debug!(
                    target: "open115::oss",
                    status = %status,
                    headers = ?headers,
                    body_len = bytes.len(),
                    truncated = truncated,
                    body = %body_to_log,
                    "OSS PutObject success response"
                );
            }
        }
        if bytes.is_empty() {
            return Ok(None);
        }
        if let Ok(cb) = serde_json::from_slice::<OssCallbackResult>(&bytes) {
            let ok = cb.state.unwrap_or(false);
            let code = cb.code.unwrap_or(0);
            if ok && code == 0 {
                if let Some(d) = cb.data {
                    if !d.file_id.is_empty() && !d.pick_code.is_empty() {
                        return Ok(Some(d));
                    }
                }
            }
        }
        Ok(None)
    }

    pub async fn upload_file(
        &self,
        parent_id: &str,
        filename: &str,
        data: Bytes,
        wait_visible: bool,
    ) -> Result<()> {
        let file_size = data.len();
        let file_sha1 = Self::sha1_hex_upper(&data);
        let pre_len = 128 * 1024;
        let pre_sha1 = Self::sha1_hex_upper(&data[..file_size.min(pre_len)]);

        // init
        let mut init_data = self
            .upload_init(
                parent_id,
                filename,
                file_size,
                &file_sha1,
                &pre_sha1,
                None,
                None,
                None,
            )
            .await?;

        let status = init_data
            .get("status")
            .and_then(|x| x.as_i64())
            .unwrap_or(-1);

        if status == 2 {
            // Fast upload path: still mark dirty and optionally wait for visibility.
            self.mark_dir_dirty(parent_id);
            if wait_visible {
                // Legacy behavior (kept for compatibility): wait via search.
                // Prefer disabling this and relying on read-side fallback listing for non-data types.
                for attempt in 1..=8usize {
                    if let Some(found) = self.find_file_fast(parent_id, filename).await? {
                        if !found.is_dir {
                            return Ok(());
                        }
                    }
                    backoff_sleep(attempt).await;
                }
                return Err(AppError::Internal(
                    "upload succeeded but file not visible yet; retry later".to_string(),
                ));
            }
            return Ok(());
        }

        if matches!(status, 6 | 7 | 8) {
            let sign_check = Self::extract_init_field(&init_data, &["sign_check", "signCheck"]);
            let sign_key = Self::extract_init_field(&init_data, &["sign_key", "signKey"]);
            if let (Some(sc), Some(sk)) = (sign_check, sign_key) {
                if let Some((start, end)) = Self::parse_sign_check(sc) {
                    if file_size == 0 || start >= file_size {
                        return Err(AppError::Internal(format!(
                            "upload init returned invalid sign_check={} for file_size={}",
                            sc, file_size
                        )));
                    }
                    let end = end.min(file_size.saturating_sub(1));
                    if start > end {
                        return Err(AppError::Internal(format!(
                            "upload init returned invalid sign_check={} (start>end) for file_size={}",
                            sc, file_size
                        )));
                    }
                    let sign_val = Self::sha1_hex_upper(&data[start..=end]);
                    init_data = self
                        .upload_init(
                            parent_id,
                            filename,
                            file_size,
                            &file_sha1,
                            &pre_sha1,
                            None,
                            Some(sk),
                            Some(&sign_val),
                        )
                        .await?;
                }
            }
        }

        let status = init_data
            .get("status")
            .and_then(|x| x.as_i64())
            .unwrap_or(-1);
        if status == 2 {
            self.mark_dir_dirty(parent_id);
            if wait_visible {
                // Legacy behavior (kept for compatibility): wait via search.
                for attempt in 1..=8usize {
                    if let Some(found) = self.find_file_fast(parent_id, filename).await? {
                        if !found.is_dir {
                            return Ok(());
                        }
                    }
                    backoff_sleep(attempt).await;
                }
                return Err(AppError::Internal(
                    "upload succeeded but file not visible yet; retry later".to_string(),
                ));
            }
            return Ok(());
        }

        // need OSS upload
        let bucket = Self::extract_init_field(&init_data, &["bucket"])
            .ok_or_else(|| AppError::Internal("upload: missing bucket".to_string()))?
            .to_string();
        let object = Self::extract_init_field(&init_data, &["object"])
            .ok_or_else(|| AppError::Internal("upload: missing object".to_string()))?
            .to_string();

        let (callback, callback_var) = Self::extract_callback_pair(&init_data).ok_or_else(|| {
            AppError::Internal("upload: missing callback/callback_var".to_string())
        })?;

        let token = self.get_upload_token().await?;
        let endpoint = token
            .endpoint
            .clone()
            .ok_or_else(|| AppError::Internal("get_token: missing endpoint".to_string()))?;
        let access_key_id = token
            .access_key_id
            .clone()
            .ok_or_else(|| AppError::Internal("get_token: missing AccessKeyId".to_string()))?;
        let access_key_secret = token
            .access_key_secret()
            .map(|s| s.to_string())
            .ok_or_else(|| AppError::Internal("get_token: missing AccessKeySecret".to_string()))?;
        let security_token = token
            .security_token
            .clone()
            .ok_or_else(|| AppError::Internal("get_token: missing SecurityToken".to_string()))?;

        let endpoint = if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
            endpoint
        } else {
            format!("https://{}", endpoint)
        };

        let cb_opt = self
            .oss_put_object(
            &endpoint,
            &access_key_id,
            &access_key_secret,
            &security_token,
            &bucket,
            &object,
            &callback,
            &callback_var,
            data.clone(),
        )
        .await?;

        // After successful OSS upload, 115 may not immediately provide `file_id`.
        // - Always mark directory cache as dirty (so list endpoints refresh later).
        // - For non-data types (config/keys/index/...) restic expects read-your-writes semantics,
        //   so we optionally wait for visibility via the SEARCH API (no directory listing).
        // If we already have a cached listing for this dir, mark it dirty. Otherwise, avoid
        // forcing a future full re-list of large dirs (especially `data/{00..ff}` subdirs).
        let had_cached_listing = self.files_cache.read().contains_key(parent_id);
        if had_cached_listing {
            self.mark_dir_dirty(parent_id);
        }

        // If OSS callback returned file metadata, store as a per-file hint to provide
        // read-after-write semantics without listing/search-index delays.
        if let Some(cb) = cb_opt {
            let cid = if cb.cid.is_empty() {
                parent_id.to_string()
            } else {
                cb.cid.clone()
            };
            let filename = if cb.file_name.is_empty() {
                filename.to_string()
            } else {
                cb.file_name.clone()
            };
            let info = FileInfo {
                file_id: cb.file_id.clone(),
                filename,
                is_dir: false,
                size: cb.file_size,
                parent_id: cid.clone(),
                pick_code: cb.pick_code.clone(),
                sha1: cb.sha1.clone(),
            };
            self.upsert_file_hint(&cid, &info);
        }

        if wait_visible {
            // Legacy behavior (kept for compatibility): wait via search.
            for attempt in 1..=8usize {
                if let Some(found) = self.find_file_fast(parent_id, filename).await? {
                    if !found.is_dir {
                        return Ok(());
                    }
                }
                backoff_sleep(attempt).await;
            }
            return Err(AppError::Internal(
                "upload succeeded but file not visible yet; retry later".to_string(),
            ));
        }
        Ok(())
    }

    pub async fn init_repository(&self) -> Result<()> {
        self.ensure_path(&self.repo_path).await?;
        for t in [
            ResticFileType::Data,
            ResticFileType::Keys,
            ResticFileType::Locks,
            ResticFileType::Snapshots,
            ResticFileType::Index,
        ] {
            self.ensure_path(&format!("{}/{}", self.repo_path, t.dirname()))
                .await?;
        }
        Ok(())
    }

    pub async fn list_all_data_files(&self) -> Result<Vec<FileInfo>> {
        let data_path = format!("{}/data", self.repo_path);
        let Some(data_id) = self.find_path_id(&data_path).await? else {
            return Ok(Vec::new());
        };
        let subdirs = self.list_files(&data_id).await?;
        let mut all = Vec::new();
        for s in subdirs.into_iter().filter(|x| x.is_dir) {
            let files = self.list_files(&s.file_id).await?;
            all.extend(files.into_iter().filter(|x| !x.is_dir));
        }
        Ok(all)
    }
}

impl std::fmt::Debug for Open115Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Open115Client")
            .field("api_base", &self.api_base)
            .field("repo_path", &self.repo_path)
            .finish()
    }
}

