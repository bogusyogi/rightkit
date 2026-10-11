//! Regression tests for the independent read of 0.3.0
//! (docs/audits/2026-10-11-astra-rightkit-http-0.3.0.md). Each test names its finding and
//! was written from the finding's stated sequence before the fix.

mod common;

use std::io::Write;

use sha2::Digest;
use std::time::Duration;

use common::RawServer;
use rightkit_http::testing::{loopback_config, MockResponse, MockServer};
use rightkit_http::*;

const GZIP_HELLO: [u8; 57] = [
    31, 139, 8, 0, 0, 0, 0, 0, 2, 255, 203, 72, 205, 201, 201, 87, 72, 175, 202, 44, 80, 72, 202,
    79, 169, 84, 72, 203, 47, 82, 40, 201, 72, 85, 72, 201, 47, 207, 203, 201, 79, 76, 81, 40, 73,
    45, 46, 1, 0, 90, 233, 8, 170, 37, 0, 0, 0,
];

fn cfg() -> ClientConfig {
    ClientConfig {
        retry: RetryPolicy::none(),
        request_timeout: Duration::from_secs(5),
        response_timeout: Some(Duration::from_secs(5)),
        ..loopback_config()
    }
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("a030-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A resumed prefix left by an earlier call is only extended when `expected_sha256` is set
/// (nothing else proves it unchanged). Tests whose outcome is decided before the hash is
/// checked pass a placeholder; the one that completes passes the real digest.
fn resume_opts() -> DownloadOptions<'static> {
    DownloadOptions {
        resume: true,
        expected_sha256: Some("0".repeat(64)),
        ..DownloadOptions::default()
    }
}

// ------------------------------------------------------------------ finding 4
#[test]
fn f4_short_resumed_range_is_not_a_complete_download() {
    let server = MockServer::start(|req, _| {
        assert_eq!(req.header("range"), Some("bytes=80-"));
        MockResponse::new(206)
            .header("Content-Range", "bytes 80-99/200")
            .body(vec![b'x'; 20])
    });
    let dir = scratch("f4");
    let dest = dir.join("out.bin");
    std::fs::write(dir.join("out.bin.part"), vec![b'x'; 80]).unwrap();
    let err = Client::new(cfg())
        .download_to_path(&server.url("/f"), &dest, resume_opts())
        .unwrap_err();
    assert!(
        matches!(
            err,
            DownloadError::LengthMismatch {
                expected: 200,
                actual: 100
            }
        ),
        "{err:?}"
    );
    assert!(
        !dest.exists(),
        "a truncated file must not be renamed into place"
    );
}

#[test]
fn f4_range_that_reaches_the_advertised_total_still_completes() {
    let server = MockServer::start(|_, _| {
        MockResponse::new(206)
            .header("Content-Range", "bytes 80-199/200")
            .body(vec![b'y'; 120])
    });
    let dir = scratch("f4ok");
    let dest = dir.join("out.bin");
    std::fs::write(dir.join("out.bin.part"), vec![b'x'; 80]).unwrap();
    let mut whole = vec![b'x'; 80];
    whole.extend_from_slice(&[b'y'; 120]);
    let digest: String = sha2::Sha256::digest(&whole)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let report = Client::new(cfg())
        .download_to_path(
            &server.url("/f"),
            &dest,
            DownloadOptions {
                expected_sha256: Some(digest),
                ..resume_opts()
            },
        )
        .unwrap();
    assert_eq!(report.bytes, 200);
}

// ------------------------------------------------------------------ finding 6
#[test]
fn f6_chunked_gzip_is_refused_by_a_download() {
    let server = RawServer::start(|s, _, _| {
        let mut out = b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
            .to_vec();
        out.extend_from_slice(format!("{:x}\r\n", GZIP_HELLO.len()).as_bytes());
        out.extend_from_slice(&GZIP_HELLO);
        out.extend_from_slice(b"\r\n0\r\n\r\n");
        let _ = s.write_all(&out);
    });
    let dir = scratch("f6");
    let dest = dir.join("out.bin");
    let result =
        Client::new(cfg()).download_to_path(&server.url("/f"), &dest, DownloadOptions::default());
    assert!(result.is_err(), "decoded bytes were accepted: {result:?}");
    assert!(!dest.exists());
}

#[test]
fn f6_plain_chunked_identity_download_still_works() {
    let server = RawServer::start(|s, _, _| {
        let _ = s.write_all(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
        );
    });
    let dir = scratch("f6ok");
    let dest = dir.join("out.bin");
    let report = Client::new(cfg())
        .download_to_path(&server.url("/f"), &dest, DownloadOptions::default())
        .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"hello");
    assert_eq!(report.bytes, 5);
}

// ------------------------------------------------------------------ finding 7
#[test]
fn f7_content_range_ending_at_u64_max_is_rejected_not_wrapped() {
    let server = RawServer::start(|s, _, _| {
        let _ = s.write_all(
            b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 1-18446744073709551615/*\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n2\r\nab\r\n0\r\n\r\n",
        );
    });
    let dir = scratch("f7");
    let dest = dir.join("out.bin");
    std::fs::write(dir.join("out.bin.part"), b"x").unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Client::new(cfg()).download_to_path(&server.url("/f"), &dest, resume_opts())
    }));
    match result {
        Ok(Err(_)) => {}
        Ok(Ok(r)) => panic!("hostile range accepted: {r:?}"),
        Err(_) => panic!("overflow panic on a hostile Content-Range"),
    }
    assert!(!dest.exists());
}

// ------------------------------------------------------------------ finding 8
#[test]
fn f8_response_debug_hides_configured_and_request_secrets() {
    let server = MockServer::start(|_, _| {
        MockResponse::text(200, "body")
            .header("X-Echo", "hunter2-config")
            .header("X-Echo2", "hunter2-request")
    });
    let c = Client::new(ClientConfig {
        redact_values: vec!["hunter2-config".into()],
        ..cfg()
    });
    let req = Request::get(server.url("/")).secret_header("X-Credential", "hunter2-request");
    let resp = c.send(&req).unwrap();
    let shown = format!("{resp:?}");
    assert!(!shown.contains("hunter2"), "{shown}");
    let head = c.send_headers(&req).unwrap();
    let shown = format!("{head:?}");
    assert!(!shown.contains("hunter2"), "{shown}");
    // Nothing else is lost: the real header values are still readable.
    assert_eq!(resp.header("x-echo"), Some("hunter2-config"));
}
