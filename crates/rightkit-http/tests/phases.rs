//! `Phase` on `Timeout` and `Transport` errors, from the blocking client.

use std::net::TcpListener;
use std::time::Duration;

use rightkit_http::testing::{loopback_config, MockResponse, MockServer};
use rightkit_http::*;

fn closed_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

fn config() -> ClientConfig {
    ClientConfig {
        retry: RetryPolicy::none(),
        request_timeout: Duration::from_secs(5),
        response_timeout: Some(Duration::from_secs(5)),
        ..loopback_config()
    }
}

fn transport(err: &HttpError) -> (Phase, bool) {
    match err {
        HttpError::Transport {
            phase,
            connect_phase,
            ..
        } => (*phase, *connect_phase),
        other => panic!("expected Transport, got {other:?}"),
    }
}

#[test]
fn connection_refused_is_the_connect_phase() {
    let err = Client::new(config())
        .send(&Request::get(format!(
            "http://127.0.0.1:{}/",
            closed_port()
        )))
        .unwrap_err();
    assert_eq!(transport(&err), (Phase::Connect, true));
    assert_eq!(err.phase(), Some(Phase::Connect));
}

#[test]
fn waiting_for_headers_is_the_headers_phase() {
    let server =
        MockServer::start(|_, _| MockResponse::text(200, "late").delay(Duration::from_millis(900)));
    let c = Client::new(ClientConfig {
        response_timeout: Some(Duration::from_millis(150)),
        ..config()
    });
    let err = c.send(&Request::get(server.url("/"))).unwrap_err();
    assert!(
        matches!(
            err,
            HttpError::Timeout {
                phase: Phase::Headers,
                ..
            }
        ),
        "{err:?}"
    );
}

#[test]
fn the_request_wide_budget_is_the_headers_phase_before_any_response() {
    let server =
        MockServer::start(|_, _| MockResponse::text(200, "late").delay(Duration::from_millis(900)));
    let c = Client::new(config());
    let err = c
        .send(&Request::get(server.url("/")).timeout(Duration::from_millis(200)))
        .unwrap_err();
    assert!(
        matches!(
            err,
            HttpError::Timeout {
                phase: Phase::Headers,
                ..
            }
        ),
        "{err:?}"
    );
}

#[test]
fn reading_the_body_is_the_body_phase() {
    let server = MockServer::start(|_, _| {
        MockResponse::new(200).chunks(vec![
            (Duration::ZERO, b"fast".to_vec()),
            (Duration::from_millis(700), b"late".to_vec()),
        ])
    });
    let c = Client::new(ClientConfig {
        recv_body_timeout: Some(Duration::from_millis(150)),
        ..config()
    });
    let err = c.send(&Request::get(server.url("/"))).unwrap_err();
    assert!(
        matches!(
            err,
            HttpError::Timeout {
                phase: Phase::Body,
                ..
            }
        ),
        "{err:?}"
    );

    // A body cut off by the server is a transport error in the body phase.
    let cut = MockServer::start(|_, _| {
        let mut r = MockResponse::new(200);
        r.body = rightkit_http::testing::MockBody::TruncatedAfter(b"part".to_vec());
        r
    });
    let err = Client::new(config())
        .send(&Request::get(cut.url("/")))
        .unwrap_err();
    assert_eq!(err.phase(), Some(Phase::Body), "{err:?}");
    assert!(matches!(
        err,
        HttpError::Transport {
            connect_phase: false,
            ..
        }
    ));
}

#[test]
fn failures_on_a_redirect_hop_are_the_redirect_phase() {
    let closed = closed_port();
    let to_closed = MockServer::start(move |_, _| {
        MockResponse::new(302).header("Location", &format!("http://127.0.0.1:{closed}/"))
    });
    let c = Client::new(ClientConfig {
        max_redirects: 3,
        ..config()
    });
    // The second hop cannot connect. It is a redirect failure, and no longer a pure connect
    // failure, because the first server was reached.
    let err = c.send(&Request::get(to_closed.url("/"))).unwrap_err();
    assert_eq!(transport(&err), (Phase::Redirect, false));

    // A slow second hop.
    let slow =
        MockServer::start(|_, _| MockResponse::text(200, "late").delay(Duration::from_millis(900)));
    let slow_url = slow.url("/");
    let redirector =
        MockServer::start(move |_, _| MockResponse::new(302).header("Location", &slow_url));
    let c = Client::new(ClientConfig {
        max_redirects: 3,
        response_timeout: Some(Duration::from_millis(150)),
        ..config()
    });
    let err = c.send(&Request::get(redirector.url("/"))).unwrap_err();
    assert!(
        matches!(
            err,
            HttpError::Timeout {
                phase: Phase::Redirect,
                ..
            }
        ),
        "{err:?}"
    );

    // An exhausted limit.
    let looping = MockServer::start(|_, _| MockResponse::new(302).header("Location", "/loop"));
    let c = Client::new(ClientConfig {
        max_redirects: 2,
        ..config()
    });
    let err = c.send(&Request::get(looping.url("/loop"))).unwrap_err();
    assert_eq!(transport(&err), (Phase::Redirect, false));
    assert_eq!(err.to_string(), "transport error: too many redirects");
}

#[test]
fn a_connect_failure_on_the_first_request_stays_retryable_for_post() {
    // `connect_phase` keeps its retry meaning: nothing reached a server, so POST retries.
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let seen = attempts.clone();
    let c = Client::new(ClientConfig {
        retry: RetryPolicy {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(5),
            max_backoff: Duration::from_millis(5),
            jitter: 0.0,
            ..RetryPolicy::default()
        },
        on_retry: Some(std::sync::Arc::new(move |_| {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })),
        ..config()
    });
    let err = c
        .send(&Request::post(format!(
            "http://127.0.0.1:{}/",
            closed_port()
        )))
        .unwrap_err();
    assert_eq!(err.phase(), Some(Phase::Connect));
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[test]
fn only_timeout_and_transport_carry_a_phase() {
    let c = Client::new(config());
    let err = c.send(&Request::get("not a url")).unwrap_err();
    assert!(matches!(err, HttpError::InvalidRequest(_)));
    assert_eq!(err.phase(), None);
    let err = Client::new(ClientConfig::default())
        .send(&Request::get("http://10.0.0.1/"))
        .unwrap_err();
    assert_eq!(err.phase(), None);
    assert_eq!(Phase::Redirect.as_str(), "redirect");
}
