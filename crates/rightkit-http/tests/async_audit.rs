//! Async regression tests for the adversarial read of 0.2.6.
#![cfg(feature = "async")]

mod common;

use std::net::IpAddr;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use rightkit_http::testing::{loopback_config, MockResponse, MockServer};
use rightkit_http::*;

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

fn client(config: ClientConfig) -> AsyncClient {
    AsyncClient::new(ClientConfig {
        retry: RetryPolicy::none(),
        ..config
    })
    .unwrap()
}

// finding 1
#[tokio::test]
async fn f1_legacy_ipv4_spellings_are_denied_direct_and_through_a_proxy() {
    let direct = client(ClientConfig::default());
    let proxy = common::TestProxy::start();
    let proxied = client(ClientConfig::default().proxy(ProxyMode::Explicit(proxy.url())));
    for url in [
        "http://127.1/",
        "http://0177.0.0.1/",
        "http://0x7f.0.0.1/",
        "http://2130706433/",
        "http://0251.0376.0251.0376/",
    ] {
        for c in [&direct, &proxied] {
            let err = c.send(&Request::get(url)).await.unwrap_err();
            assert!(
                matches!(err, HttpError::AddressDenied { .. }),
                "{url}: {err:?}"
            );
        }
    }
    assert_eq!(proxy.total(), 0);
}

// finding 2 (async: exemption scoped to the real proxy connection)
#[tokio::test]
async fn f2_direct_requests_to_the_proxy_host_get_the_full_check() {
    // The proxy host resolves to a private address and NO_PROXY sends this host direct.
    let policy = AddressPolicy::default().with_resolver(|_| vec![ip("10.0.0.5")]);
    let c = client(
        ClientConfig::default()
            .address_policy(policy)
            .proxy(ProxyMode::Explicit("http://10.0.0.5:3128".into())),
    );
    // Through the proxy this name is fine (the proxy resolves it).
    // Direct access to a *different port of the proxy machine by name* would go through
    // VettingDns, which no longer exempts any host: prove it with the client's own resolver.
    let direct = client(
        ClientConfig::default()
            .address_policy(AddressPolicy::default().with_resolver(|_| vec![ip("10.0.0.5")])),
    );
    let err = direct
        .send(&Request::get("http://proxy.example:22/"))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            HttpError::AddressDenied {
                class: Some(AddressClass::Private),
                ..
            }
        ),
        "{err:?}"
    );
    drop(c);
}

// finding 5
#[tokio::test]
async fn f5_huge_retry_after_does_not_panic() {
    let server = MockServer::start(|_, i| {
        if i == 0 {
            MockResponse::new(503).header("Retry-After", "18446744073709551616")
        } else {
            MockResponse::text(200, "ok")
        }
    });
    let c = AsyncClient::new(ClientConfig {
        retry: RetryPolicy {
            max_attempts: 2,
            initial_backoff: Duration::from_millis(5),
            max_backoff: Duration::from_millis(5),
            max_retry_after: Duration::from_millis(60),
            jitter: 0.0,
            ..RetryPolicy::default()
        },
        ..loopback_config()
    })
    .unwrap();
    assert_eq!(
        c.send(&Request::get(server.url("/"))).await.unwrap().text(),
        "ok"
    );
}

// finding 7
#[tokio::test]
async fn f7_cancellation_and_deadline() {
    let server = MockServer::start(|_, _| MockResponse::new(200));
    let c = client(loopback_config());
    let token = CancelToken::new();
    token.cancel();
    let err = c
        .send(&Request::get(server.url("/")).cancel(token))
        .await
        .unwrap_err();
    assert!(matches!(err, HttpError::Cancelled), "{err:?}");
    assert_eq!(
        server.request_count(),
        0,
        "a cancelled call must send nothing"
    );

    // Cancelling during a retry sleep.
    let flaky = MockServer::start(|_, _| MockResponse::new(503));
    let slow_retry = AsyncClient::new(ClientConfig {
        retry: RetryPolicy {
            max_attempts: 5,
            initial_backoff: Duration::from_secs(5),
            max_backoff: Duration::from_secs(5),
            jitter: 0.0,
            ..RetryPolicy::default()
        },
        ..loopback_config()
    })
    .unwrap();
    let token = CancelToken::new();
    let t = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        t.cancel();
    });
    let started = Instant::now();
    let err = slow_retry
        .send(&Request::get(flaky.url("/")).cancel(token))
        .await
        .unwrap_err();
    assert!(matches!(err, HttpError::Cancelled), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(2));

    // The overall deadline stops further attempts and sleeps.
    let started = Instant::now();
    let err = slow_retry
        .send(&Request::get(flaky.url("/")).deadline(Duration::from_millis(300)))
        .await
        .unwrap_err();
    assert_eq!(err.status(), Some(503), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(2));

    // Cancelling an endless body stream, with default stream and body budgets.
    let endless = MockServer::start(|_, _| {
        MockResponse::new(200).chunks(
            (0..200)
                .map(|_| (Duration::from_millis(100), vec![b'x']))
                .collect(),
        )
    });
    let token = CancelToken::new();
    let t = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(250)).await;
        t.cancel();
    });
    let mut s = client(loopback_config())
        .stream(&Request::get(endless.url("/")).cancel(token))
        .await
        .unwrap()
        .into_stream();
    let started = Instant::now();
    let mut ended = None;
    while let Some(item) = s.next().await {
        if let Err(e) = item {
            ended = Some(e);
            break;
        }
    }
    assert!(matches!(ended, Some(HttpError::Cancelled)), "{ended:?}");
    assert!(started.elapsed() < Duration::from_secs(3));
}

// finding 8
#[tokio::test]
async fn f8_per_request_limit_and_headers_only() {
    let server = MockServer::start(|_, _| MockResponse::new(200).body(vec![7u8; 100]));
    let c = client(loopback_config());
    let err = c
        .send(&Request::get(server.url("/")).max_response_bytes(10))
        .await
        .unwrap_err();
    assert!(
        matches!(err, HttpError::BodyTooLarge { limit: 10 }),
        "{err:?}"
    );
    assert_eq!(
        c.send(&Request::get(server.url("/")))
            .await
            .unwrap()
            .body
            .len(),
        100
    );

    let slow = MockServer::start(|_, _| {
        MockResponse::new(200).chunks(vec![
            (Duration::ZERO, b"first".to_vec()),
            (Duration::from_secs(4), b"never".to_vec()),
        ])
    });
    let started = Instant::now();
    let resp = c
        .send_any(&Request::get(slow.url("/")).headers_only())
        .await
        .unwrap();
    assert_eq!(resp.status, 200);
    assert!(resp.body.is_empty());
    assert!(started.elapsed() < Duration::from_secs(2));
}

// finding 11 (async responses are the shared type) and 13
#[tokio::test]
async fn f13_json_decode_errors_are_scrubbed_async() {
    let server = MockServer::start(|_, _| {
        MockResponse::new(200)
            .header("Content-Type", "application/json")
            .body(b"\"SECRET-8\"".to_vec())
    });
    let c = client(ClientConfig {
        redact_values: vec!["SECRET-8".into()],
        ..loopback_config()
    });
    let err = c.get_json::<u64>(&server.url("/")).await.unwrap_err();
    assert!(matches!(err, HttpError::Json(_)));
    assert!(!format!("{err} {err:?}").contains("SECRET-8"));
}

#[tokio::test]
async fn non_canonical_spellings_are_refused_before_the_url_parser_rewrites_them() {
    let c = client(ClientConfig::default());
    for url in [
        "http://127.1/",
        "http://0x7f.0.0.1/",
        "http://2130706433/",
        "http://1.1.1.1./",
    ] {
        let err = c.send(&Request::get(url)).await.unwrap_err();
        assert!(err.to_string().contains("non-canonical"), "{url}: {err}");
    }
    // A redirect to a legacy spelling is refused too.
    let server =
        MockServer::start(|_, _| MockResponse::new(302).header("Location", "http://0177.0.0.1/"));
    let c = client(ClientConfig {
        max_redirects: 3,
        ..loopback_config()
    });
    let err = c.send(&Request::get(server.url("/"))).await.unwrap_err();
    assert!(err.to_string().contains("non-canonical"), "{err}");
    // The opt-out accepts the spelling and the class rules still deny it.
    let lax = client(
        ClientConfig::default().address_policy(AddressPolicy::default().allow_legacy_ipv4()),
    );
    let err = lax.send(&Request::get("http://127.1/")).await.unwrap_err();
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
}

#[tokio::test]
async fn proxy_diagnostic_reports_the_endpoint() {
    let proxy = common::TestProxy::start();
    let c = client(ClientConfig::default().proxy(ProxyMode::Explicit(
        proxy.url().replace("http://", "http://u:p-9@"),
    )));
    let d = c.proxy_diagnostic().unwrap();
    assert_eq!(d.endpoint, proxy.url());
    assert!(!format!("{d:?}").contains("p-9"));
}
