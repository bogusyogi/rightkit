//! Behaviour of the the app download adapter over a real loopback server.
use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rightkit-resume-test-{}-{}",
        std::process::id(),
        NEXT_DIR.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// The crate's vetted client refuses loopback; allow exactly 127.0.0.1 for the test server.
fn loopback_client() -> ModelHttp {
    http_client_with_options(ModelHttpOptions {
        address_policy: rightkit_http::AddressPolicy::public_web()
            .allow_if(vec!["127.0.0.1".to_string()], |ip| ip.is_loopback()),
        ..Default::default()
    })
}

fn sha256(bytes: &[u8]) -> String {
    rightkit_fs::digest::sha256_bytes(bytes)
}

/// One scripted reply: status line + headers, then body.
struct Reply {
    head: String,
    body: Vec<u8>,
}

fn ok(body: &[u8], etag: &str) -> Reply {
    Reply {
        head: format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: {etag}\r\nConnection: close\r\n\r\n",
            body.len()
        ),
        body: body.to_vec(),
    }
}

fn status(line: &str) -> Reply {
    Reply {
        head: format!("HTTP/1.1 {line}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
        body: Vec::new(),
    }
}

/// Serve `replies` in order, one connection each; returns the URL, the raw
/// request heads seen, and the join handle.
fn serve(replies: Vec<Reply>) -> (String, Arc<Mutex<Vec<String>>>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/model", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let server_seen = Arc::clone(&seen);
    let handle = std::thread::spawn(move || {
        for reply in replies {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let read = stream.read(&mut buffer).unwrap();
                if read == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..read]);
                if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            server_seen
                .lock()
                .unwrap()
                .push(String::from_utf8(bytes).unwrap().to_ascii_lowercase());
            stream.write_all(reply.head.as_bytes()).unwrap();
            stream.write_all(&reply.body).unwrap();
        }
    });
    (url, seen, handle)
}

fn seed_partial(part: &Path, prefix: &[u8], url: &str, checksum: &str, etag: &str, total: usize) {
    fs::write(part, prefix).unwrap();
    let metadata = serde_json::json!({
        "url": url, "expected_sha256": checksum, "etag": etag, "total": total
    });
    let mut path = part.as_os_str().to_os_string();
    path.push(".meta");
    fs::write(path, serde_json::to_vec(&metadata).unwrap()).unwrap();
}

fn request<'a>(url: &'a str, part: &'a Path, checksum: &'a str) -> DownloadRequest<'a> {
    DownloadRequest {
        url,
        part_path: part,
        expected_sha256: checksum,
        authorization: None,
    }
}

#[test]
fn archive_path_strips_part_suffix() {
    assert_eq!(
        archive_path_for(Path::new("/m/.parakeet.abc.tar.gz.part")),
        PathBuf::from("/m/.parakeet.abc.tar.gz")
    );
    assert_eq!(
        archive_path_for(Path::new("/m/model.bin")),
        PathBuf::from("/m/model.bin.verified")
    );
}

#[test]
fn valid_resume_requests_the_remaining_range_and_appends() {
    let dir = temp_dir();
    let part = dir.join("model.tar.gz.part");
    let full = b"resume-safe-model";
    let checksum = sha256(full);
    let split = 7;
    let (url, seen, server) = serve(vec![Reply {
        head: format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {split}-{}/{}\r\nETag: \"v1\"\r\nConnection: close\r\n\r\n",
            full.len() - split,
            full.len() - 1,
            full.len()
        ),
        body: full[split..].to_vec(),
    }]);
    seed_partial(&part, &full[..split], &url, &checksum, "\"v1\"", full.len());

    let agent = loopback_client();
    let result = download_resumable(&agent, request(&url, &part, &checksum), |_, _| {}).unwrap();

    server.join().unwrap();
    let seen = seen.lock().unwrap();
    assert!(seen[0].contains("range: bytes=7-"));
    assert!(seen[0].contains("if-range: \"v1\""));
    assert_eq!(result.total, full.len() as u64);
    assert_eq!(fs::read(&result.archive_path).unwrap(), full);
    assert!(!part.exists(), "part is committed into the archive");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn ignored_range_200_replaces_instead_of_appending() {
    let dir = temp_dir();
    let part = dir.join("model.tar.gz.part");
    let full = b"complete-model-bytes";
    let checksum = sha256(full);
    let (url, _seen, server) = serve(vec![ok(full, "\"v1\"")]);
    seed_partial(
        &part,
        b"stale-prefix",
        &url,
        &checksum,
        "\"v1\"",
        full.len(),
    );

    let agent = loopback_client();
    let result = download_resumable(&agent, request(&url, &part, &checksum), |_, _| {}).unwrap();

    server.join().unwrap();
    assert_eq!(fs::read(&result.archive_path).unwrap(), full);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn etag_mismatch_restarts_from_zero_without_range() {
    let dir = temp_dir();
    let part = dir.join("model.tar.gz.part");
    let full = b"fresh-model-bytes";
    let checksum = sha256(full);
    let (url, seen, server) = serve(vec![ok(full, "\"v2\"")]);
    // Weak stored ETag: continuity cannot be proven, so no Range may be sent.
    seed_partial(&part, b"old-", &url, &checksum, "W/\"v1\"", full.len());

    let agent = loopback_client();
    let result = download_resumable(&agent, request(&url, &part, &checksum), |_, _| {}).unwrap();

    server.join().unwrap();
    assert!(!seen.lock().unwrap()[0].contains("range:"));
    assert_eq!(fs::read(&result.archive_path).unwrap(), full);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn final_hash_mismatch_removes_partial_metadata_and_archive() {
    let dir = temp_dir();
    let part = dir.join("model.tar.gz.part");
    let checksum = sha256(b"expected");
    let (url, _seen, server) = serve(vec![ok(b"corrupt", "\"v1\"")]);

    let agent = loopback_client();
    let error = download_resumable(&agent, request(&url, &part, &checksum), |_, _| {})
        .err()
        .expect("mismatch must fail");

    server.join().unwrap();
    assert!(error.contains("integrity check"), "{error}");
    assert!(!part.exists());
    assert!(!dir.join("model.tar.gz.part.meta").exists());
    assert!(!dir.join("model.tar.gz").exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn invalid_checksum_is_rejected_before_any_request() {
    let dir = temp_dir();
    let part = dir.join("model.tar.gz.part");
    fs::write(&part, b"poison").unwrap();
    let agent = loopback_client();
    let error = download_resumable(
        &agent,
        request("http://127.0.0.1:1/never", &part, "not-a-sha"),
        |_, _| {},
    )
    .err()
    .unwrap();
    assert_eq!(error, "download checksum must be a 64-character SHA-256");
    assert!(!part.exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn complete_verified_archive_survives_restart_without_network() {
    let dir = temp_dir();
    let part = dir.join("model.tar.gz.part");
    let archive = archive_path_for(&part);
    let full = b"already-complete-model";
    fs::write(&archive, full).unwrap();

    let agent = loopback_client();
    let result = download_resumable(
        &agent,
        request("not-a-valid-url", &part, &sha256(full)),
        |_, _| {},
    )
    .unwrap();

    assert_eq!(result.received, full.len() as u64);
    assert_eq!(result.archive_path, archive);
    assert_eq!(fs::read(&archive).unwrap(), full);
    discard_partial(&part);
    assert!(!archive.exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn permanent_http_errors_are_not_retried() {
    let dir = temp_dir();
    let part = dir.join("model.tar.gz.part");
    let checksum = sha256(b"x");
    let (url, seen, server) = serve(vec![status("403 Forbidden")]);
    let agent = loopback_client();
    let error = download_resumable(&agent, request(&url, &part, &checksum), |_, _| {})
        .err()
        .unwrap();
    server.join().unwrap();
    assert!(is_authorization_rejection(&error), "{error}");
    assert_eq!(seen.lock().unwrap().len(), 1);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn rejected_authorization_is_refreshed_once() {
    let body = b"scoped-grant-model";
    let checksum = sha256(body);
    let (url, seen, server) = serve(vec![status("401 Unauthorized"), ok(body, "\"model-v1\"")]);
    let dir = temp_dir();
    let part = dir.join("model.tar.gz.part");
    let mut grants = vec!["Bearer grant-2".to_string(), "Bearer grant-1".to_string()];
    let agent = loopback_client();

    let result = download_resumable_with_authorization_refresh(
        &agent,
        RefreshingDownloadRequest {
            url: &url,
            part_path: &part,
            expected_sha256: &checksum,
        },
        || grants.pop().ok_or_else(|| "no more grants".to_string()),
        |_, _| {},
    )
    .unwrap();

    server.join().unwrap();
    assert_eq!(result.received, body.len() as u64);
    assert_eq!(fs::read(&result.archive_path).unwrap(), body);
    let seen = seen.lock().unwrap();
    assert!(seen[0].contains("authorization: bearer grant-1"));
    assert!(seen[1].contains("authorization: bearer grant-2"));
    discard_partial(&part);
    fs::remove_dir_all(dir).unwrap();
}
