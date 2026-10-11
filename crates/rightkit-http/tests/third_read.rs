//! Regression tests for the third independent read of 0.3.0
//! (docs/audits/2026-10-11-astra-http-third-read.md). Each test names its finding.

mod common;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::{coord_dir, part_of, plant_claim};
use rightkit_http::testing::{loopback_config, MockBody, MockResponse, MockServer};
use rightkit_http::*;

fn cfg() -> ClientConfig {
    ClientConfig {
        retry: RetryPolicy::none(),
        request_timeout: Duration::from_secs(5),
        response_timeout: Some(Duration::from_secs(5)),
        max_redirects: 3,
        ..loopback_config()
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("third-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

// ------------------------------------------------------------------ finding 1
/// The partial file is a second name for another file, and the claim record vouches for it, so
/// it reaches the open on resume: refused before any write or truncation.
#[test]
fn f1_a_hard_linked_part_is_not_truncated_or_overwritten_on_resume() {
    let server = MockServer::start(|_, _| MockResponse::text(200, "fresh bytes"));
    let dir = scratch("f1");
    let victim = dir.join("precious.txt");
    fs::write(&victim, b"precious").unwrap();
    let dest = dir.join("out.bin");
    plant_claim(&dest);
    let part = part_of(&dest);
    fs::hard_link(&victim, &part).expect("hard link fixture must be created");
    let err = Client::new(cfg())
        .download_to_path(
            &server.url("/f"),
            &dest,
            DownloadOptions {
                resume: true,
                ..DownloadOptions::default()
            },
        )
        .expect_err("a hard-linked partial must be refused");
    assert!(matches!(err, DownloadError::Io(_)), "{err:?}");
    assert_eq!(fs::read(&victim).unwrap(), b"precious");
    assert_eq!(fs::read(&part).unwrap(), b"precious");
    assert!(!dest.exists());
}

/// The same hard link with no claim record behind it was not made by a download of this
/// destination: it is unlinked, never followed, and the download proceeds on a fresh file.
#[test]
fn f1_an_unclaimed_hard_linked_part_is_unlinked_and_the_other_name_keeps_its_bytes() {
    let server = MockServer::start(|_, _| MockResponse::text(200, "fresh bytes"));
    let dir = scratch("f1-unclaimed");
    let victim = dir.join("precious.txt");
    fs::write(&victim, b"precious").unwrap();
    let dest = dir.join("out.bin");
    fs::create_dir_all(coord_dir(&dest)).unwrap();
    fs::hard_link(&victim, part_of(&dest)).expect("hard link fixture must be created");
    let report = Client::new(cfg())
        .download_to_path(
            &server.url("/f"),
            &dest,
            DownloadOptions {
                resume: true,
                ..DownloadOptions::default()
            },
        )
        .unwrap();
    assert_eq!(report.bytes, 11);
    assert_eq!(fs::read(&dest).unwrap(), b"fresh bytes");
    assert_eq!(fs::read(&victim).unwrap(), b"precious");
}

// ------------------------------------------------------------------ finding 2
/// Names that used to be reserved for the lock and partial file beside the destination are
/// ordinary destination names now: those files are not beside the destination any more.
#[test]
fn f2_destinations_named_like_the_old_internal_files_are_ordinary_destinations() {
    let server = MockServer::start(|_, _| MockResponse::text(200, "x"));
    let dir = scratch("f2");
    for name in ["out.lock", "out.part", "OUT.LOCK"] {
        let dest = dir.join(name);
        let report = Client::new(cfg())
            .download_to_path(&server.url("/f"), &dest, DownloadOptions::default())
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!(report.bytes, 1, "{name}");
        assert_eq!(fs::read(&dest).unwrap(), b"x", "{name}");
    }
}

// ------------------------------------------------------------------ finding 3
/// First answer comes from `first`, is cut short, and the reconnect lands on `second`, which
/// answers the ranged request with `416 bytes */4`. The four saved bytes belong to another
/// resource, so they must not be published; the download restarts and gets `second`'s bytes.
fn run_416_after_redirect_change(case: &str, first: &'static str, second: &'static str) {
    let n = Arc::new(AtomicUsize::new(0));
    let counter = n.clone();
    let server = MockServer::start(move |req, _| {
        let target = if req.query().is_empty() {
            req.path().to_string()
        } else {
            format!("{}?{}", req.path(), req.query())
        };
        if req.path() == "/dl" {
            let i = counter.fetch_add(1, Ordering::SeqCst);
            let to = if i == 0 { first } else { second };
            return MockResponse::new(302).header("Location", to);
        }
        if target == first {
            let mut r = MockResponse::new(200);
            r.body = MockBody::TruncatedAfter(b"AAAA".to_vec());
            return r;
        }
        if req.header("range").is_some() {
            MockResponse::new(416).header("Content-Range", "bytes */4")
        } else {
            MockResponse::new(200).body(b"BBBB".to_vec())
        }
    });
    let dir = scratch(case);
    let dest = dir.join("out.bin");
    Client::new(cfg())
        .download_to_path(
            &server.url("/dl"),
            &dest,
            DownloadOptions {
                max_reconnects: 2,
                ..DownloadOptions::default()
            },
        )
        .unwrap();
    assert_eq!(fs::read(&dest).unwrap(), b"BBBB");
}

#[test]
fn f3_a_416_from_a_different_path_does_not_publish_the_old_prefix() {
    run_416_after_redirect_change("f3-path", "/a", "/b");
}

#[test]
fn f3_a_416_from_a_different_query_does_not_publish_the_old_prefix() {
    run_416_after_redirect_change("f3-query", "/f?id=A", "/f?id=B");
}

// ------------------------------------------------------------------ finding 4
#[test]
fn f4_secret_form_fields_are_scrubbed_from_status_errors() {
    let server = MockServer::start(|_, _| {
        MockResponse::new(400)
            .header("Content-Type", "application/json")
            .body(
                br#"{"error":"invalid_request","error_description":"rejected single-use"}"#
                    .to_vec(),
            )
    });
    let req = Request::post(server.url("/token")).secret_form(
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", "single-use"),
        ],
        &["refresh_token"],
    );
    let err = Client::new(cfg()).send(&req).unwrap_err();
    let text = format!("{err} {err:?}");
    assert!(!text.contains("single-use"), "{text}");
    assert!(text.contains("[redacted]"), "{text}");
    assert_eq!(
        req.scrub("rejected single-use, grant refresh_token"),
        "rejected [redacted], grant refresh_token"
    );
}
