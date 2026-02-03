use restic_115::{config::Config, open115::Open115Client};
use std::env;

async fn get_test_config(repo_path: &str) -> Option<Config> {
    let access = env::var("OPEN115_ACCESS_TOKEN").ok()?;
    let refresh = env::var("OPEN115_REFRESH_TOKEN").ok()?;
    Some(Config {
        access_token: Some(access),
        refresh_token: Some(refresh),
        repo_path: repo_path.to_string(),
        listen_addr: "127.0.0.1".to_string(),
        listen_port: 0,
        log_level: "info".to_string(),
        api_base: "https://proapi.115.com".to_string(),
        user_agent: "restic-115-tests".to_string(),
        callback_server: "https://api.oplist.org/115cloud/callback".to_string(),
        db_path: "test-persistence.db".to_string(),
        force_cache_rebuild: false,
    })
}

#[tokio::test]
async fn test_persistence() {
    let access = env::var("OPEN115_ACCESS_TOKEN");
    let refresh = env::var("OPEN115_REFRESH_TOKEN");
    if access.is_err() || refresh.is_err() {
        eprintln!("Skipping test_persistence: OPEN115 tokens not set");
        return;
    }

    let repo_path = format!(
        "/integration-test-persistence-{}",
        chrono::Utc::now().timestamp_millis()
    );
    let config = get_test_config(&repo_path).await.unwrap();

    // 0. Ensure fresh DB for test (or use a separate test DB).
    // The current implementation uses restic-115-cache.db in CWD.
    // For testing, we might want to use a temp file.
    // But since Open115Client::new has a hardcoded string right now, I'll stick to it for a simple check.
    // Or I should have made the DB path configurable.
    // Let's assume we can run it.

    // 1. First run: Initialize with tokens
    {
        let client = Open115Client::new(config.clone())
            .await
            .expect("Failed to create client");
        // warm_cache will fetch and save to DB
        client
            .ensure_path(&repo_path, false)
            .await
            .expect("Failed to ensure path");
        client.warm_cache(false).await.expect("Failed to warm cache");
    }

    // 2. Second run: Use empty tokens in config, should load from DB
    {
        let mut config_no_tokens = config.clone();
        config_no_tokens.access_token = None;
        config_no_tokens.refresh_token = None;

        let client = Open115Client::new(config_no_tokens)
            .await
            .expect("Failed to create client with no tokens");

        // Verify tokens are available (implicitly by calling an API)
        let root_files = client.list_files("0").await.expect("Failed to list files");
        assert!(
            !root_files.is_empty(),
            "Should have some files from DB cache or real API"
        );

        // Verify directory cache from DB
        let path_id = client
            .find_path_id(&repo_path)
            .await
            .expect("Failed to find path id");
        assert!(path_id.is_some(), "Path ID should be found in DB");
    }
}

#[tokio::test]
async fn test_upload_does_not_wipe_siblings() {
    let access = env::var("OPEN115_ACCESS_TOKEN");
    let refresh = env::var("OPEN115_REFRESH_TOKEN");
    if access.is_err() || refresh.is_err() {
        eprintln!("Skipping test: OPEN115 tokens not set");
        return;
    }

    let repo_path = format!(
        "/integration-test-siblings-{}",
        chrono::Utc::now().timestamp_millis()
    );
    let config = get_test_config(&repo_path).await.unwrap();
    let client = Open115Client::new(config.clone()).await.unwrap();

    // 1. Create a directory and add two files to it
    let dir_id = client.ensure_path(&repo_path, false).await.unwrap();

    // Upload file1
    client
        .upload_file(&dir_id, "file1.txt", bytes::Bytes::from("content1"))
        .await
        .unwrap();

    // Upload file2
    client
        .upload_file(&dir_id, "file2.txt", bytes::Bytes::from("content2"))
        .await
        .unwrap();

    // Verify we have both files in DB listing
    let files = client.list_files(&dir_id).await.unwrap();
    assert!(files.iter().any(|f| f.filename == "file1.txt"));
    assert!(files.iter().any(|f| f.filename == "file2.txt"));

    // 2. Upload a THIRD file
    client
        .upload_file(&dir_id, "file3.txt", bytes::Bytes::from("content3"))
        .await
        .unwrap();

    // 3. Verify that file1 and file2 are still in the cache!
    let files_after = client.list_files(&dir_id).await.unwrap();
    assert!(
        files_after.iter().any(|f| f.filename == "file1.txt"),
        "file1.txt was wiped from cache!"
    );
    assert!(
        files_after.iter().any(|f| f.filename == "file2.txt"),
        "file2.txt was wiped from cache!"
    );
    assert!(
        files_after.iter().any(|f| f.filename == "file3.txt"),
        "file3.txt not found!"
    );

    // Cleanup
    let _ = client.delete_file("0", &dir_id).await;
}
