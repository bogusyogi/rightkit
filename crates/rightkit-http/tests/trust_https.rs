//! `TrustRoots` and `https_only` (gap ledger row 110, items 1 and 2).
//!
//! A real certificate chain needs a TLS server this crate does not ship, so the trust tests
//! prove the parts reachable over loopback: both root sources build a working verifier and
//! reject a peer that does not speak TLS (no panic, no silent downgrade), and the defaults
//! are what the changelog says. The redirect-hop rule is unit-tested in `redirect.rs`.

use std::time::Duration;

use rightkit_http::testing::{loopback_config, MockResponse, MockServer};
use rightkit_http::*;

fn cfg() -> ClientConfig {
    ClientConfig {
        retry: RetryPolicy::none(),
        request_timeout: Duration::from_secs(5),
        response_timeout: Some(Duration::from_secs(5)),
        connect_timeout: Some(Duration::from_secs(3)),
        ..loopback_config()
    }
}

fn plain_server() -> MockServer {
    MockServer::start(|_, _| MockResponse::text(200, "ok"))
}

#[test]
fn defaults_are_platform_trust_and_https_not_required() {
    let c = ClientConfig::default();
    assert_eq!(c.trust, TrustRoots::Platform);
    assert!(!c.https_only);
    assert_eq!(TrustRoots::default(), TrustRoots::Platform);
    assert_eq!(Client::new(cfg()).verifier_kind(), "platform");
    assert_eq!(
        Client::new(cfg().trust(TrustRoots::Bundled)).verifier_kind(),
        "bundled"
    );
}

#[test]
fn https_only_refuses_an_http_url_before_sending() {
    let server = plain_server();
    let c = Client::new(cfg().https_only(true));
    let err = c.send(&Request::get(server.url("/"))).unwrap_err();
    match &err {
        HttpError::HttpsRequired {
            scheme, redirect, ..
        } => {
            assert_eq!(scheme, "http");
            assert!(!redirect);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(err.class(), ErrorClass::Invalid);
    assert_eq!(server.request_count(), 0);
}

#[test]
fn https_only_off_keeps_plain_http_working() {
    let server = plain_server();
    let c = Client::new(cfg());
    assert_eq!(c.send(&Request::get(server.url("/"))).unwrap().status, 200);
}

#[test]
fn https_only_is_not_retried_and_covers_streams() {
    let server = plain_server();
    let c = Client::new(cfg().https_only(true));
    assert!(matches!(
        c.send_any(&Request::get(server.url("/"))),
        Err(HttpError::HttpsRequired { .. })
    ));
    assert_eq!(server.request_count(), 0);
}

#[test]
fn both_trust_sources_fail_closed_against_a_peer_that_is_not_tls() {
    let server = plain_server();
    let url = server.url("/").replacen("http://", "https://", 1);
    for trust in [TrustRoots::Platform, TrustRoots::Bundled] {
        let c = Client::new(cfg().trust(trust));
        let err = c.send(&Request::get(url.clone())).unwrap_err();
        assert!(
            matches!(err, HttpError::Transport { .. } | HttpError::Timeout { .. }),
            "{trust:?}: {err:?}"
        );
    }
}

#[cfg(feature = "async")]
mod async_side {
    use super::*;

    #[tokio::test]
    async fn https_only_refuses_an_http_url_before_sending() {
        let server = plain_server();
        let c = AsyncClient::new(cfg().https_only(true)).unwrap();
        let err = c.send(&Request::get(server.url("/"))).await.unwrap_err();
        assert!(matches!(
            err,
            HttpError::HttpsRequired {
                redirect: false,
                ..
            }
        ));
        assert_eq!(server.request_count(), 0);
    }

    #[tokio::test]
    async fn both_trust_sources_build_and_fail_closed_against_a_non_tls_peer() {
        let server = plain_server();
        let url = server.url("/").replacen("http://", "https://", 1);
        for trust in [TrustRoots::Platform, TrustRoots::Bundled] {
            let c = AsyncClient::new(cfg().trust(trust)).unwrap();
            assert_eq!(c.verifier_kind(), trust.verifier_kind());
            let err = c.send(&Request::get(url.clone())).await.unwrap_err();
            assert!(
                matches!(err, HttpError::Transport { .. } | HttpError::Timeout { .. }),
                "{trust:?}: {err:?}"
            );
        }
    }
}
