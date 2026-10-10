//! Request-level policy: per-request body limits, stream timeouts, retry-unsafe
//! overrides, RetryPolicy::none() (no sleeps), cancellation, secret redaction and
//! the per-call deadline.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use rightkit_http::testing::{MockResponse, MockServer};
use rightkit_http::*;

fn client(retry: RetryPolicy) -> Client {
    Client::new(ClientConfig {
        retry,
        request_timeout: Duration::from_secs(5),
        response_timeout: Some(Duration::from_secs(5)),
        ..rightkit_http::testing::loopback_config()
    })
}

fn fast(max: u32) -> RetryPolicy {
    RetryPolicy {
        max_attempts: max,
        initial_backoff: Duration::from_millis(10),
        max_backoff: Duration::from_millis(20),
        jitter: 0.0,
        ..RetryPolicy::default()
    }
}

fn cancel_after(token: &CancelToken, after: Duration) {
    let t = token.clone();
    thread::spawn(move || {
        thread::sleep(after);
        t.cancel();
    });
}

#[test]
fn per_request_max_response_bytes_overrides_config() {
    let server = MockServer::start(|_req, _i| MockResponse::new(200).body(vec![7u8; 100]));
    let c = client(RetryPolicy::none());
    let err = c
        .send(&Request::get(server.url("/x")).max_response_bytes(10))
        .unwrap_err();
    assert!(
        matches!(err, HttpError::BodyTooLarge { limit: 10 }),
        "{err:?}"
    );
    let ok = c.send(&Request::get(server.url("/x"))).unwrap();
    assert_eq!(ok.body.len(), 100);
}

#[test]
fn stream_honours_request_timeout() {
    let server = MockServer::start(|_req, _i| {
        MockResponse::new(200)
            .delay(Duration::from_secs(2))
            .body(b"late".to_vec())
    });
    let started = Instant::now();
    let result = client(RetryPolicy::none())
        .stream(&Request::get(server.url("/slow")).timeout(Duration::from_millis(200)));
    assert!(result.is_err());
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn retry_unsafe_statuses_false_stops_post_status_retries() {
    let server = MockServer::start(|_req, _i| MockResponse::new(503));
    let c = client(fast(3));
    let err = c
        .send(&Request::post(server.url("/paid")).retry_unsafe_statuses(false))
        .unwrap_err();
    assert_eq!(err.status(), Some(503));
    assert_eq!(server.request_count(), 1);
}

#[test]
fn retry_unsafe_statuses_default_retries_post() {
    let server = MockServer::start(|_req, _i| MockResponse::new(503));
    let c = client(fast(3));
    let _ = c.send(&Request::post(server.url("/paid")));
    assert_eq!(server.request_count(), 3);
}

#[test]
fn retry_policy_none_disables_unsafe_status_retries_and_sleeps() {
    let server = MockServer::start(|_req, _i| MockResponse::new(503));
    // Default backoff is 500 ms; RetryPolicy::none() must not sleep at all.
    let started = Instant::now();
    let c = client(RetryPolicy::none());
    let _ = c.send(&Request::post(server.url("/paid")).retry_unsafe_statuses(true));
    let _ = c.send(&Request::get(server.url("/probe")));
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(server.request_count(), 2);
}

#[test]
fn cancel_before_call_sends_nothing() {
    let server = MockServer::start(|_req, _i| MockResponse::new(200));
    let token = CancelToken::new();
    token.cancel();
    let err = client(RetryPolicy::none())
        .send(&Request::get(server.url("/x")).cancel(token))
        .unwrap_err();
    assert!(matches!(err, HttpError::Cancelled), "{err:?}");
    assert_eq!(server.request_count(), 0);
}

#[test]
fn cancel_interrupts_retry_sleep() {
    let server = MockServer::start(|_req, _i| MockResponse::new(503));
    let policy = RetryPolicy {
        max_attempts: 5,
        initial_backoff: Duration::from_secs(5),
        max_backoff: Duration::from_secs(5),
        jitter: 0.0,
        ..RetryPolicy::default()
    };
    let token = CancelToken::new();
    cancel_after(&token, Duration::from_millis(200));
    let started = Instant::now();
    let err = client(policy)
        .send(&Request::get(server.url("/x")).cancel(token))
        .unwrap_err();
    assert!(matches!(err, HttpError::Cancelled), "{err:?}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(server.request_count(), 1);
}

#[test]
fn cancel_stops_buffered_body_read() {
    let server = MockServer::start(|_req, _i| {
        MockResponse::new(200).chunks(vec![
            (Duration::ZERO, vec![1u8; 10]),
            (Duration::from_millis(300), vec![2u8; 10]),
            (Duration::from_millis(300), vec![3u8; 10]),
        ])
    });
    let token = CancelToken::new();
    cancel_after(&token, Duration::from_millis(100));
    let err = client(RetryPolicy::none())
        .send(&Request::get(server.url("/x")).cancel(token))
        .unwrap_err();
    assert!(matches!(err, HttpError::Cancelled), "{err:?}");
}

#[test]
fn cancel_stops_stream_reads_with_typed_io_error() {
    let server = MockServer::start(|_req, _i| {
        MockResponse::new(200).chunks(vec![
            (Duration::ZERO, vec![1u8; 10]),
            (Duration::from_millis(300), vec![2u8; 10]),
        ])
    });
    let token = CancelToken::new();
    let resp = client(RetryPolicy::none())
        .stream(&Request::get(server.url("/x")).cancel(token.clone()))
        .unwrap();
    cancel_after(&token, Duration::from_millis(100));
    let mut reader = resp.into_reader();
    let mut out = Vec::new();
    let err = std::io::Read::read_to_end(&mut reader, &mut out).unwrap_err();
    assert!(is_cancelled_io(&err), "{err:?}");
    // The first chunk is always read; the second read may complete before the flag is seen.
    assert!((10..=20).contains(&out.len()), "{}", out.len());
}

#[test]
fn secret_header_is_sent_but_never_in_error_text() {
    let secret = "sk-live-SECRET-42";
    let server = MockServer::start(|req, _i| {
        let echoed = req.header("x-api-key").unwrap_or("").to_string();
        MockResponse::new(401).body(format!("rejected key {echoed}").into_bytes())
    });
    let err = client(RetryPolicy::none())
        .send(&Request::get(server.url("/x")).secret_header("X-Api-Key", secret))
        .unwrap_err();
    assert_eq!(server.requests()[0].header("x-api-key"), Some(secret));
    match &err {
        HttpError::Status { body, .. } => {
            assert!(body.contains("[redacted]"), "{body}");
            assert!(!body.contains(secret), "{body}");
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(!format!("{err}").contains(secret));
    assert!(!format!("{err:?}").contains(secret));
}

#[test]
fn configured_redact_values_scrub_error_text_and_retry_events() {
    let server = MockServer::start(|_req, _i| {
        MockResponse::new(503).body(b"upstream internal-host-7 unavailable".to_vec())
    });
    let events = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let sink = events.clone();
    let c = Client::new(
        ClientConfig {
            retry: fast(2),
            on_retry: Some(Arc::new(move |e: &RetryEvent| {
                sink.lock()
                    .unwrap()
                    .push(format!("{} | {}", e.url, e.reason));
            })),
            ..rightkit_http::testing::loopback_config()
        }
        .redact_values(vec!["internal-host-7".to_string()]),
    );
    let err = c
        .send(&Request::get(server.url("/internal-host-7/x")))
        .unwrap_err();
    match &err {
        HttpError::Status { body, .. } => {
            assert_eq!(body, "upstream [redacted] unavailable");
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(!format!("{err:?}").contains("internal-host-7"));
    let seen = events.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(!seen[0].contains("internal-host-7"), "{seen:?}");
    assert!(seen[0].contains("[redacted]"), "{seen:?}");
}

#[test]
fn deadline_bounds_attempts_and_retry_sleeps() {
    let server = MockServer::start(|_req, _i| MockResponse::new(503).header("Retry-After", "1"));
    let policy = RetryPolicy {
        max_attempts: 10,
        initial_backoff: Duration::from_secs(1),
        max_backoff: Duration::from_secs(8),
        jitter: 0.0,
        ..RetryPolicy::default()
    };
    let started = Instant::now();
    let err = client(policy)
        .send(&Request::get(server.url("/x")).deadline(Duration::from_millis(1500)))
        .unwrap_err();
    let elapsed = started.elapsed();
    assert_eq!(err.status(), Some(503), "{err:?}");
    assert_eq!(
        server.request_count(),
        2,
        "second sleep (2 s) would pass the deadline"
    );
    assert!(elapsed < Duration::from_millis(1800), "{elapsed:?}");
}

#[test]
fn deadline_does_not_change_per_attempt_default() {
    let server = MockServer::start(|_req, _i| MockResponse::new(200).body(b"ok".to_vec()));
    let body = client(RetryPolicy::none())
        .send(&Request::get(server.url("/x")).deadline(Duration::from_secs(300)))
        .unwrap()
        .body;
    assert_eq!(body, b"ok");
}
