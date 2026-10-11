//! download_to_path against an in-process HTTP server: fresh, resume (206),
//! restart (200), 416 verify, cancel keeps .part, integrity and length mismatch
//! delete .part, and reconnect after a truncated body.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rightkit_http::testing::{MockBody, MockResponse, MockServer};
use rightkit_http::*;
use sha2::{Digest, Sha256};

fn payload() -> Vec<u8> {
    (0..200u32).map(|i| (i % 251) as u8).collect()
}

fn sha_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn client() -> Client {
    Client::new(ClientConfig {
        retry: RetryPolicy::none(),
        ..rightkit_http::testing::loopback_config()
    })
}

/// Fresh, empty scratch directory under cargo's per-target temp dir.
fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("download-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn part_of(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

#[test]
fn fresh_download_writes_dest_and_reports_hash() {
    let data = payload();
    let served = data.clone();
    let server = MockServer::start(move |_req, _i| {
        MockResponse::new(200)
            .header("ETag", "\"v1\"")
            .body(served.clone())
    });
    let dir = scratch("fresh");
    let dest = dir.join("out.bin");
    let mut seen = Vec::new();
    let mut cb = |p: &DownloadProgress| seen.push(p.downloaded);
    let report = client()
        .download_to_path(
            &server.url("/file"),
            &dest,
            DownloadOptions {
                on_progress: Some(&mut cb),
                ..DownloadOptions::default()
            },
        )
        .unwrap();
    assert_eq!(report.bytes, 200);
    assert_eq!(report.sha256, sha_hex(&data));
    assert_eq!(report.resumed_from, 0);
    assert_eq!(report.attempts, 1);
    assert_eq!(fs::read(&dest).unwrap(), data);
    assert!(!part_of(&dest).exists());
    assert_eq!(seen.last(), Some(&200));
    let reqs = server.requests();
    assert_eq!(reqs[0].header("range"), None);
    assert_eq!(reqs[0].header("accept-encoding"), Some("identity"));
}

#[test]
fn resume_appends_on_206() {
    let data = payload();
    let served = data.clone();
    let server = MockServer::start(move |req, _i| {
        if req.header("range") == Some("bytes=80-") {
            MockResponse::new(206)
                .header("Content-Range", "bytes 80-199/200")
                .body(served[80..].to_vec())
        } else {
            MockResponse::new(500)
        }
    });
    let dir = scratch("resume");
    let dest = dir.join("out.bin");
    fs::write(part_of(&dest), &data[..80]).unwrap();
    let report = client()
        .download_to_path(
            &server.url("/file"),
            &dest,
            DownloadOptions {
                resume: true,
                expected_sha256: Some(sha_hex(&data)),
                ..DownloadOptions::default()
            },
        )
        .unwrap();
    assert_eq!(report.bytes, 200);
    assert_eq!(report.resumed_from, 80);
    assert_eq!(report.attempts, 1);
    assert_eq!(report.sha256, sha_hex(&data));
    assert_eq!(fs::read(&dest).unwrap(), data);
    assert!(!part_of(&dest).exists());
    let reqs = server.requests();
    assert_eq!(reqs[0].header("range"), Some("bytes=80-"));
    assert_eq!(reqs[0].header("if-range"), None, "no ETag known from disk");
}

#[test]
fn restart_on_200_truncates_partial_file() {
    let data = payload();
    let served = data.clone();
    let server = MockServer::start(move |_req, _i| MockResponse::new(200).body(served.clone()));
    let dir = scratch("restart");
    let dest = dir.join("out.bin");
    fs::write(part_of(&dest), vec![0xEEu8; 50]).unwrap();
    let report = client()
        .download_to_path(
            &server.url("/file"),
            &dest,
            DownloadOptions {
                resume: true,
                expected_sha256: Some(sha_hex(&data)),
                ..DownloadOptions::default()
            },
        )
        .unwrap();
    assert_eq!(report.resumed_from, 0);
    assert_eq!(report.bytes, 200);
    assert_eq!(fs::read(&dest).unwrap(), data);
    assert_eq!(server.requests()[0].header("range"), Some("bytes=50-"));
}

#[test]
fn complete_partial_verifies_on_416_without_fetching_bytes() {
    let data = payload();
    let server =
        MockServer::start(|_req, _i| MockResponse::new(416).header("Content-Range", "bytes */200"));
    let dir = scratch("verify416");
    let dest = dir.join("out.bin");
    fs::write(part_of(&dest), &data).unwrap();
    let report = client()
        .download_to_path(
            &server.url("/file"),
            &dest,
            DownloadOptions {
                resume: true,
                expected_sha256: Some(sha_hex(&data)),
                expected_bytes: Some(200),
                ..DownloadOptions::default()
            },
        )
        .unwrap();
    assert_eq!(report.attempts, 1);
    assert_eq!(report.resumed_from, 200);
    assert_eq!(fs::read(&dest).unwrap(), data);
    assert_eq!(server.request_count(), 1);
}

#[test]
fn verify_on_416_with_wrong_hash_deletes_part() {
    let data = payload();
    let server =
        MockServer::start(|_req, _i| MockResponse::new(416).header("Content-Range", "bytes */200"));
    let dir = scratch("verify416-bad");
    let dest = dir.join("out.bin");
    fs::write(part_of(&dest), &data).unwrap();
    let err = client()
        .download_to_path(
            &server.url("/file"),
            &dest,
            DownloadOptions {
                resume: true,
                expected_sha256: Some("00".repeat(32)),
                ..DownloadOptions::default()
            },
        )
        .unwrap_err();
    assert!(matches!(err, DownloadError::Integrity { .. }), "{err:?}");
    assert!(!part_of(&dest).exists());
    assert!(!dest.exists());
}

#[test]
fn cancel_keeps_part_and_reports_partial_bytes() {
    let data = payload();
    let chunks = vec![
        (Duration::ZERO, data[..50].to_vec()),
        (Duration::from_millis(150), data[50..100].to_vec()),
        (Duration::from_millis(150), data[100..].to_vec()),
    ];
    let server = MockServer::start(move |_req, _i| MockResponse::new(200).chunks(chunks.clone()));
    let dir = scratch("cancel");
    let dest = dir.join("out.bin");
    let flag = AtomicBool::new(false);
    let mut cb = |p: &DownloadProgress| {
        if p.downloaded >= 50 {
            flag.store(true, Ordering::SeqCst);
        }
    };
    let err = client()
        .download_to_path(
            &server.url("/file"),
            &dest,
            DownloadOptions {
                cancel: Some(&flag),
                on_progress: Some(&mut cb),
                ..DownloadOptions::default()
            },
        )
        .unwrap_err();
    let partial = match err {
        DownloadError::Cancelled { partial_bytes } => partial_bytes,
        other => panic!("expected Cancelled, got {other:?}"),
    };
    assert!((50..200).contains(&partial), "partial={partial}");
    assert_eq!(fs::metadata(part_of(&dest)).unwrap().len(), partial);
    assert!(!dest.exists());
}

#[test]
fn integrity_mismatch_deletes_part() {
    let served = payload();
    let server = MockServer::start(move |_req, _i| MockResponse::new(200).body(served.clone()));
    let dir = scratch("integrity");
    let dest = dir.join("out.bin");
    let err = client()
        .download_to_path(
            &server.url("/file"),
            &dest,
            DownloadOptions {
                expected_sha256: Some("00".repeat(32)),
                ..DownloadOptions::default()
            },
        )
        .unwrap_err();
    match err {
        DownloadError::Integrity { expected, actual } => {
            assert_eq!(expected, "00".repeat(32));
            assert_eq!(actual, sha_hex(&payload()));
        }
        other => panic!("expected Integrity, got {other:?}"),
    }
    assert!(!part_of(&dest).exists());
    assert!(!dest.exists());
}

#[test]
fn length_mismatch_deletes_part() {
    let served = payload();
    let server = MockServer::start(move |_req, _i| MockResponse::new(200).body(served.clone()));
    let dir = scratch("length");
    let dest = dir.join("out.bin");
    let err = client()
        .download_to_path(
            &server.url("/file"),
            &dest,
            DownloadOptions {
                expected_bytes: Some(999),
                ..DownloadOptions::default()
            },
        )
        .unwrap_err();
    assert!(
        matches!(
            err,
            DownloadError::LengthMismatch {
                expected: 999,
                actual: 200
            }
        ),
        "{err:?}"
    );
    assert!(!part_of(&dest).exists());
}

#[test]
fn truncated_body_reconnects_with_if_range() {
    let data = payload();
    let first = data[..80].to_vec();
    let rest = data[80..].to_vec();
    let server = MockServer::start(move |req, index| {
        if index == 0 {
            let mut r = MockResponse::new(200).header("ETag", "\"v1\"");
            r.body = MockBody::TruncatedAfter(first.clone());
            r
        } else if req.header("range") == Some("bytes=80-")
            && req.header("if-range") == Some("\"v1\"")
        {
            MockResponse::new(206)
                .header("Content-Range", "bytes 80-199/200")
                .body(rest.clone())
        } else {
            MockResponse::new(500)
        }
    });
    let dir = scratch("reconnect");
    let dest = dir.join("out.bin");
    let report = client()
        .download_to_path(
            &server.url("/file"),
            &dest,
            DownloadOptions {
                max_reconnects: 2,
                ..DownloadOptions::default()
            },
        )
        .unwrap();
    assert_eq!(report.attempts, 2);
    assert_eq!(report.bytes, 200);
    assert_eq!(fs::read(&dest).unwrap(), data);
}

#[test]
fn caller_range_header_is_rejected() {
    let server = MockServer::start(|_req, _i| MockResponse::new(200));
    let dir = scratch("range-header");
    let dest = dir.join("out.bin");
    let err = client()
        .download_to_path(
            &server.url("/file"),
            &dest,
            DownloadOptions {
                headers: vec![("Range".into(), "bytes=0-9".into())],
                ..DownloadOptions::default()
            },
        )
        .unwrap_err();
    assert!(matches!(
        err,
        DownloadError::Http(HttpError::InvalidRequest(_))
    ));
    assert_eq!(server.request_count(), 0);
}
