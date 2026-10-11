//! One routing decision per hop (finding 3). `ProxyMode::Environment` reads the process
//! environment, so this binary holds one test function and sets the variables once.

mod common;

use std::net::IpAddr;
use std::time::Duration;

use common::TestProxy;
use rightkit_http::testing::{loopback_policy, MockResponse, MockServer};
use rightkit_http::*;

fn policy() -> AddressPolicy {
    // `api.example.com` resolves to cloud metadata; the policy must refuse it when the
    // request is direct, and the proxy must never see it.
    loopback_policy().with_resolver(|h| {
        if h.ends_with("example.com") {
            vec!["169.254.169.254".parse::<IpAddr>().unwrap()]
        } else {
            vec![]
        }
    })
}

fn config() -> ClientConfig {
    ClientConfig {
        address_policy: policy(),
        proxy: ProxyMode::Environment,
        retry: RetryPolicy::none(),
        max_redirects: 3,
        request_timeout: Duration::from_secs(5),
        response_timeout: Some(Duration::from_secs(5)),
        connect_timeout: Some(Duration::from_secs(3)),
        ..ClientConfig::default()
    }
}

fn is_denied(e: &HttpError) -> bool {
    matches!(e, HttpError::AddressDenied { .. })
}

#[test]
fn wildcard_no_proxy_hosts_go_direct_and_are_vetted_in_both_clients_and_after_redirects() {
    let proxy = TestProxy::start();
    for name in [
        "ALL_PROXY", "all_proxy", "HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy",
        "no_proxy",
    ] {
        std::env::remove_var(name);
    }
    std::env::set_var("HTTP_PROXY", proxy.url());
    std::env::set_var("NO_PROXY", "127.0.0.1,*.example.com");

    let blocking = Client::new(config());
    let err = blocking
        .send(&Request::get("http://api.example.com:8080/"))
        .unwrap_err();
    assert!(is_denied(&err), "blocking: {err:?}");
    assert_eq!(proxy.total(), 0, "blocking client reached the proxy");

    // A redirect hop to a wildcard-listed host.
    let redirector = MockServer::start(|_, _| {
        MockResponse::new(302).header("Location", "http://api.example.com:8080/")
    });
    let err = blocking.send(&Request::get(redirector.url("/"))).unwrap_err();
    assert!(is_denied(&err), "blocking redirect: {err:?}");
    assert_eq!(proxy.total(), 0, "blocking redirect reached the proxy");

    #[cfg(feature = "async")]
    {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let c = AsyncClient::new(config()).unwrap();
            let err = c
                .send(&Request::get("http://api.example.com:8080/"))
                .await
                .unwrap_err();
            assert!(is_denied(&err), "async: {err:?}");
            assert_eq!(proxy.total(), 0, "async client reached the proxy");
            let err = c.send(&Request::get(redirector.url("/"))).await.unwrap_err();
            assert!(is_denied(&err), "async redirect: {err:?}");
            assert_eq!(proxy.total(), 0, "async redirect reached the proxy");
        });
    }
}
