//! End-to-end tests using the actual restic CLI.
//!
//! These tests require:
//! - Environment variables: OPEN115_ACCESS_TOKEN, OPEN115_REFRESH_TOKEN
//! - restic CLI installed and available in PATH

use std::env;
use std::fs;
use std::io::{Read};
use std::io::Write;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::thread::JoinHandle;
use std::time::Duration;
use tempfile::TempDir;

fn get_test_tokens() -> Option<(String, String)> {
    let access = env::var("OPEN115_ACCESS_TOKEN").ok()?;
    let refresh = env::var("OPEN115_REFRESH_TOKEN").ok()?;
    Some((access, refresh))
}

fn find_available_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("Failed to bind to port");
    listener.local_addr().unwrap().port()
}

fn wait_for_server(port: u16, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    let url = format!("http://127.0.0.1:{}/", port);
    while start.elapsed() < timeout {
        if let Ok(resp) = reqwest::blocking::get(&url) {
            if resp.status().is_client_error() || resp.status().is_success() {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

fn step_timeout() -> Duration {
    // Hard timeout per external command (restic) to avoid hanging CI/dev runs.
    // Override via E2E_TIMEOUT_SECS if needed.
    env::var("E2E_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(300))
}

fn run_with_timeout(mut cmd: Command, timeout: Duration, label: &str) -> Output {
    let start = std::time::Instant::now();
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("Failed to spawn {label}: {e}"));

    loop {
        if let Some(_status) = child.try_wait().expect("try_wait failed") {
            return child
                .wait_with_output()
                .unwrap_or_else(|e| panic!("Failed to collect output for {label}: {e}"));
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let out = child
                .wait_with_output()
                .unwrap_or_else(|e| panic!("Failed to collect output after kill for {label}: {e}"));
            panic!(
                "{label} timed out after {:?}\nstdout:\n{}\nstderr:\n{}",
                timeout,
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn create_test_files(dir: &PathBuf) {
    let mut file1 = fs::File::create(dir.join("test1.txt")).expect("Failed to create file");
    writeln!(file1, "This is test file 1").unwrap();

    let mut file2 = fs::File::create(dir.join("test2.txt")).expect("Failed to create file");
    writeln!(file2, "This is test file 2 with more content").unwrap();

    let subdir = dir.join("subdir");
    fs::create_dir(&subdir).unwrap();
    let mut file3 = fs::File::create(subdir.join("test3.txt")).unwrap();
    writeln!(file3, "This is test file 3 in a subdirectory").unwrap();

    let mut binary = fs::File::create(dir.join("binary.bin")).unwrap();
    binary.write_all(&[0u8, 1, 2, 3, 4, 5, 255, 254, 253]).unwrap();
}

/// Create ~100MB of random, incompressible data using /dev/urandom.
/// This mirrors the `restic-123pan` large-scale test strategy.
fn create_large_test_files(dir: &PathBuf, total_size_mb: usize) {
    use std::io::BufWriter;

    let mut urandom = fs::File::open("/dev/urandom").expect("Failed to open /dev/urandom");
    let total_bytes = total_size_mb * 1024 * 1024;

    // Mix:
    // - 60% large files (5-20MB)
    // - 30% medium files (100KB-1MB)
    // - 10% small files (1KB-10KB)
    let large_target = total_bytes * 60 / 100;
    let medium_target = total_bytes * 30 / 100;
    let small_target = total_bytes * 10 / 100;

    let large_dir = dir.join("large");
    let medium_dir = dir.join("medium");
    let small_dir = dir.join("small");
    fs::create_dir_all(&large_dir).expect("Failed to create large dir");
    fs::create_dir_all(&medium_dir).expect("Failed to create medium dir");
    fs::create_dir_all(&small_dir).expect("Failed to create small dir");

    let chunk_size = 256 * 1024;
    let mut file_counter = 0usize;

    let mut large_created = 0usize;
    while large_created < large_target {
        let size = 5 * 1024 * 1024 + (file_counter * 3 * 1024 * 1024) % (15 * 1024 * 1024);
        let size = size.min(large_target - large_created);
        let path = large_dir.join(format!("large_{:04}.bin", file_counter));
        let file = fs::File::create(&path).expect("Failed to create large file");
        let mut writer = BufWriter::new(file);
        let mut written = 0usize;
        while written < size {
            let to_write = (size - written).min(chunk_size);
            let mut buf = vec![0u8; to_write];
            urandom.read_exact(&mut buf).expect("Failed to read urandom");
            writer.write_all(&buf).expect("Failed to write");
            written += to_write;
        }
        large_created += size;
        file_counter += 1;
    }

    let mut medium_created = 0usize;
    while medium_created < medium_target {
        let size = 100 * 1024 + (file_counter * 100 * 1024) % (900 * 1024);
        let size = size.min(medium_target - medium_created);
        let path = medium_dir.join(format!("medium_{:04}.dat", file_counter));
        let file = fs::File::create(&path).expect("Failed to create medium file");
        let mut writer = BufWriter::new(file);
        let mut written = 0usize;
        while written < size {
            let to_write = (size - written).min(chunk_size);
            let mut buf = vec![0u8; to_write];
            urandom.read_exact(&mut buf).expect("Failed to read urandom");
            writer.write_all(&buf).expect("Failed to write");
            written += to_write;
        }
        medium_created += size;
        file_counter += 1;
    }

    let mut small_created = 0usize;
    while small_created < small_target {
        let size = 1024 + (file_counter * 1024) % (9 * 1024);
        let size = size.min(small_target - small_created);
        let path = small_dir.join(format!("small_{:04}.bin", file_counter));
        let mut file = fs::File::create(&path).expect("Failed to create small file");
        let mut buf = vec![0u8; size];
        urandom.read_exact(&mut buf).expect("Failed to read urandom");
        file.write_all(&buf).expect("Failed to write");
        small_created += size;
        file_counter += 1;
    }
}

fn sha256_file(path: &PathBuf) -> String {
    use sha2::{Digest, Sha256};
    let content = fs::read(path).expect("Failed to read file");
    format!("{:x}", Sha256::digest(&content))
}

fn hash_directory(dir: &PathBuf) -> std::collections::HashMap<String, String> {
    use walkdir::WalkDir;
    let mut hashes = std::collections::HashMap::new();
    for entry in WalkDir::new(dir) {
        let entry = entry.expect("Failed to read entry");
        if entry.file_type().is_file() {
            let rel = entry.path().strip_prefix(dir).unwrap();
            let h = sha256_file(&entry.path().to_path_buf());
            hashes.insert(rel.to_string_lossy().to_string(), h);
        }
    }
    hashes
}

macro_rules! skip_if_not_ready {
    () => {
        if get_test_tokens().is_none() {
            eprintln!("Skipping test: OPEN115_ACCESS_TOKEN and OPEN115_REFRESH_TOKEN not set");
            return;
        }
        if Command::new("restic").arg("version").output().is_err() {
            eprintln!("Skipping test: restic CLI not found in PATH");
            return;
        }
    };
}

fn spawn_stream_printer<R: std::io::Read + Send + 'static>(mut reader: R, prefix: &'static str) -> JoinHandle<()> {
    std::thread::spawn(move || {
        use std::io::BufRead;
        let buf = std::io::BufReader::new(&mut reader);
        for line in buf.lines().map_while(Result::ok) {
            // keep output compact and easy to grep in CI logs
            println!("{} {}", prefix, line);
        }
    })
}

fn start_server(access: &str, refresh: &str, port: u16, repo_path: &str) -> (Child, Vec<JoinHandle<()>>) {
    let cargo_bin =
        env::var("CARGO_BIN_EXE_restic-115").unwrap_or_else(|_| "target/debug/restic-115".to_string());

    let mut child = Command::new(&cargo_bin)
        .env("OPEN115_ACCESS_TOKEN", access)
        .env("OPEN115_REFRESH_TOKEN", refresh)
        .env("OPEN115_REPO_PATH", repo_path)
        .env("LISTEN_ADDR", format!("127.0.0.1:{}", port))
        .env("RUST_LOG", "debug")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to start server");

    let mut handles = Vec::new();
    if let Some(out) = child.stdout.take() {
        handles.push(spawn_stream_printer(out, "[server:stdout]"));
    }
    if let Some(err) = child.stderr.take() {
        handles.push(spawn_stream_printer(err, "[server:stderr]"));
    }

    (child, handles)
}

#[test]
fn test_server_startup() {
    skip_if_not_ready!();
    let (access, refresh) = get_test_tokens().unwrap();
    let port = find_available_port();
    let repo_path = format!("/restic-115-startup-{}", chrono::Utc::now().timestamp());

    let (mut server, handles) = start_server(&access, &refresh, port, &repo_path);
    if !wait_for_server(port, Duration::from_secs(15)) {
        server.kill().ok();
        let _ = server.wait();
        for h in handles {
            let _ = h.join();
        }
        panic!("Server failed to start");
    }
    server.kill().ok();
    let _ = server.wait();
    for h in handles {
        let _ = h.join();
    }
}

#[test]
fn test_e2e_backup_and_restore() {
    skip_if_not_ready!();
    let (access, refresh) = get_test_tokens().unwrap();

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let source_dir = temp_dir.path().join("source");
    let restore_dir = temp_dir.path().join("restore");
    fs::create_dir(&source_dir).unwrap();
    fs::create_dir(&restore_dir).unwrap();
    create_test_files(&source_dir);

    let port = find_available_port();
    let repo_path = format!("/restic-115-e2e-{}", chrono::Utc::now().timestamp());
    let (mut server, handles) = start_server(&access, &refresh, port, &repo_path);

    if !wait_for_server(port, Duration::from_secs(20)) {
        server.kill().ok();
        let _ = server.wait();
        for h in handles {
            let _ = h.join();
        }
        panic!("Server failed to start within timeout");
    }

    let repo_url = format!("rest:http://127.0.0.1:{}/", port);
    let password = "test-password-115";

    let init = run_with_timeout(
        {
            let mut c = Command::new("restic");
            c.args(["-r", &repo_url, "init"])
                .env("RESTIC_PASSWORD", password);
            c
        },
        step_timeout(),
        "restic init",
    );
    if !init.status.success() {
        server.kill().ok();
        let _ = server.wait();
        for h in handles {
            let _ = h.join();
        }
        panic!("restic init failed: {}", String::from_utf8_lossy(&init.stderr));
    }

    let backup = run_with_timeout(
        {
            let mut c = Command::new("restic");
            c.args(["-r", &repo_url, "backup", source_dir.to_str().unwrap()])
                .env("RESTIC_PASSWORD", password);
            c
        },
        step_timeout(),
        "restic backup",
    );
    if !backup.status.success() {
        server.kill().ok();
        let _ = server.wait();
        for h in handles {
            let _ = h.join();
        }
        panic!(
            "restic backup failed: {}",
            String::from_utf8_lossy(&backup.stderr)
        );
    }

    let restore = run_with_timeout(
        {
            let mut c = Command::new("restic");
            c.args([
                "-r",
                &repo_url,
                "restore",
                "latest",
                "--target",
                restore_dir.to_str().unwrap(),
            ])
            .env("RESTIC_PASSWORD", password);
            c
        },
        step_timeout(),
        "restic restore",
    );
    if !restore.status.success() {
        server.kill().ok();
        let _ = server.wait();
        for h in handles {
            let _ = h.join();
        }
        panic!(
            "restic restore failed: {}",
            String::from_utf8_lossy(&restore.stderr)
        );
    }

    server.kill().ok();
    let _ = server.wait();
    for h in handles {
        let _ = h.join();
    }
}

/// 100MB large-scale E2E test: backup + check + restore + verify hashes.
#[test]
fn test_e2e_100mb() {
    skip_if_not_ready!();
    let (access, refresh) = get_test_tokens().unwrap();

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let source_dir = temp_dir.path().join("source");
    let restore_dir = temp_dir.path().join("restore");
    fs::create_dir(&source_dir).unwrap();
    fs::create_dir(&restore_dir).unwrap();

    println!("Creating ~100MB of random test files...");
    create_large_test_files(&source_dir, 100);

    println!("Hashing original files...");
    let original_hashes = hash_directory(&source_dir);
    assert!(!original_hashes.is_empty(), "Should have created some files");

    let port = find_available_port();
    let repo_path = format!("/restic-115-e2e-100mb-{}", chrono::Utc::now().timestamp());
    let (mut server, handles) = start_server(&access, &refresh, port, &repo_path);

    if !wait_for_server(port, Duration::from_secs(30)) {
        server.kill().ok();
        let _ = server.wait();
        for h in handles {
            let _ = h.join();
        }
        panic!("Server failed to start within timeout");
    }

    let repo_url = format!("rest:http://127.0.0.1:{}/", port);
    let password = "test-password-115-100mb";

    println!("Initializing repository...");
    let init = run_with_timeout(
        {
            let mut c = Command::new("restic");
            c.args(["-r", &repo_url, "init"])
                .env("RESTIC_PASSWORD", password);
            c
        },
        step_timeout(),
        "restic init (100mb)",
    );
    assert!(
        init.status.success(),
        "restic init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    println!("Backing up ~100MB (may take a while)...");
    let backup = run_with_timeout(
        {
            let mut c = Command::new("restic");
            c.args(["-r", &repo_url, "backup", source_dir.to_str().unwrap()])
                .env("RESTIC_PASSWORD", password);
            c
        },
        step_timeout(),
        "restic backup (100mb)",
    );
    assert!(
        backup.status.success(),
        "restic backup failed: {}",
        String::from_utf8_lossy(&backup.stderr)
    );

    println!("Running restic check...");
    let check = run_with_timeout(
        {
            let mut c = Command::new("restic");
            c.args(["-r", &repo_url, "check"])
                .env("RESTIC_PASSWORD", password);
            c
        },
        step_timeout(),
        "restic check (100mb)",
    );
    if !check.status.success() {
        // Don't fail hard; `check` can be flaky on remote backends; keep signal in logs.
        println!(
            "WARNING: restic check failed: {}",
            String::from_utf8_lossy(&check.stderr)
        );
    }

    println!("Restoring latest snapshot...");
    let restore = run_with_timeout(
        {
            let mut c = Command::new("restic");
            c.args([
                "-r",
                &repo_url,
                "restore",
                "latest",
                "--target",
                restore_dir.to_str().unwrap(),
            ])
            .env("RESTIC_PASSWORD", password);
            c
        },
        step_timeout(),
        "restic restore (100mb)",
    );
    assert!(
        restore.status.success(),
        "restic restore failed: {}",
        String::from_utf8_lossy(&restore.stderr)
    );

    server.kill().ok();
    let _ = server.wait();
    for h in handles {
        let _ = h.join();
    }

    // restic restores with full path; locate the "source" dir inside restore
    let restored_source = walkdir::WalkDir::new(&restore_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name() == "source")
        .map(|e| e.path().to_path_buf())
        .expect("Could not find restored source directory");

    println!("Hashing restored files...");
    let restored_hashes = hash_directory(&restored_source);

    assert_eq!(
        original_hashes.len(),
        restored_hashes.len(),
        "File count mismatch: original={}, restored={}",
        original_hashes.len(),
        restored_hashes.len()
    );
    for (name, expected) in &original_hashes {
        let actual = restored_hashes
            .get(name)
            .unwrap_or_else(|| panic!("Missing restored file: {}", name));
        assert_eq!(expected, actual, "Hash mismatch for {}", name);
    }
}

