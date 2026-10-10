//! Headers-only calls and `stream_any`: status and headers for any status, body never read,
//! with the address policy on every hop.

use std::io::Read;
use std::time::{Duration, Instant};

use rightkit_http::testing::{loopback_config, loopback_policy, MockResponse, MockServer};
use rightkit_http::*;

fn client(max_redirects: u32, retry: RetryPolicy) -> Client {
    Client::new(ClientConfig {
        max_redirects,
        retry,
        request_timeout: Duration::from_secs(5),
        response_timeout: Some(Duration::from_secs(5)),
        ..loopback_config()
    })
}

/// A body that would take far longer than any assertion here waits.
fn slow_body() -> MockResponse {
    MockResponse::new(200)
        .header("X-Seen", "headers")
        .chunks(vec![
            (Duration::ZERO, b"first".to_vec()),
            (Duration::from_secs(4), b"never-awaited".to_vec()),
        ])
}

#[test]
fn any_status_returns_status_and_headers() {
    let server = MockServer::start(|req, _| match req.path() {
        "/missing" => MockResponse::text(404, "nope").header("X-Why", "gone"),
        "/boom" => MockResponse::text(503, "down").header("Retry-After", "7"),
        "/bad" => MockResponse::text(400, "bad input"),
        _ => MockResponse::text(200, "fine").header("ETag", "\"v1\""),
    });
    let c = client(0, RetryPolicy::none());
    let head = c
        .send_headers(&Request::get(server.url("/missing")))
        .unwrap();
    assert_eq!(head.status, 404);
    assert_eq!(head.header("x-why"), Some("gone"));
    assert_eq!(head.hops, 0);
    assert_eq!(head.final_url, server.url("/missing"));
    assert_eq!(head.attempts, 1);

    let head = c.send_headers(&Request::get(server.url("/boom"))).unwrap();
    assert_eq!(head.status, 503);
    assert_eq!(head.header("retry-after"), Some("7"));
    assert_eq!(
        c.send_headers(&Request::get(server.url("/bad")))
            .unwrap()
            .status,
        400
    );
    let ok = c.send_headers(&Request::get(server.url("/ok"))).unwrap();
    assert_eq!((ok.status, ok.header("etag")), (200, Some("\"v1\"")));
}

#[test]
fn retryable_statuses_are_retried_before_the_last_one_is_returned() {
    let server = MockServer::start(|_, _| MockResponse::new(503));
    let c = client(
        0,
        RetryPolicy {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(5),
            max_backoff: Duration::from_millis(5),
            jitter: 0.0,
            ..RetryPolicy::default()
        },
    );
    let head = c.send_headers(&Request::get(server.url("/"))).unwrap();
    assert_eq!(head.status, 503);
    assert_eq!(head.attempts, 3);
    assert_eq!(server.request_count(), 3);
}

#[test]
fn the_body_is_not_read() {
    let server = MockServer::start(|_, _| slow_body());
    let c = client(0, RetryPolicy::none());
    let started = Instant::now();
    let head = c.send_headers(&Request::get(server.url("/"))).unwrap();
    assert_eq!(head.status, 200);
    assert_eq!(head.header("x-seen"), Some("headers"));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "send_headers waited for the body: {:?}",
        started.elapsed()
    );

    // The same through the request flag on send_any, which returns an empty body.
    let started = Instant::now();
    let resp = c
        .send_any(&Request::get(server.url("/")).headers_only())
        .unwrap();
    assert_eq!(resp.status, 200);
    assert!(resp.body.is_empty());
    assert!(started.elapsed() < Duration::from_secs(2));
    // And `send` with a non-2xx status reports it without a body.
    let err = c
        .send(&Request::get(server.url("/")).headers_only())
        .map(|r| r.status);
    assert_eq!(err.unwrap(), 200);
}

#[test]
fn redirects_off_returns_the_3xx_with_its_location() {
    let server = MockServer::start(|_, _| MockResponse::new(302).header("Location", "/next"));
    let head = client(0, RetryPolicy::none())
        .send_headers(&Request::get(server.url("/start")))
        .unwrap();
    assert_eq!(head.status, 302);
    assert_eq!(head.header("location"), Some("/next"));
    assert_eq!(head.hops, 0);
    assert_eq!(head.final_url, server.url("/start"));
    assert_eq!(server.request_count(), 1);
}

#[test]
fn followed_redirects_report_the_final_url_and_hops() {
    let server = MockServer::start(|req, _| match req.path() {
        "/a" => MockResponse::new(301).header("Location", "/b?token=in-the-query"),
        "/b" => MockResponse::new(302).header("Location", "/c"),
        _ => MockResponse::text(200, "end"),
    });
    let head = client(5, RetryPolicy::none())
        .send_headers(&Request::get(server.url("/a")))
        .unwrap();
    assert_eq!(head.status, 200);
    assert_eq!(head.hops, 2);
    assert_eq!(head.final_url, server.url("/c"));
    assert_eq!(server.request_count(), 3);

    // The final URL is the caller's data; its Debug redacts the query.
    let server = MockServer::start(|req, _| match req.path() {
        "/a" => MockResponse::new(302).header("Location", "/b?token=in-the-query"),
        _ => MockResponse::text(200, "end"),
    });
    let head = client(5, RetryPolicy::none())
        .send_headers(&Request::get(server.url("/a")))
        .unwrap();
    assert!(head.final_url.ends_with("/b?token=in-the-query"));
    assert!(!format!("{head:?}").contains("in-the-query"));
    assert!(format!("{head:?}").contains("?[redacted]"));
}

#[test]
fn a_redirect_limit_still_applies() {
    let server = MockServer::start(|_, _| MockResponse::new(302).header("Location", "/loop"));
    let err = client(2, RetryPolicy::none())
        .send_headers(&Request::get(server.url("/loop")))
        .unwrap_err();
    assert_eq!(err.phase(), Some(Phase::Redirect));
    // With the limit returning the last 3xx instead.
    let c = Client::new(ClientConfig {
        max_redirects: 2,
        max_redirects_will_error: false,
        retry: RetryPolicy::none(),
        ..loopback_config()
    });
    let head = c.send_headers(&Request::get(server.url("/loop"))).unwrap();
    assert_eq!(head.status, 302);
    assert_eq!(head.hops, 2);
}

#[test]
fn every_hop_passes_the_address_policy() {
    let target = MockServer::start(|_, _| MockResponse::text(200, "target"));
    // Redirect to the metadata endpoint.
    let a = MockServer::start(|_, _| {
        MockResponse::new(302).header("Location", "http://169.254.169.254/latest/meta-data/")
    });
    let err = client(3, RetryPolicy::none())
        .send_headers(&Request::get(a.url("/")))
        .unwrap_err();
    assert!(
        matches!(
            err,
            HttpError::AddressDenied {
                class: Some(AddressClass::LinkLocal),
                ..
            }
        ),
        "{err:?}"
    );

    // Redirect to loopback under a host that is not listed.
    let port = target.port();
    let a = MockServer::start(move |_, _| {
        MockResponse::new(302).header("Location", &format!("http://localhost:{port}/"))
    });
    let c = Client::new(ClientConfig {
        address_policy: AddressPolicy::default()
            .allow_if(vec!["127.0.0.1".into()], |ip| ip.is_loopback()),
        max_redirects: 3,
        retry: RetryPolicy::none(),
        ..ClientConfig::default()
    });
    let err = c.send_headers(&Request::get(a.url("/"))).unwrap_err();
    assert!(
        matches!(
            err,
            HttpError::AddressDenied {
                class: Some(AddressClass::Loopback),
                ..
            }
        ),
        "{err:?}"
    );
    assert_eq!(target.request_count(), 0);

    // The first request is checked too.
    let err = Client::new(ClientConfig::default())
        .send_headers(&Request::get(target.url("/")))
        .unwrap_err();
    assert!(matches!(err, HttpError::AddressDenied { .. }), "{err:?}");
    assert_eq!(target.request_count(), 0);
    let _ = loopback_policy();
}

#[test]
fn cancellation_applies() {
    let server = MockServer::start(|_, _| MockResponse::new(200));
    let token = CancelToken::new();
    token.cancel();
    let err = client(0, RetryPolicy::none())
        .send_headers(&Request::get(server.url("/")).cancel(token))
        .unwrap_err();
    assert!(matches!(err, HttpError::Cancelled), "{err:?}");
    assert_eq!(server.request_count(), 0);
}

#[test]
fn stream_any_returns_non_2xx_with_headers_and_a_readable_body() {
    let server = MockServer::start(|req, _| match req.path() {
        "/auth" => {
            MockResponse::text(401, "who are you").header("WWW-Authenticate", "Bearer realm=\"x\"")
        }
        _ => MockResponse::text(200, "fine"),
    });
    let c = client(0, RetryPolicy::none());
    let resp = c.stream_any(&Request::get(server.url("/auth"))).unwrap();
    assert_eq!(resp.status, 401);
    assert_eq!(resp.header("www-authenticate"), Some("Bearer realm=\"x\""));
    let mut body = String::new();
    resp.into_reader().read_to_string(&mut body).unwrap();
    assert_eq!(body, "who are you");

    // `stream` keeps failing closed on non-2xx.
    let err = c.stream(&Request::get(server.url("/auth"))).err().unwrap();
    assert_eq!(err.status(), Some(401));

    // 2xx works the same through both.
    let mut ok = String::new();
    c.stream_any(&Request::get(server.url("/ok")))
        .unwrap()
        .into_reader()
        .read_to_string(&mut ok)
        .unwrap();
    assert_eq!(ok, "fine");
}

#[test]
fn stream_any_honours_the_request_timeout() {
    let server = MockServer::start(|_, _| {
        MockResponse::new(200).chunks(vec![
            (Duration::ZERO, b"one".to_vec()),
            (Duration::from_millis(900), b"two".to_vec()),
        ])
    });
    let c = client(0, RetryPolicy::none());
    let resp = c
        .stream_any(&Request::get(server.url("/")).timeout(Duration::from_millis(300)))
        .unwrap();
    let mut reader = resp.into_reader();
    let mut buf = [0u8; 3];
    reader.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"one");
    assert!(reader.read_to_end(&mut Vec::new()).is_err());
}
