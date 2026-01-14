//! 115 Open Platform client module.

mod auth;
mod client;
mod types;

pub use client::{FileInfo, Open115Client};

/// Restic backend file types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResticFileType {
    Data,
    Keys,
    Locks,
    Snapshots,
    Index,
    Config,
}

impl ResticFileType {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "data" => Some(Self::Data),
            "keys" => Some(Self::Keys),
            "locks" => Some(Self::Locks),
            "snapshots" => Some(Self::Snapshots),
            "index" => Some(Self::Index),
            "config" => Some(Self::Config),
            _ => None,
        }
    }

    pub fn dirname(&self) -> &'static str {
        match self {
            Self::Data => "data",
            Self::Keys => "keys",
            Self::Locks => "locks",
            Self::Snapshots => "snapshots",
            Self::Index => "index",
            Self::Config => "config",
        }
    }

    pub fn is_config(&self) -> bool {
        matches!(self, Self::Config)
    }
}

