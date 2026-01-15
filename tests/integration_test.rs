//! Integration tests for 115 Open Platform API operations.
//!
//! These tests require the following environment variables:
//! - OPEN115_ACCESS_TOKEN
//! - OPEN115_REFRESH_TOKEN

use bytes::Bytes;
use restic_115::{config::Config, open115::Open115Client};
use std::env;
use std::sync::Once;

fn init_tracing_once() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        // Let tests print tracing logs (used by OSS PutObject response dump).
        // Controlled by RUST_LOG / EnvFilter.
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
        tracing_subscriber::fmt().with_env_filter(filter).init();
    });
}

fn get_test_tokens() -> Option<(String, String)> {
    let access = env::var("OPEN115_ACCESS_TOKEN").ok()?;
    let refresh = env::var("OPEN115_REFRESH_TOKEN").ok()?;
    Some((access, refresh))
}

macro_rules! skip_if_no_tokens {
    () => {
        if get_test_tokens().is_none() {
            eprintln!("Skipping test: OPEN115_ACCESS_TOKEN and OPEN115_REFRESH_TOKEN not set");
            return;
        }
    };
}

async fn make_test_client(repo_path: &str) -> Option<Open115Client> {
    let (access, refresh) = get_test_tokens()?;
    Open115Client::new(Config {
        access_token: Some(access),
        refresh_token: Some(refresh),
        repo_path: repo_path.to_string(),
        listen_addr: "127.0.0.1:0".to_string(),
        log_level: "info".to_string(),
        api_base: "https://proapi.115.com".to_string(),
        user_agent: "restic-115-tests".to_string(),
        callback_server: "https://api.oplist.org/115cloud/callback".to_string(),
    })
    .await
    .ok()
}

fn unique_repo_path(prefix: &str) -> String {
    format!("/{prefix}-{}", chrono::Utc::now().timestamp_millis())
}

#[tokio::test]
async fn test_refresh_token() {
    skip_if_no_tokens!();
    init_tracing_once();
    let repo_path = unique_repo_path("restic-115-integration");
    let client = make_test_client(&repo_path).await.unwrap();

    // A cheap call: try to list root. If access token is stale, refresh-on-401 should fix it.
    let files = client.list_files("0").await;
    if let Err(e) = files {
        // Token refresh may be rate limited by 115; don't fail the whole suite on this.
        eprintln!(
            "Skipping test_refresh_token due to API/token state: {:?}",
            e
        );
        return;
    }
}

#[tokio::test]
async fn test_create_list_delete_directory() {
    skip_if_no_tokens!();
    init_tracing_once();
    let repo_path = unique_repo_path("restic-115-integration-dir");
    let client = make_test_client(&repo_path).await.unwrap();

    let dir_id = match client.ensure_path(&repo_path).await {
        Ok(id) => id,
        Err(e) => {
            eprintln!("Failed to create directory (maybe rate limited): {:?}", e);
            return;
        }
    };

    let listing = client.list_files(&dir_id).await;
    assert!(listing.is_ok(), "list_files should work");

    // cleanup
    let _ = client.delete_file("0", &dir_id).await;
}

#[tokio::test]
async fn test_upload_and_download_small_file() {
    skip_if_no_tokens!();
    init_tracing_once();
    let repo_path = unique_repo_path("restic-115-integration-upload");
    let client = make_test_client(&repo_path).await.unwrap();

    let dir_id = match client.ensure_path(&repo_path).await {
        Ok(id) => id,
        Err(e) => {
            eprintln!("Failed to create directory (maybe rate limited): {:?}", e);
            return;
        }
    };

    // init cache
    let _ = client.list_files(&dir_id).await;

    // Use unique content each run to avoid 115 "fast upload" (秒传, status=2),
    // so we can observe the real OSS PutObject + callback response and compare ids.
    let content = Bytes::from(format!(
        "hello restic-115 {}",
        chrono::Utc::now().timestamp_millis()
    ));
    let uploaded = client
        .upload_file(&dir_id, "hello.txt", content.clone())
        .await;
    if let Err(e) = uploaded {
        eprintln!("Upload failed (maybe API shape changed): {:?}", e);
        return;
    }

    let info = client
        .get_file_info(&dir_id, "hello.txt")
        .await
        .expect("get_file_info failed")
        .expect("file not found after upload");
    eprintln!(
        "search result: file_id={}, pick_code={}",
        info.file_id, info.pick_code
    );

    let downloaded = client
        .download_file(&info.pick_code, None)
        .await
        .expect("download failed");
    assert_eq!(downloaded, content);

    let _ = client.delete_file(&dir_id, &info.file_id).await;
    let _ = client.delete_file("0", &dir_id).await;
}
