//! Types for 115 Open Platform API responses.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

fn deserialize_state<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = Option::<Value>::deserialize(deserializer)?;
    Ok(match v {
        None | Some(Value::Null) => None,
        Some(Value::Bool(b)) => Some(b),
        Some(Value::Number(n)) => Some(n.as_i64().unwrap_or(0) != 0),
        Some(Value::String(s)) => match s.as_str() {
            "0" | "false" | "False" | "FALSE" => Some(false),
            "1" | "true" | "True" | "TRUE" => Some(true),
            _ => None,
        },
        _ => None,
    })
}

#[derive(Debug, Deserialize)]
pub struct RefreshTokenResponse {
    #[serde(default, deserialize_with = "deserialize_state")]
    pub state: Option<bool>,
    pub code: Option<i64>,
    pub message: Option<String>,
    pub data: Option<RefreshTokenData>,
    pub errno: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RefreshTokenData {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_in: Option<i64>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct FileListResponse {
    #[serde(default)]
    pub data: Vec<FileEntry>,
    pub count: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_state")]
    pub state: Option<bool>,
    pub code: Option<i64>,
    pub message: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct FileEntry {
    pub fid: String,
    #[serde(default)]
    pub pid: String,
    pub fc: String,
    #[serde(rename = "fn")]
    pub name: String,
    #[serde(default)]
    pub fs: i64,
    #[serde(default)]
    pub pc: String,
    #[serde(default)]
    pub sha1: String,
}

impl FileEntry {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn is_dir(&self) -> bool {
        self.fc == "0"
    }
}

/// Generic 115 API boolean response wrapper.
#[derive(Debug, Deserialize)]
pub struct BoolResponse<T> {
    #[serde(default, deserialize_with = "deserialize_state")]
    pub state: Option<bool>,
    pub code: Option<i64>,
    pub message: Option<String>,
    pub data: Option<T>,
    pub errno: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MkdirData {
    pub file_name: Option<String>,
    pub file_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UploadTokenResponse {
    #[serde(default, deserialize_with = "deserialize_state")]
    pub state: Option<bool>,
    pub code: Option<i64>,
    pub message: Option<String>,
    pub data: Option<Value>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct UploadToken {
    pub endpoint: Option<String>,
    #[serde(rename = "AccessKeyId")]
    pub access_key_id: Option<String>,
    #[serde(rename = "AccessKeySecret")]
    pub access_key_secret: Option<String>,
    #[serde(rename = "AccessKeySecrett")]
    pub access_key_secret_typo: Option<String>,
    #[serde(rename = "SecurityToken")]
    pub security_token: Option<String>,
    #[serde(rename = "Expiration")]
    pub expiration: Option<String>,
}

impl UploadToken {
    pub fn access_key_secret(&self) -> Option<&str> {
        self.access_key_secret
            .as_deref()
            .or(self.access_key_secret_typo.as_deref())
    }
}

/// Upload init is not stable across docs/SDKs; keep it as JSON value.
#[derive(Debug, Deserialize)]
pub struct UploadInitResponse {
    #[serde(default, deserialize_with = "deserialize_state")]
    pub state: Option<bool>,
    pub code: Option<i64>,
    pub message: Option<String>,
    pub data: Option<serde_json::Value>,
}

/// Downurl response is keyed by file id in `data`.
#[derive(Debug, Deserialize)]
pub struct DownUrlResponse {
    #[serde(default, deserialize_with = "deserialize_state")]
    pub state: Option<bool>,
    pub code: Option<i64>,
    pub message: Option<String>,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct EmptyJson {}

// ============================================================================
// OSS callback result (returned by OSS PutObject/CompleteMultipartUpload with callback headers)
// ============================================================================

#[derive(Debug, Deserialize, Clone)]
pub struct OssCallbackResult {
    #[serde(default, deserialize_with = "deserialize_state")]
    pub state: Option<bool>,
    pub code: Option<i64>,
    pub message: Option<String>,
    pub data: Option<OssCallbackData>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct OssCallbackData {
    #[serde(rename = "pick_code", default)]
    pub pick_code: String,
    #[serde(rename = "file_name", default)]
    pub file_name: String,
    #[serde(rename = "file_size", default)]
    pub file_size: i64,
    #[serde(rename = "file_id", default)]
    pub file_id: String,
    #[serde(default)]
    pub sha1: String,
    #[serde(default)]
    pub cid: String,
}

// ============================================================================
// Search
// ============================================================================

/// Response for /open/ufile/search
#[derive(Debug, Deserialize, Clone)]
pub struct SearchResponse {
    #[serde(default)]
    pub data: Vec<SearchEntry>,
    pub count: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_state")]
    pub state: Option<bool>,
    pub code: Option<i64>,
    pub message: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SearchEntry {
    #[serde(rename = "file_id")]
    pub file_id: String,
    #[serde(rename = "file_name")]
    pub file_name: String,
    #[serde(rename = "file_size", default)]
    pub file_size: String,
    #[serde(rename = "parent_id", default)]
    pub parent_id: String,
    #[serde(rename = "pick_code", default)]
    pub pick_code: String,
    #[serde(default)]
    pub sha1: String,
    /// "1" file, "0" folder (per docs)
    #[serde(rename = "file_category", default)]
    pub file_category: String,
}
