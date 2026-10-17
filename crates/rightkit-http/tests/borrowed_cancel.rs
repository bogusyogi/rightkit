//! A borrowed `&AtomicBool` cancels a buffered call without an `Arc`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rightkit_http::testing::{MockResponse, MockServer};
use rightkit_http::*;

#[test]
fn a_set_flag_cancels_before_anything_is_sent() {
    let server = MockServer::start(|_, _| MockResponse::text(200, "ok"));
    let client = Client::new(rightkit_http::testing::loopback_config());
    let flag = AtomicBool::new(true);
    let err = client
        .send_with_flag(&Request::get(server.url("/x")), &flag)
        .unwrap_err();
    assert!(matches!(err, HttpError::Cancelled), "{err:?}");
    assert_eq!(server.request_count(), 0);
}

#[test]
fn a_flag_set_during_the_retry_sleep_cancels_the_call() {
    let server = MockServer::start(|_, _| MockResponse::text(503, "busy"));
    let client = Client::new(ClientConfig {
        retry: RetryPolicy {
            max_attempts: 3,
            initial_backoff: Duration::from_secs(5),
            max_backoff: Duration::from_secs(5),
            jitter: 0.0,
            ..RetryPolicy::default()
        },
        ..rightkit_http::testing::loopback_config()
    });
    let flag = AtomicBool::new(false);
    std::thread::scope(|s| {
        s.spawn(|| {
            std::thread::sleep(Duration::from_millis(200));
            flag.store(true, Ordering::SeqCst);
        });
        let started = std::time::Instant::now();
        let err = client
            .send_with_flag(&Request::get(server.url("/x")), &flag)
            .unwrap_err();
        assert!(matches!(err, HttpError::Cancelled), "{err:?}");
        assert!(started.elapsed() < Duration::from_secs(4));
    });
}

#[test]
fn an_unset_flag_changes_nothing() {
    let server = MockServer::start(|_, _| MockResponse::text(200, "fine"));
    let client = Client::new(rightkit_http::testing::loopback_config());
    let flag = AtomicBool::new(false);
    let resp = client
        .send_with_flag(&Request::get(server.url("/x")), &flag)
        .unwrap();
    assert_eq!(resp.status, 200);
}
