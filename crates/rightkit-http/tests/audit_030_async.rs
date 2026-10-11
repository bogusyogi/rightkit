//! Async regression tests for the independent read of 0.3.0 (findings 5 and 8).
#![cfg(feature = "async")]

mod common;

use std::io::Write;
use std::time::{Duration, Instant};

use common::RawServer;
use rightkit_http::testing::{loopback_config, MockResponse, MockServer};
use rightkit_http::*;

fn no_backoff(attempts: u32) -> RetryPolicy {
    RetryPolicy {
        max_attempts: attempts,
        initial_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
        jitter: 0.0,
        ..RetryPolicy::default()
    }
}

fn cfg() -> ClientConfig {
    ClientConfig {
        retry: no_backoff(3),
        request_timeout: Duration::from_secs(10),
        response_timeout: Some(Duration::from_secs(10)),
        ..loopback_config()
    }
}

/// 503 with `Content-Length: 1` whose body never arrives.
fn stalled_503() -> RawServer {
    RawServer::start(|s, _, _| {
        let _ = s.write_all(
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 1\r\nConnection: close\r\n\r\n",
        );
        let _ = s.flush();
        std::thread::sleep(Duration::from_secs(8));
    })
}

// ------------------------------------------------------------------ finding 5
#[tokio::test]
async fn f5_retry_drain_respects_the_request_deadline() {
    let server = stalled_503();
    let c = AsyncClient::new(cfg()).unwrap();
    let started = Instant::now();
    let _ = c
        .send_any(&Request::get(server.url("/")).deadline(Duration::from_millis(300)))
        .await;
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn f5_retry_drain_respects_recv_body_timeout() {
    let server = stalled_503();
    let c = AsyncClient::new(ClientConfig {
        recv_body_timeout: Some(Duration::from_millis(300)),
        ..cfg()
    })
    .unwrap();
    let started = Instant::now();
    let _ = c.send_any(&Request::get(server.url("/"))).await;
    assert!(
        started.elapsed() < Duration::from_millis(2500),
        "took {:?}",
        started.elapsed()
    );
}

// ------------------------------------------------------------------ finding 8
#[tokio::test]
async fn f8_async_response_debug_hides_configured_and_request_secrets() {
    let server = MockServer::start(|_, _| {
        MockResponse::text(200, "body")
            .header("X-Echo", "hunter2-config")
            .header("X-Echo2", "hunter2-request")
    });
    let c = AsyncClient::new(ClientConfig {
        redact_values: vec!["hunter2-config".into()],
        ..cfg()
    })
    .unwrap();
    let req = Request::get(server.url("/")).secret_header("X-Credential", "hunter2-request");
    let resp = c.send(&req).await.unwrap();
    let shown = format!("{resp:?}");
    assert!(!shown.contains("hunter2"), "{shown}");
    assert_eq!(resp.header("x-echo"), Some("hunter2-config"));
}
