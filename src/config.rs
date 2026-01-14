//! Configuration handling for the application.

use clap::Parser;

/// Restic REST API server backed by 115 open platform.
#[derive(Parser, Debug, Clone)]
#[command(name = "restic-115")]
#[command(about = "Restic REST API backend server using 115 cloud storage (Open Platform)")]
pub struct Config {
    /// 115 access token (Bearer token for proapi.115.com)
    #[arg(long, env = "OPEN115_ACCESS_TOKEN")]
    pub access_token: Option<String>,

    /// 115 refresh token (used to refresh access token via passportapi.115.com)
    #[arg(long, env = "OPEN115_REFRESH_TOKEN")]
    pub refresh_token: Option<String>,

    /// Root folder path on 115 for the repository
    #[arg(long, env = "OPEN115_REPO_PATH", default_value = "/restic-backup")]
    pub repo_path: String,

    /// Server listen address
    #[arg(long, env = "LISTEN_ADDR", default_value = "127.0.0.1:8000")]
    pub listen_addr: String,

    /// Log level (trace, debug, info, warn, error)
    #[arg(long, env = "RUST_LOG", default_value = "info")]
    pub log_level: String,

    /// 115 Open Platform API base URL for file operations
    #[arg(long, env = "OPEN115_API_BASE", default_value = "https://proapi.115.com")]
    pub api_base: String,

    /// User-Agent used when calling 115 APIs and downloading files
    #[arg(long, env = "OPEN115_USER_AGENT", default_value = "restic-115")]
    pub user_agent: String,

    /// Callback server used for obtaining initial tokens (documentation / hint only)
    #[arg(
        long,
        env = "OPEN115_CALLBACK_SERVER",
        default_value = "https://api.oplist.org/115cloud/callback"
    )]
    pub callback_server: String,
}

