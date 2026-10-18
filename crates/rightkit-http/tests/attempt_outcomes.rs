//! Sanitized per-attempt outcomes (`Client::on_attempt_outcome`): once per
//! physical attempt including the terminal one, separate transport/status budgets, and
//! the rule that an uncertain POST is never auto-replayed.

use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rightkit_http::client::{AttemptDelivery, AttemptOutcome};
use rightkit_http::testing::{MockResponse, MockServer};
use rightkit_http::*;

fn fast(max: u32) -> RetryPolicy {
    RetryPolicy {
        max_attempts: max,
        initial_backoff: Duration::from_millis(5),
        max_backoff: Duration::from_millis(10),
        jitter: 0.0,
        ..RetryPolicy::default()
    }
}

fn collecting(retry: RetryPolicy) -> (ClientConfig, Arc<Mutex<Vec<AttemptOutcome>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let config = ClientConfig {
        retry,
        request_timeout: Duration::from_secs(5),
        response_timeout: Some(Duration::from_secs(5)),
        ..rightkit_http::testing::loopback_config()
    };
    (config, seen)
}

fn observed_client(config: ClientConfig, seen: &Arc<Mutex<Vec<AttemptOutcome>>>) -> Client {
    let sink = seen.clone();
    Client::new(config).on_attempt_outcome(move |o| sink.lock().unwrap().push(o.clone()))
}

#[cfg(feature = "async")]
fn observed_async_client(
    config: ClientConfig,
    seen: &Arc<Mutex<Vec<AttemptOutcome>>>,
) -> AsyncClient {
    let sink = seen.clone();
    AsyncClient::new(config)
        .unwrap()
        .on_attempt_outcome(move |o| sink.lock().unwrap().push(o.clone()))
}

fn closed_port_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    format!("http://127.0.0.1:{port}/x")
}

#[test]
fn terminal_success_reports_once_with_redacted_copied_headers() {
    let server = MockServer::start(|_, _| {
        MockResponse::text(200, "body-secret-9")
            .header("Set-Cookie", "session=abc123")
            .header("X-Api-Key", "k-secret")
            .header("X-Trace", "t-1")
    });
    let (config, seen) = collecting(RetryPolicy::none());
    let client = observed_client(config, &seen);
    let resp = client
        .send(&Request::get(server.url("/x?key=query-secret")))
        .unwrap();
    assert_eq!(resp.status, 200);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    let o = &seen[0];
    assert_eq!(o.attempt, 1);
    assert_eq!(o.method, Method::Get);
    assert_eq!(o.status, Some(200));
    assert_eq!(o.delivery, AttemptDelivery::Responded);
    assert_eq!(o.next_delay, None);
    assert!(o.error.is_none() && o.error_class.is_none());
    let header = |name: &str| {
        o.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    assert_eq!(header("set-cookie"), Some("[redacted]"));
    assert_eq!(header("x-api-key"), Some("[redacted]"));
    assert_eq!(header("x-trace"), Some("t-1"));
    let debug = format!("{o:?}");
    for secret in ["abc123", "k-secret", "query-secret", "body-secret-9"] {
        assert!(!debug.contains(secret), "{secret} leaked: {debug}");
    }
}

#[test]
fn every_attempt_reports_including_terminal_with_chosen_delays() {
    let server = MockServer::start(|_, i| {
        if i < 2 {
            MockResponse::text(503, "busy")
        } else {
            MockResponse::text(200, "ok")
        }
    });
    let (config, seen) = collecting(fast(3));
    let client = observed_client(config, &seen);
    let resp = client.send(&Request::get(server.url("/x"))).unwrap();
    assert_eq!(resp.attempts, 3);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 3);
    assert_eq!(
        seen.iter().map(|o| o.attempt).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(
        seen.iter().map(|o| o.status).collect::<Vec<_>>(),
        vec![Some(503), Some(503), Some(200)]
    );
    assert!(seen[0].next_delay.is_some() && seen[1].next_delay.is_some());
    assert_eq!(seen[2].next_delay, None);
}

#[test]
fn vetoed_retry_reports_no_second_outcome_and_sends_nothing() {
    let server = MockServer::start(|_, _| MockResponse::text(503, "busy"));
    let (config, seen) = collecting(fast(5));
    let client = observed_client(
        config.on_attempt(|e: &AttemptEvent| {
            if e.attempt >= 2 {
                Err("budget spent".to_string())
            } else {
                Ok(())
            }
        }),
        &seen,
    );
    let err = client.send(&Request::get(server.url("/x"))).unwrap_err();
    assert!(
        matches!(&err, HttpError::InvalidRequest(m) if m.contains("budget spent")),
        "{err:?}"
    );
    assert_eq!(server.request_count(), 1);
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[test]
fn transport_terminal_reports_typed_error_and_not_sent_delivery() {
    let (config, seen) = collecting(fast(2));
    let client = observed_client(config, &seen);
    let err = client.send(&Request::get(closed_port_url())).unwrap_err();
    assert_eq!(err.class(), ErrorClass::Transport);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "connect-phase failures retry: {seen:?}");
    for o in seen.iter() {
        assert_eq!(o.status, None);
        assert_eq!(o.error_class, Some(ErrorClass::Transport));
        assert_eq!(o.delivery, AttemptDelivery::NotSent);
        assert!(o.headers.is_empty());
    }
    assert!(seen[0].next_delay.is_some());
    assert_eq!(seen[1].next_delay, None);
}

#[test]
fn uncertain_post_is_never_auto_replayed() {
    // The server accepts the POST and stays silent past the response budget: the
    // request may have been acted on, so there must be exactly one attempt.
    let server =
        MockServer::start(|_, _| MockResponse::text(200, "late").delay(Duration::from_millis(600)));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let client = Client::new(ClientConfig {
        retry: fast(4),
        request_timeout: Duration::from_secs(5),
        response_timeout: Some(Duration::from_millis(100)),
        ..rightkit_http::testing::loopback_config()
    })
    .on_attempt_outcome(move |o: &AttemptOutcome| sink.lock().unwrap().push(o.clone()));
    let err = client
        .send(&Request::post(server.url("/paid")).body(b"charge".to_vec()))
        .unwrap_err();
    assert_eq!(err.class(), ErrorClass::Timeout, "{err:?}");
    assert_eq!(server.request_count(), 1);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].delivery, AttemptDelivery::Uncertain);
    assert_eq!(seen[0].error_class, Some(ErrorClass::Timeout));
    assert_eq!(seen[0].next_delay, None);
}

#[test]
fn separate_status_budget_stops_before_the_total_cap() {
    let server = MockServer::start(|_, _| MockResponse::text(503, "busy"));
    let mut policy = fast(10);
    policy.max_status_attempts = Some(2);
    let (config, seen) = collecting(policy);
    let client = observed_client(config, &seen);
    let _ = client.send(&Request::get(server.url("/x")));
    assert_eq!(server.request_count(), 2);
    assert_eq!(seen.lock().unwrap().len(), 2);
}

#[test]
fn separate_transport_budget_stops_before_the_total_cap() {
    let mut policy = fast(10);
    policy.max_transport_attempts = Some(2);
    let (config, seen) = collecting(policy);
    let client = observed_client(config, &seen);
    let _ = client.send(&Request::get(closed_port_url()));
    assert_eq!(seen.lock().unwrap().len(), 2);
}

#[test]
fn total_cap_binds_even_with_large_separate_budgets() {
    let server = MockServer::start(|_, _| MockResponse::text(503, "busy"));
    let mut policy = fast(3);
    policy.max_status_attempts = Some(10);
    policy.max_transport_attempts = Some(10);
    let (config, seen) = collecting(policy);
    let client = observed_client(config, &seen);
    let _ = client.send(&Request::get(server.url("/x")));
    assert_eq!(server.request_count(), 3);
    assert_eq!(seen.lock().unwrap().len(), 3);
}

#[cfg(feature = "async")]
mod async_outcomes {
    use super::*;

    #[tokio::test]
    async fn async_attempts_report_like_the_blocking_client() {
        let server = MockServer::start(|_, i| {
            if i == 0 {
                MockResponse::text(503, "busy")
            } else {
                MockResponse::text(200, "ok")
            }
        });
        let (config, seen) = collecting(fast(3));
        let client = observed_async_client(config, &seen);
        let resp = client.send(&Request::get(server.url("/x"))).await.unwrap();
        assert_eq!(resp.attempts, 2);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].status, Some(503));
        assert!(seen[0].next_delay.is_some());
        assert_eq!(seen[1].status, Some(200));
        assert_eq!(seen[1].next_delay, None);
        assert_eq!(seen[1].delivery, AttemptDelivery::Responded);
    }

    #[tokio::test]
    async fn async_cancel_mid_attempt_reports_one_uncertain_outcome() {
        let server = MockServer::start(|_, _| {
            MockResponse::text(200, "late").delay(Duration::from_millis(600))
        });
        let (config, seen) = collecting(fast(3));
        let client = observed_async_client(config, &seen);
        let token = CancelToken::new();
        let canceller = token.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            canceller.cancel();
        });
        let err = client
            .send(&Request::get(server.url("/x")).cancel(token))
            .await
            .unwrap_err();
        assert!(matches!(err, HttpError::Cancelled), "{err:?}");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert_eq!(seen[0].delivery, AttemptDelivery::Uncertain);
        assert_eq!(seen[0].error_class, Some(ErrorClass::Cancelled));
        assert_eq!(seen[0].next_delay, None);
    }
}

fn redirect_fixture(location: &str) -> (MockServer, ClientConfig, Arc<Mutex<Vec<AttemptOutcome>>>) {
    let location = location.to_string();
    let server = MockServer::start(move |_, _| {
        MockResponse::text(303, "accepted").header("Location", &location)
    });
    let (mut config, seen) = collecting(fast(4));
    config.max_redirects = 4;
    (server, config, seen)
}

fn assert_received_post(seen: &[AttemptOutcome], url: &str) {
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].url, url);
    assert_eq!(seen[0].method, Method::Post);
    assert_eq!(seen[0].attempt, 1);
    assert_eq!(seen[0].redirect_hop, 0);
    assert_eq!(seen[0].status, Some(303));
    assert_eq!(seen[0].delivery, AttemptDelivery::Responded);
    assert_eq!(seen[0].next_delay, None);
}

#[test]
fn post_redirect_refusal_retains_response_without_fictitious_not_sent_outcome() {
    for location in ["http://[", "http://169.254.169.254/latest"] {
        let (server, config, seen) = redirect_fixture(location);
        let url = server.url("/charge");
        assert!(observed_client(config, &seen)
            .send(&Request::post(&url))
            .is_err());
        assert_eq!(server.request_count(), 1);
        assert_received_post(&seen.lock().unwrap(), &url);
    }
}

#[test]
fn redirected_connect_failure_never_replays_received_post() {
    let (server, config, seen) = redirect_fixture(&closed_port_url());
    assert!(observed_client(config, &seen)
        .send(&Request::post(server.url("/charge")))
        .is_err());
    assert_eq!(server.request_count(), 1);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].delivery, AttemptDelivery::Responded);
    assert_eq!(seen[1].method, Method::Get);
    assert_eq!(seen[1].redirect_hop, 1);
    assert_eq!(seen[1].next_delay, None);
}

#[test]
fn redirect_hops_report_actual_method_url_and_preserve_credential_stripping() {
    let target = MockServer::start(|_, _| MockResponse::text(200, "ok"));
    let destination = target.url("/done?private=query-value");
    let source = MockServer::start(move |req, _| {
        if req.path() == "/start" {
            MockResponse::text(303, "accepted").header("Location", "/middle")
        } else {
            MockResponse::text(302, "next").header("Location", &destination)
        }
    });
    let (mut config, seen) = collecting(RetryPolicy::none());
    config.max_redirects = 4;
    observed_client(config, &seen)
        .send(&Request::post(source.url("/start")).bearer("credential-value"))
        .unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(
        seen.iter().map(|o| o.redirect_hop).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(
        seen.iter().map(|o| o.method).collect::<Vec<_>>(),
        vec![Method::Post, Method::Get, Method::Get]
    );
    assert_eq!(
        seen.iter().map(|o| o.url.clone()).collect::<Vec<_>>(),
        vec![
            source.url("/start"),
            source.url("/middle"),
            target.url("/done?[redacted]")
        ]
    );
    assert!(seen
        .iter()
        .all(|o| o.attempt == 1 && o.delivery == AttemptDelivery::Responded));
    assert_eq!(
        source.requests()[1].header("authorization"),
        Some("Bearer credential-value")
    );
    assert_eq!(target.requests()[0].header("authorization"), None);
    let debug = format!("{seen:?}");
    assert!(!debug.contains("credential-value") && !debug.contains("query-value"));
}

#[test]
fn zero_send_validation_produces_no_transport_outcome() {
    let (config, seen) = collecting(RetryPolicy::none());
    assert!(observed_client(config, &seen)
        .send(&Request::get("http://["))
        .is_err());
    assert!(seen.lock().unwrap().is_empty());
}

fn assert_redirect_connect_refusal_is_hop_local(seen: &[AttemptOutcome], server: &MockServer) {
    // The first hop was answered, so the original POST was delivered exactly once.
    assert_eq!(server.request_count(), 1);
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen[0].redirect_hop, 0);
    assert_eq!(seen[0].method, Method::Post);
    assert_eq!(seen[0].status, Some(303));
    assert_eq!(seen[0].delivery, AttemptDelivery::Responded);
    // The refused connect on hop 1 is truthfully NotSent for that hop alone.
    assert_eq!(seen[1].redirect_hop, 1);
    assert_eq!(seen[1].method, Method::Get);
    assert_eq!(seen[1].status, None);
    assert_eq!(seen[1].error_phase, Some(Phase::Redirect));
    assert_eq!(seen[1].delivery, AttemptDelivery::NotSent, "{:?}", seen[1]);
    // Still not retried: the chain may have delivered the original POST.
    assert_eq!(seen[1].attempt, 1);
    assert_eq!(seen[1].next_delay, None);
}

#[test]
fn redirect_hop_connect_refusal_is_hop_local_not_sent_but_chain_not_replayed() {
    let (server, config, seen) = redirect_fixture(&closed_port_url());
    let err = observed_client(config, &seen)
        .send(&Request::post(server.url("/charge")))
        .unwrap_err();
    // The returned error keeps the conservative chain verdict: not a pure connect failure.
    match err {
        HttpError::Transport {
            connect_phase,
            phase,
            ..
        } => {
            assert!(!connect_phase);
            assert_eq!(phase, Phase::Redirect);
        }
        other => panic!("expected a redirect-phase transport error, got {other:?}"),
    }
    assert_redirect_connect_refusal_is_hop_local(&seen.lock().unwrap(), &server);
}

#[cfg(feature = "async")]
#[tokio::test]
async fn async_redirect_hop_connect_refusal_is_hop_local_not_sent_but_chain_not_replayed() {
    let (server, config, seen) = redirect_fixture(&closed_port_url());
    let err = observed_async_client(config, &seen)
        .send(&Request::post(server.url("/charge")))
        .await
        .unwrap_err();
    match err {
        HttpError::Transport {
            connect_phase,
            phase,
            ..
        } => {
            assert!(!connect_phase);
            assert_eq!(phase, Phase::Redirect);
        }
        other => panic!("expected a redirect-phase transport error, got {other:?}"),
    }
    assert_redirect_connect_refusal_is_hop_local(&seen.lock().unwrap(), &server);
}

#[cfg(feature = "async")]
#[tokio::test]
async fn async_redirect_refusals_preserve_post_response() {
    for location in ["http://[", "http://169.254.169.254/latest"] {
        let (server, config, seen) = redirect_fixture(location);
        let url = server.url("/charge");
        assert!(observed_async_client(config, &seen)
            .send(&Request::post(&url))
            .await
            .is_err());
        assert_eq!(server.request_count(), 1);
        assert_received_post(&seen.lock().unwrap(), &url);
    }
}

#[cfg(feature = "async")]
#[tokio::test]
async fn async_redirected_connect_failure_never_replays_received_post() {
    let (server, config, seen) = redirect_fixture(&closed_port_url());
    assert!(observed_async_client(config, &seen)
        .send(&Request::post(server.url("/charge")))
        .await
        .is_err());
    assert_eq!(server.request_count(), 1);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].delivery, AttemptDelivery::Responded);
    assert_eq!(seen[1].redirect_hop, 1);
    assert_eq!(seen[1].method, Method::Get);
    assert_eq!(seen[1].next_delay, None);
}

#[cfg(feature = "async")]
#[tokio::test]
async fn async_hops_report_identity_and_strip_credentials() {
    let target = MockServer::start(|_, _| MockResponse::text(200, "ok"));
    let destination = target.url("/done?secret=query-value");
    let source = MockServer::start(move |_, _| {
        MockResponse::text(303, "accepted").header("Location", &destination)
    });
    let (mut config, seen) = collecting(RetryPolicy::none());
    config.max_redirects = 4;
    observed_async_client(config, &seen)
        .send(&Request::post(source.url("/start")).bearer("credential-value"))
        .await
        .unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].method, Method::Post);
    assert_eq!(seen[1].method, Method::Get);
    assert_eq!(seen[1].url, target.url("/done?[redacted]"));
    assert_eq!(seen[1].redirect_hop, 1);
    assert_eq!(target.requests()[0].header("authorization"), None);
    assert!(!format!("{seen:?}").contains("query-value"));
}

#[cfg(feature = "async")]
#[tokio::test]
async fn async_zero_send_validation_produces_no_transport_outcome() {
    let (config, seen) = collecting(RetryPolicy::none());
    assert!(observed_async_client(config, &seen)
        .send(&Request::get("http://["))
        .await
        .is_err());
    assert!(seen.lock().unwrap().is_empty());
}
