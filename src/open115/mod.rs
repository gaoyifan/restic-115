//! 115 Open Platform client module.

mod auth;
mod client;
pub mod database;
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

impl std::str::FromStr for ResticFileType {
    type Err = ();

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "data" => Ok(Self::Data),
            "keys" => Ok(Self::Keys),
            "locks" => Ok(Self::Locks),
            "snapshots" => Ok(Self::Snapshots),
            "index" => Ok(Self::Index),
            "config" => Ok(Self::Config),
            _ => Err(()),
        }
    }
}

impl ResticFileType {
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
