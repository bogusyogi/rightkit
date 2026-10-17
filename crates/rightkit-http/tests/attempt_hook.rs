//! The per-attempt hook runs below the retry loop: once per attempt, and it can refuse one.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rightkit_http::testing::{MockResponse, MockServer};
use rightkit_http::*;

fn config(max: u32) -> ClientConfig {
    ClientConfig {
        retry: RetryPolicy {
            max_attempts: max,
            initial_backoff: Duration::from_millis(5),
            max_backoff: Duration::from_millis(10),
            jitter: 0.0,
            ..RetryPolicy::default()
        },
        ..rightkit_http::testing::loopback_config()
    }
}

#[test]
fn hook_runs_once_per_attempt_with_the_attempt_number() {
    let server = MockServer::start(|_, _| MockResponse::text(503, "busy"));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    let client = Client::new(config(3).on_attempt(move |e: &AttemptEvent| {
        s.lock().unwrap().push((e.attempt, e.max_attempts));
        Ok(())
    }));
    let _ = client.send(&Request::get(server.url("/x")));
    assert_eq!(*seen.lock().unwrap(), vec![(1, 3), (2, 3), (3, 3)]);
    assert_eq!(server.request_count(), 3);
}

#[test]
fn refusing_a_retry_stops_the_call_before_it_is_sent() {
    let server = MockServer::start(|_, _| MockResponse::text(503, "busy"));
    let client = Client::new(config(5).on_attempt(|e: &AttemptEvent| {
        if e.attempt >= 2 {
            Err("budget spent".to_string())
        } else {
            Ok(())
        }
    }));
    let err = client.send(&Request::get(server.url("/x"))).unwrap_err();
    assert!(
        matches!(&err, HttpError::InvalidRequest(m) if m.contains("budget spent")),
        "{err:?}"
    );
    assert_eq!(server.request_count(), 1);
}
