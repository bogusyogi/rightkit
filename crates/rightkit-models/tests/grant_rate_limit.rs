//! Typed grant errors: HTTP 429 is `GrantError::RateLimited` with the server's `Retry-After`
//! (delay-seconds or HTTP-date), and the grant POST is not replayed. Loopback mock server in
//! the same style as `grant_contract.rs`.

use rightkit_http::{AddressPolicy, Client, ClientConfig, RetryPolicy};
use rightkit_models::suite::grant::{try_acquire_grant_with_client, GrantError, ModelGrantRequest};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Serves `status` with an optional `Retry-After` header to every request and counts them.
fn serve(status: u16, retry_after: Option<&'static str>) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            counter.fetch_add(1, Ordering::SeqCst);
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0usize;
            loop {
                let mut h = String::new();
                reader.read_line(&mut h).unwrap();
                if h.trim().is_empty() {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0u8; length];
            reader.read_exact(&mut body).unwrap();
            let header = retry_after
                .map(|v| format!("Retry-After: {v}\r\n"))
                .unwrap_or_default();
            let mut out = stream;
            let _ = write!(
                out,
                "HTTP/1.1 {status} X\r\n{header}Content-Length: 2\r\nConnection: close\r\n\r\n{{}}"
            );
        }
    });
    (base, hits)
}

fn client() -> Client {
    Client::new(ClientConfig {
        // No retries: a replayed grant POST would show up as extra hits.
        retry: RetryPolicy::none(),
        address_policy: AddressPolicy::public_web()
            .allow_if(vec!["127.0.0.1".to_string()], |ip| ip.is_loopback()),
        ..ClientConfig::default()
    })
}

fn call(base: &str) -> Result<rightkit_models::suite::grant::ModelGrant, GrantError> {
    try_acquire_grant_with_client(
        &client(),
        ModelGrantRequest {
            worker_base: base,
            route: "/models/demo",
            model: "demo",
            platform: "macos-arm64",
            sha256: &"a".repeat(64),
            device_id: "device-1",
        },
    )
}

#[test]
fn http_429_with_delay_seconds_is_rate_limited_with_that_delay() {
    let (base, hits) = serve(429, Some("7"));
    match call(&base).unwrap_err() {
        GrantError::RateLimited {
            retry_after,
            message,
        } => {
            assert_eq!(retry_after, Some(Duration::from_secs(7)));
            assert!(
                message.starts_with("model grant request failed"),
                "{message}"
            );
            assert!(message.contains("429"), "{message}");
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
    // The grant POST is never replayed after a 429.
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[test]
fn http_429_with_http_date_yields_the_delay_until_that_date() {
    // A date far in the future: the delay is positive and large.
    let (base, _) = serve(429, Some("Fri, 31 Dec 2100 23:59:59 GMT"));
    let err = call(&base).unwrap_err();
    let delay = err.retry_after().expect("Retry-After date should parse");
    assert!(delay > Duration::from_secs(3600), "{delay:?}");
}

#[test]
fn http_429_with_a_past_date_means_retry_now() {
    let (base, _) = serve(429, Some("Sun, 06 Nov 1994 08:49:37 GMT"));
    assert_eq!(call(&base).unwrap_err().retry_after(), Some(Duration::ZERO));
}

#[test]
fn http_429_without_retry_after_has_no_delay() {
    let (base, _) = serve(429, None);
    let err = call(&base).unwrap_err();
    assert!(
        matches!(
            err,
            GrantError::RateLimited {
                retry_after: None,
                ..
            }
        ),
        "{err:?}"
    );
}

#[test]
fn other_error_statuses_stay_failed_with_the_unchanged_message() {
    let (base, _) = serve(503, Some("7"));
    match call(&base).unwrap_err() {
        GrantError::Failed(message) => {
            assert_eq!(
                message,
                "model grant request failed: HTTP 503 after 1 attempt(s)"
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}
