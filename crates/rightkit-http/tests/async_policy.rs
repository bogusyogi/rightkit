//! Outbound address policy for the async client (feature `async`): the same guard as the
//! blocking client, with resolution done by the client's DNS resolver and redirects followed
//! hop by hop by the crate.
#![cfg(feature = "async")]

mod common;

use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::TestProxy;
use futures_util::StreamExt;
use rightkit_http::testing::{loopback_policy, MockResponse, MockServer};
use rightkit_http::*;

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

fn lookup(host: &str) -> Vec<IpAddr> {
    match host {
        "private.test" => vec![ip("10.2.3.4")],
        "meta.test" => vec![ip("169.254.169.254")],
        "mapped.test" => vec![ip("::ffff:127.0.0.1")],
        "nat64.test" => vec![ip("64:ff9b::a9fe:a9fe")],
        "ula.test" => vec![ip("fd00::5")],
        "b.test" | "pinned.test" | "pub.test" | "proxy.test" => vec![ip("127.0.0.1")],
        "lan.test" => vec![ip("10.0.0.5")],
        _ => vec![],
    }
}

fn client(policy: AddressPolicy, max_redirects: u32) -> AsyncClient {
    AsyncClient::new(ClientConfig {
        address_policy: policy,
        max_redirects,
        retry: RetryPolicy::none(),
        request_timeout: Duration::from_secs(5),
        response_timeout: Some(Duration::from_secs(5)),
        ..ClientConfig::default()
    })
    .unwrap()
}

fn denied_class(err: HttpError) -> (Option<AddressClass>, String) {
    match err {
        HttpError::AddressDenied { class, reason, .. } => (class, reason),
        other => panic!("expected AddressDenied, got {other:?}"),
    }
}

#[tokio::test]
async fn default_client_refuses_loopback_literals_and_names() {
    let server = MockServer::start(|_, _| MockResponse::text(200, "ok"));
    let c = AsyncClient::new(ClientConfig::default()).unwrap();
    for (url, class) in [
        (server.url("/"), AddressClass::Loopback),
        ("http://[::1]:9/".to_string(), AddressClass::Loopback),
        (
            "http://169.254.169.254/".to_string(),
            AddressClass::LinkLocal,
        ),
        ("http://10.0.0.1/".to_string(), AddressClass::Private),
        (
            format!("http://localhost:{}/", server.port()),
            AddressClass::Loopback,
        ),
        (
            "http://metadata.google.internal/".to_string(),
            AddressClass::LinkLocal,
        ),
    ] {
        let (got, _) = denied_class(c.send(&Request::get(&url)).await.unwrap_err());
        assert_eq!(got, Some(class), "{url}");
    }
    assert_eq!(server.request_count(), 0);
}

#[tokio::test]
async fn resolver_denies_names_by_resolved_class_including_embedded_forms() {
    let c = client(AddressPolicy::default().with_resolver(lookup), 0);
    for (name, class) in [
        ("private.test", AddressClass::Private),
        ("meta.test", AddressClass::LinkLocal),
        ("mapped.test", AddressClass::Loopback),
        ("nat64.test", AddressClass::LinkLocal),
        ("ula.test", AddressClass::Private),
    ] {
        let (got, _) = denied_class(
            c.send(&Request::get(format!("http://{name}/")))
                .await
                .unwrap_err(),
        );
        assert_eq!(got, Some(class), "{name}");
    }
    // A name that resolves to nothing.
    let (class, reason) = denied_class(
        c.send(&Request::get("http://nothing.test/"))
            .await
            .unwrap_err(),
    );
    assert_eq!(class, None);
    assert!(reason.contains("did not resolve"), "{reason}");
}

#[tokio::test]
async fn connection_uses_the_vetted_address() {
    let server = MockServer::start(|_, _| MockResponse::text(200, "via-vetted-address"));
    let policy = AddressPolicy::default()
        .with_resolver(lookup)
        .allow_if(vec!["pinned.test".into()], |a| a.is_loopback());
    let c = client(policy, 0);
    let resp = c
        .send(&Request::get(format!(
            "http://pinned.test:{}/p",
            server.port()
        )))
        .await
        .unwrap();
    assert_eq!(resp.text(), "via-vetted-address");
}

#[tokio::test]
async fn deny_ranges_and_deny_if_apply() {
    let server = MockServer::start(|_, _| MockResponse::text(200, "ok"));
    let c = client(
        loopback_policy().deny_ranges(vec!["127.0.0.0/8".parse().unwrap()]),
        0,
    );
    let (_, reason) = denied_class(c.send(&Request::get(server.url("/"))).await.unwrap_err());
    assert!(reason.contains("127.0.0.0/8"), "{reason}");
    let c = client(
        loopback_policy().deny_if(|_| Some("no thanks".to_string())),
        0,
    );
    let (_, reason) = denied_class(c.send(&Request::get(server.url("/"))).await.unwrap_err());
    assert_eq!(reason, "no thanks");
    assert_eq!(server.request_count(), 0);
}

#[tokio::test]
async fn redirect_hops_are_vetted_and_credentials_stripped_cross_origin() {
    // Denied hop.
    let a = MockServer::start(|_, _| {
        MockResponse::new(302).header("Location", "http://169.254.169.254/latest/")
    });
    let c = client(loopback_policy(), 4);
    let (class, _) = denied_class(c.send(&Request::get(a.url("/"))).await.unwrap_err());
    assert_eq!(class, Some(AddressClass::LinkLocal));

    // Cross-origin hop to an allowed host drops credentials; same-origin keeps them.
    let b = MockServer::start(|_, _| MockResponse::text(200, "at-b"));
    let b_port = b.port();
    let a = MockServer::start(move |req, _| match req.path() {
        "/cross" => MockResponse::new(302).header("Location", &format!("http://b.test:{b_port}/l")),
        "/same" => MockResponse::new(302).header("Location", "/l"),
        _ => MockResponse::text(200, "at-a"),
    });
    let policy = AddressPolicy::default()
        .with_resolver(lookup)
        .allow_if(vec!["127.0.0.1".into(), "b.test".into()], |x| {
            x.is_loopback()
        });
    let c = client(policy, 4);
    let req = |p: &str| {
        Request::get(a.url(p))
            .bearer("bearer-async-1")
            .secret_header("X-Service-Key", "service-async-2")
            .header("Accept", "text/plain")
    };
    assert_eq!(c.send(&req("/cross")).await.unwrap().text(), "at-b");
    let landed = &b.requests()[0];
    assert_eq!(landed.header("authorization"), None);
    assert_eq!(landed.header("x-service-key"), None);
    assert_eq!(landed.header("accept"), Some("text/plain"));
    assert_eq!(c.send(&req("/same")).await.unwrap().text(), "at-a");
    let landed = a.requests().into_iter().find(|r| r.path() == "/l").unwrap();
    assert_eq!(
        landed.header("authorization"),
        Some("Bearer bearer-async-1")
    );
}

#[tokio::test]
async fn redirect_limit_is_enforced() {
    let server = MockServer::start(|req, _| match req.path() {
        "/a" => MockResponse::new(302).header("Location", "/b"),
        "/b" => MockResponse::new(302).header("Location", "/c"),
        _ => MockResponse::text(200, "landed"),
    });
    assert_eq!(
        client(loopback_policy(), 2)
            .send(&Request::get(server.url("/a")))
            .await
            .unwrap()
            .text(),
        "landed"
    );
    let err = client(loopback_policy(), 1)
        .send(&Request::get(server.url("/a")))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, HttpError::Transport { message, .. } if message == "too many redirects"),
        "{err:?}"
    );
    let resp = client(loopback_policy(), 0)
        .send_any(&Request::get(server.url("/a")))
        .await
        .unwrap();
    assert_eq!(resp.status, 302);
}

#[tokio::test]
async fn first_party_pins_hosts_and_requires_https() {
    let c = client(
        AddressPolicy::first_party(vec!["license.test".into()]).with_resolver(lookup),
        0,
    );
    let (_, reason) = denied_class(
        c.send(&Request::get("http://license.test/"))
            .await
            .unwrap_err(),
    );
    assert!(reason.contains("HTTPS only"), "{reason}");
    let (_, reason) = denied_class(
        c.send(&Request::get("https://other.test/"))
            .await
            .unwrap_err(),
    );
    assert!(reason.contains("first-party host list"), "{reason}");
    let c = AsyncClient::first_party(vec!["api.example.com".into()]).unwrap();
    let (_, reason) = denied_class(
        c.send(&Request::get("https://evil.example/"))
            .await
            .unwrap_err(),
    );
    assert!(reason.contains("first-party host list"), "{reason}");
}

#[tokio::test]
async fn stream_and_stream_any_honour_the_request_timeout() {
    let server = MockServer::start(|_, _| {
        MockResponse::new(200).chunks(vec![
            (Duration::ZERO, b"one".to_vec()),
            (Duration::from_millis(700), b"two".to_vec()),
        ])
    });
    let c = AsyncClient::new(ClientConfig {
        retry: RetryPolicy::none(),
        ..rightkit_http::testing::loopback_config()
    })
    .unwrap();

    // Request::timeout is the total stream budget; ClientConfig::stream_timeout is None.
    let mut s = c
        .stream(&Request::get(server.url("/")).timeout(Duration::from_millis(250)))
        .await
        .unwrap()
        .into_stream();
    assert_eq!(&s.next().await.unwrap().unwrap()[..], b"one");
    assert!(matches!(
        s.next().await,
        Some(Err(HttpError::Timeout { .. }))
    ));
    let mut s = c
        .stream_any(&Request::get(server.url("/")).timeout(Duration::from_millis(250)))
        .await
        .unwrap()
        .into_stream();
    assert_eq!(&s.next().await.unwrap().unwrap()[..], b"one");
    assert!(matches!(
        s.next().await,
        Some(Err(HttpError::Timeout { .. }))
    ));

    // Without it, the whole body arrives.
    let mut s = c
        .stream(&Request::get(server.url("/")))
        .await
        .unwrap()
        .into_stream();
    let mut all = Vec::new();
    while let Some(chunk) = s.next().await {
        all.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(all, b"onetwo");

    // Request::timeout wins over a longer stream_timeout, like the blocking client.
    let c = AsyncClient::new(ClientConfig {
        stream_timeout: Some(Duration::from_secs(30)),
        retry: RetryPolicy::none(),
        ..rightkit_http::testing::loopback_config()
    })
    .unwrap();
    let mut s = c
        .stream(&Request::get(server.url("/")).timeout(Duration::from_millis(250)))
        .await
        .unwrap()
        .into_stream();
    assert_eq!(&s.next().await.unwrap().unwrap()[..], b"one");
    assert!(matches!(
        s.next().await,
        Some(Err(HttpError::Timeout { .. }))
    ));
}

#[tokio::test]
async fn errors_and_retry_events_never_echo_url_query_or_userinfo() {
    let server = MockServer::start(|_, _| MockResponse::text(503, "busy"));
    let events = Arc::new(Mutex::new(Vec::<RetryEvent>::new()));
    let sink = events.clone();
    let c = AsyncClient::new(ClientConfig {
        retry: RetryPolicy {
            max_attempts: 2,
            initial_backoff: Duration::from_millis(5),
            max_backoff: Duration::from_millis(5),
            jitter: 0.0,
            ..RetryPolicy::default()
        },
        on_retry: Some(Arc::new(move |e: &RetryEvent| {
            sink.lock().unwrap().push(e.clone())
        })),
        ..rightkit_http::testing::loopback_config()
    })
    .unwrap();
    let err = c
        .send(&Request::get(format!(
            "{}?key=ASYNC-KEY-1",
            server.url("/v1")
        )))
        .await
        .unwrap_err();
    let events = events.lock().unwrap().clone();
    assert_eq!(events[0].url, format!("{}?[redacted]", server.url("/v1")));
    assert!(!format!("{err} {err:?} {:?}", events[0]).contains("ASYNC-KEY-1"));

    let c = AsyncClient::new(ClientConfig::default()).unwrap();
    let err = c
        .send(&Request::get(
            "http://user:async-pass-5@10.0.0.1/p?key=ASYNC-KEY-2",
        ))
        .await
        .unwrap_err();
    assert!(matches!(err, HttpError::AddressDenied { .. }));
    let text = format!("{err} {err:?}");
    assert!(
        !text.contains("ASYNC-KEY-2") && !text.contains("async-pass-5"),
        "{text}"
    );
    let err = c
        .send(&Request::get("not a url?key=ASYNC-KEY-3"))
        .await
        .unwrap_err();
    assert!(!format!("{err} {err:?}").contains("ASYNC-KEY-3"));
}

fn proxied(policy: AddressPolicy, mode: ProxyMode) -> AsyncClient {
    AsyncClient::new(ClientConfig {
        address_policy: policy,
        proxy: mode,
        retry: RetryPolicy::none(),
        request_timeout: Duration::from_secs(5),
        response_timeout: Some(Duration::from_secs(5)),
        ..ClientConfig::default()
    })
    .unwrap()
}

#[tokio::test]
async fn a_private_proxy_is_trusted_and_carries_a_checked_destination() {
    let proxy = TestProxy::start();
    // The proxy is a loopback IP literal that the default policy denies as a destination.
    let c = proxied(AddressPolicy::default(), ProxyMode::Explicit(proxy.url()));
    let resp = c
        .send(&Request::get("http://app.example.com:8080/page"))
        .await
        .unwrap();
    assert_eq!(resp.text(), "via-proxy");
    let forwards = proxy.forwards();
    assert_eq!(forwards.len(), 1);
    assert!(
        forwards[0].contains("http://app.example.com:8080/page"),
        "{forwards:?}"
    );
}

#[tokio::test]
async fn literal_and_name_destinations_are_denied_before_the_proxy_is_asked() {
    let proxy = TestProxy::start();
    let c = proxied(
        AddressPolicy::default().allow_if(vec!["wiki".into()], |_| true),
        ProxyMode::Explicit(proxy.url()),
    );
    for url in [
        "http://169.254.169.254/",
        "http://10.1.2.3/",
        "http://localhost:9/",
        "http://printer.local/",
        "http://db.internal/",
        "http://intranet/",
    ] {
        let err = c.send(&Request::get(url)).await.unwrap_err();
        assert!(
            matches!(err, HttpError::AddressDenied { .. }),
            "{url}: {err:?}"
        );
    }
    assert_eq!(proxy.total(), 0);
    // Listed single-label host passes.
    assert_eq!(
        c.send(&Request::get("http://wiki/")).await.unwrap().text(),
        "via-proxy"
    );
}

#[tokio::test]
async fn invalid_explicit_proxy_is_an_error_at_construction() {
    let err =
        AsyncClient::new(ClientConfig::default().proxy(ProxyMode::Explicit("ftp://nope:1".into())))
            .err()
            .unwrap();
    assert!(matches!(err, HttpError::InvalidRequest(_)), "{err:?}");
}

#[tokio::test]
async fn error_phases() {
    // Connect.
    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let c = client(loopback_policy(), 0);
    let err = c
        .send(&Request::get(format!("http://127.0.0.1:{closed}/")))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            HttpError::Transport {
                phase: Phase::Connect,
                connect_phase: true,
                ..
            }
        ),
        "{err:?}"
    );

    // Headers.
    let slow =
        MockServer::start(|_, _| MockResponse::text(200, "late").delay(Duration::from_millis(900)));
    let c = AsyncClient::new(ClientConfig {
        response_timeout: Some(Duration::from_millis(150)),
        retry: RetryPolicy::none(),
        ..rightkit_http::testing::loopback_config()
    })
    .unwrap();
    let err = c.send(&Request::get(slow.url("/"))).await.unwrap_err();
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

    // Body.
    let chunked = MockServer::start(|_, _| {
        MockResponse::new(200).chunks(vec![
            (Duration::ZERO, b"one".to_vec()),
            (Duration::from_millis(700), b"two".to_vec()),
        ])
    });
    let c = AsyncClient::new(ClientConfig {
        recv_body_timeout: Some(Duration::from_millis(150)),
        retry: RetryPolicy::none(),
        ..rightkit_http::testing::loopback_config()
    })
    .unwrap();
    let err = c.send(&Request::get(chunked.url("/"))).await.unwrap_err();
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

    // Redirect: a closed second hop, and an exhausted limit.
    let to_closed = MockServer::start(move |_, _| {
        MockResponse::new(302).header("Location", &format!("http://127.0.0.1:{closed}/"))
    });
    let c = client(loopback_policy(), 3);
    let err = c.send(&Request::get(to_closed.url("/"))).await.unwrap_err();
    assert!(
        matches!(
            err,
            HttpError::Transport {
                phase: Phase::Redirect,
                connect_phase: false,
                ..
            }
        ),
        "{err:?}"
    );
    let looping = MockServer::start(|_, _| MockResponse::new(302).header("Location", "/loop"));
    let c = client(loopback_policy(), 2);
    let err = c
        .send(&Request::get(looping.url("/loop")))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            HttpError::Transport {
                phase: Phase::Redirect,
                ..
            }
        ),
        "{err:?}"
    );
}
