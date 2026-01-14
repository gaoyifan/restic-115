//! Restic REST API types.

use serde::Serialize;

/// Restic REST API v2 file entry.
#[derive(Debug, Serialize)]
pub struct FileEntryV2 {
    pub name: String,
    pub size: u64,
}

