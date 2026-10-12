//! Regression tests for the second independent read of 0.3.0
//! (docs/audits/2026-10-11-astra-rightkit-http-second-read.md). Each test names its finding and
//! was written from the finding's stated sequence before the fix. The async finding (4) is in
//! `second_read_async.rs`; the OAuth finding (6) is in `rightkit-oauth/tests/e2e.rs`.

mod common;

use std::io::Write;
use std::time::{Duration, Instant};

use common::RawServer;
use rightkit_http::testing::loopback_config;
use rightkit_http::*;

// ------------------------------------------------------------------ finding 5
/// A retryable 503 whose body never arrives. The pre-drain check passes (the backoff fits in
/// the deadline), the drain runs until the deadline, and the retry sleep then has to be
/// clamped to what is left, not slept in full.
#[test]
fn f5_blocking_retry_sleep_does_not_outlive_the_request_deadline() {
    let server = RawServer::start(|s, _, _| {
        let _ = s.write_all(
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 1\r\nConnection: close\r\n\r\n",
        );
        let _ = s.flush();
        std::thread::sleep(Duration::from_secs(8));
    });
    let c = Client::new(ClientConfig {
        retry: RetryPolicy {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(1000),
            max_backoff: Duration::from_millis(1000),
            multiplier: 1.0,
            jitter: 0.0,
            ..RetryPolicy::default()
        },
        request_timeout: Duration::from_secs(10),
        response_timeout: Some(Duration::from_secs(10)),
        ..loopback_config()
    });
    let started = Instant::now();
    let result = c.send_any(&Request::get(server.url("/")).deadline(Duration::from_millis(1500)));
    let took = started.elapsed();
    // The deadline error is the expected outcome; the last 503 is an acceptable one if the
    // clock left a sliver of budget. Only the elapsed time is the regression.
    match &result {
        Err(HttpError::Timeout { .. }) => {}
        Ok(resp) if resp.status == 503 => {}
        other => panic!("unexpected outcome: {other:?}"),
    }
    // Deadline 1.5 s. Before the fix: drain until 1.5 s, then the full 1 s backoff: 2.5 s.
    assert!(
        took < Duration::from_millis(2000),
        "slept past the deadline: {took:?}"
    );
}
