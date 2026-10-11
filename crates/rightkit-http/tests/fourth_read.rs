//! Regression tests for the fourth independent read
//! (docs/audits/2026-10-11-astra-http-fourth-read.md). Finding 2 is the destination-name
//! class; finding 6 is the resume/restart decision table in the module docs of
//! `src/download.rs`, one test per row that no earlier file already pins.
//!
//! Rows pinned elsewhere: append on a proven `206` (`download.rs::resume_appends_on_206`,
//! `truncated_body_reconnects_with_if_range`), `416` restarts (`download.rs::
//! a_416_restarts_from_zero_even_when_the_partial_file_is_complete`, `fifth_read.rs::f10_*`),
//! `206`/`416` from another resource (`third_read.rs::f3_*`, `audit_fixes.rs` splice test),
//! malformed `206` (`audit_030.rs::f4_*`, `audit_fixes.rs::f16_*`), `200` over a cross-call
//! prefix (`download.rs::restart_on_200_truncates_partial_file`).

mod common;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::plant_part;
use rightkit_http::testing::{
    loopback_config, MockBody, MockResponse, MockServer, RecordedRequest,
};
use rightkit_http::*;
use sha2::{Digest, Sha256};

fn cfg() -> ClientConfig {
    ClientConfig {
        retry: RetryPolicy::none(),
        request_timeout: Duration::from_secs(5),
        response_timeout: Some(Duration::from_secs(5)),
        ..loopback_config()
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("fourth-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn sha_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ------------------------------------------------------------------ finding 2
/// A trailing dot or space is dropped by Windows before the lookup, so `.rk-download.` would
/// name the download directory there, and `out.` would name `out`. The refusal is made on every
/// platform, before any request or filesystem access, and nothing is created.
#[test]
fn f2_destinations_that_windows_normalizes_onto_another_name_are_refused() {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    let server = MockServer::start(move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        MockResponse::text(200, "x")
    });
    let dir = scratch("f2-alias");
    let held = dir.join("out.lock");
    fs::write(&held, b"an ordinary file").unwrap();
    for name in [
        ".rk-download.",
        ".rk-download ",
        "out.lock.",
        "out.lock..",
        "out.lock ",
        "OUT.LOCK. ",
        "out.part.",
        "out.part ",
        "out.",
        "out ",
    ] {
        let dest = dir.join(name);
        let err = Client::new(cfg())
            .download_to_path(&server.url("/f"), &dest, DownloadOptions::default())
            .expect_err("an aliasing destination must be refused");
        assert!(matches!(err, DownloadError::Io(_)), "{name:?}: {err:?}");
    }
    assert_eq!(hits.load(Ordering::SeqCst), 0, "no request may be sent");
    assert_eq!(fs::read(&held).unwrap(), b"an ordinary file");
    assert_eq!(
        fs::read_dir(&dir).unwrap().count(),
        1,
        "nothing may be created"
    );
}

// ------------------------------------------------------------------ finding 6
/// First request: `200` carrying `first_headers` and a body cut after `AAAA` (the declared
/// length is 1004). Any later request with a `Range` header is answered by `ranged`; one
/// without gets a complete `200` with `BBBB`. Returns the published bytes and every request.
fn run_reconnect(
    case: &str,
    first_headers: &'static [(&'static str, &'static str)],
    ranged: fn() -> MockResponse,
    expected_sha256: Option<String>,
) -> (Vec<u8>, Vec<RecordedRequest>) {
    let server = MockServer::start(move |req, index| {
        if index == 0 {
            let mut r = MockResponse::new(200);
            for (k, v) in first_headers {
                r = r.header(k, v);
            }
            r.body = MockBody::TruncatedAfter(b"AAAA".to_vec());
            r
        } else if req.header("range").is_some() {
            ranged()
        } else {
            MockResponse::new(200).body(b"BBBB".to_vec())
        }
    });
    let dir = scratch(case);
    let dest = dir.join("out.bin");
    let report = Client::new(cfg())
        .download_to_path(
            &server.url("/f"),
            &dest,
            DownloadOptions {
                max_reconnects: 3,
                expected_sha256,
                ..DownloadOptions::default()
            },
        )
        .unwrap();
    assert_eq!(report.bytes, 4, "{case}");
    (fs::read(&dest).unwrap(), server.requests())
}

/// The audit's sequence: `200`, no ETag, cut after `AAAA`; the resource is replaced by
/// `BBBB` at the same URL. The prefix has no validator, so the reconnect must not ask for a
/// range at all (a `416 */4` would have been read as "complete" and published `AAAA`).
#[test]
fn f6_no_validator_means_the_reconnect_restarts_from_zero() {
    let (bytes, reqs) = run_reconnect(
        "f6-novalidator",
        &[],
        || MockResponse::new(416).header("Content-Range", "bytes */4"),
        None,
    );
    assert_eq!(bytes, b"BBBB");
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[1].header("range"), None);
}

/// A weak ETag is not a strong validator either.
#[test]
fn f6_a_weak_etag_does_not_prove_the_prefix() {
    let (bytes, reqs) = run_reconnect(
        "f6-weak",
        &[("ETag", "W/\"v1\"")],
        || MockResponse::new(416).header("Content-Range", "bytes */4"),
        None,
    );
    assert_eq!(bytes, b"BBBB");
    assert_eq!(reqs[1].header("range"), None);
    assert_eq!(reqs[1].header("if-range"), None);
}

/// Strong ETag, but the server ignores `If-Range` and answers `416 */4`. That total
/// contradicts the 1004 the first response declared, so it is not a completion.
#[test]
fn f6_a_416_total_that_contradicts_the_saved_total_restarts() {
    let (bytes, reqs) = run_reconnect(
        "f6-416-etag",
        &[("ETag", "\"v1\"")],
        || MockResponse::new(416).header("Content-Range", "bytes */4"),
        None,
    );
    assert_eq!(bytes, b"BBBB");
    assert_eq!(reqs.len(), 3);
    assert_eq!(reqs[1].header("range"), Some("bytes=4-"));
    assert_eq!(reqs[1].header("if-range"), Some("\"v1\""));
    assert_eq!(reqs[2].header("range"), None);
}

/// Same sequence proven by `expected_sha256` instead of an ETag: the range is sent (the hash
/// will decide), the contradicting `416` still restarts, and the final hash passes.
#[test]
fn f6_a_416_total_that_contradicts_the_saved_total_restarts_with_a_hash() {
    let (bytes, reqs) = run_reconnect(
        "f6-416-hash",
        &[],
        || MockResponse::new(416).header("Content-Range", "bytes */4"),
        Some(sha_hex(b"BBBB")),
    );
    assert_eq!(bytes, b"BBBB");
    assert_eq!(reqs[1].header("range"), Some("bytes=4-"));
    assert_eq!(reqs[1].header("if-range"), None);
    assert_eq!(reqs[2].header("range"), None);
}

/// `206` whose ETag differs from the saved one: another representation.
#[test]
fn f6_a_206_with_another_etag_restarts() {
    let (bytes, reqs) = run_reconnect(
        "f6-206-etag",
        &[("ETag", "\"v1\"")],
        || {
            MockResponse::new(206)
                .header("ETag", "\"v2\"")
                .header("Content-Range", "bytes 4-7/1004")
                .body(b"CCCC".to_vec())
        },
        None,
    );
    assert_eq!(bytes, b"BBBB");
    assert_eq!(reqs[2].header("range"), None);
}

/// `206` whose `Last-Modified` differs from the saved one (proven only by the hash).
#[test]
fn f6_a_206_with_another_last_modified_restarts() {
    let (bytes, reqs) = run_reconnect(
        "f6-206-lm",
        &[("Last-Modified", "Mon, 01 Jan 2024 00:00:00 GMT")],
        || {
            MockResponse::new(206)
                .header("Last-Modified", "Tue, 02 Jan 2024 00:00:00 GMT")
                .header("Content-Range", "bytes 4-7/1004")
                .body(b"CCCC".to_vec())
        },
        Some(sha_hex(b"BBBB")),
    );
    assert_eq!(bytes, b"BBBB");
    assert_eq!(reqs[2].header("range"), None);
}

/// `206` that reports a smaller total than the first response declared (the object shrank).
#[test]
fn f6_a_206_whose_total_shrank_restarts() {
    let (bytes, reqs) = run_reconnect(
        "f6-206-shrink",
        &[("ETag", "\"v1\"")],
        || {
            MockResponse::new(206)
                .header("ETag", "\"v1\"")
                .header("Content-Range", "bytes 4-7/8")
                .body(b"CCCC".to_vec())
        },
        None,
    );
    assert_eq!(bytes, b"BBBB");
    assert_eq!(reqs[2].header("range"), None);
}

/// `206` that reports a larger total (the object grew).
#[test]
fn f6_a_206_whose_total_grew_restarts() {
    let (bytes, reqs) = run_reconnect(
        "f6-206-grow",
        &[("ETag", "\"v1\"")],
        || {
            MockResponse::new(206)
                .header("ETag", "\"v1\"")
                .header("Content-Range", "bytes 4-7/2004")
                .body(b"CCCC".to_vec())
        },
        None,
    );
    assert_eq!(bytes, b"BBBB");
    assert_eq!(reqs[2].header("range"), None);
}

/// `200` to a ranged request (the server's `If-Range` check failed): the body replaces the
/// prefix.
#[test]
fn f6_a_200_to_a_ranged_request_replaces_the_prefix() {
    let (bytes, reqs) = run_reconnect(
        "f6-200",
        &[("ETag", "\"v1\"")],
        || MockResponse::new(200).body(b"BBBB".to_vec()),
        None,
    );
    assert_eq!(bytes, b"BBBB");
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[1].header("range"), Some("bytes=4-"));
}

/// A partial file from an earlier call, with nothing to prove it: it is discarded and no range
/// is requested.
#[test]
fn f6_a_prefix_from_an_earlier_call_without_a_hash_is_discarded() {
    let server = MockServer::start(|_, _| MockResponse::new(200).body(b"NEWDATA".to_vec()));
    let dir = scratch("f6-crosscall");
    let dest = dir.join("out.bin");
    plant_part(&dest, b"OLD!");
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
    assert_eq!(report.resumed_from, 0);
    assert_eq!(fs::read(&dest).unwrap(), b"NEWDATA");
    assert_eq!(server.requests()[0].header("range"), None);
}

/// A partial file from an earlier call with a hash: the range is requested. A `416` restarts,
/// and the restart is a plain request.
#[test]
fn f6_a_416_restarts_with_a_plain_request() {
    let server = MockServer::start(|req, _| {
        if req.header("range").is_some() {
            MockResponse::new(416).header("Content-Range", "bytes */300")
        } else {
            MockResponse::new(200).body(b"NEWDATA".to_vec())
        }
    });
    let dir = scratch("f6-416-other");
    let dest = dir.join("out.bin");
    plant_part(&dest, b"zzzz");
    Client::new(cfg())
        .download_to_path(
            &server.url("/f"),
            &dest,
            DownloadOptions {
                resume: true,
                expected_sha256: Some(sha_hex(b"NEWDATA")),
                ..DownloadOptions::default()
            },
        )
        .unwrap();
    assert_eq!(fs::read(&dest).unwrap(), b"NEWDATA");
    let reqs = server.requests();
    assert_eq!(reqs[0].header("range"), Some("bytes=4-"));
    assert_eq!(reqs[1].header("range"), None);
}

/// `304` is never a download: nothing is published.
#[test]
fn f6_a_304_publishes_nothing() {
    let server = MockServer::start(|_, _| MockResponse::new(304));
    let dir = scratch("f6-304");
    let dest = dir.join("out.bin");
    plant_part(&dest, b"zzzz");
    let err = Client::new(cfg())
        .download_to_path(
            &server.url("/f"),
            &dest,
            DownloadOptions {
                resume: true,
                expected_sha256: Some(sha_hex(b"zzzz")),
                ..DownloadOptions::default()
            },
        )
        .expect_err("304 must not complete a download");
    assert!(matches!(err, DownloadError::Http(_)), "{err:?}");
    assert!(!dest.exists());
}
