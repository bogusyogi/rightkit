//! `ProxyMode::Environment` reads the process environment, so this binary holds one test:
//! nothing else here can race the variables it sets.

mod common;

use std::net::IpAddr;
use std::time::Duration;

use common::TestProxy;
use rightkit_http::testing::{MockResponse, MockServer};
use rightkit_http::*;

#[test]
fn environment_proxy_is_used_trusted_and_honours_no_proxy() {
    let target = MockServer::start(|_, _| MockResponse::text(200, "at-target"));
    let addr = format!("127.0.0.1:{}", target.port()).parse().unwrap();
    // A proxy on a private (loopback) address.
    let proxy = TestProxy::start_to(Some(addr));
    for name in [
        "ALL_PROXY",
        "all_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "no_proxy",
    ] {
        std::env::remove_var(name);
    }
    std::env::set_var("HTTP_PROXY", proxy.url());
    std::env::set_var("NO_PROXY", "direct.test");

    let policy = AddressPolicy::default()
        .with_resolver(|h| {
            if h == "direct.test" {
                vec!["127.0.0.1".parse::<IpAddr>().unwrap()]
            } else {
                vec![]
            }
        })
        .allow_if(vec!["direct.test".into()], |a| a.is_loopback());
    let c = Client::new(ClientConfig {
        address_policy: policy,
        proxy: ProxyMode::Environment,
        retry: RetryPolicy::none(),
        request_timeout: Duration::from_secs(5),
        ..ClientConfig::default()
    });

    // A normal name goes through the private proxy, unresolved.
    let resp = c
        .send(&Request::get(format!(
            "http://app.example.com:{}/",
            target.port()
        )))
        .unwrap();
    assert_eq!(resp.text(), "at-target");
    assert_eq!(
        proxy.connects(),
        vec![format!("app.example.com:{}", target.port())]
    );

    // A literal metadata address is denied through the proxy.
    let err = c
        .send(&Request::get("http://169.254.169.254/"))
        .unwrap_err();
    assert!(matches!(err, HttpError::AddressDenied { .. }), "{err:?}");
    assert_eq!(proxy.connects().len(), 1);

    // A NO_PROXY host connects directly (resolved and vetted here), not through the proxy.
    let resp = c
        .send(&Request::get(format!(
            "http://direct.test:{}/",
            target.port()
        )))
        .unwrap();
    assert_eq!(resp.text(), "at-target");
    assert_eq!(
        proxy.connects().len(),
        1,
        "NO_PROXY host must bypass the proxy"
    );

    // And a NO_PROXY host gets the full direct check: unlisted loopback is denied.
    std::env::set_var("NO_PROXY", "other.test");
    let c = Client::new(ClientConfig {
        address_policy: AddressPolicy::default()
            .with_resolver(|_| vec!["10.0.0.9".parse().unwrap()]),
        proxy: ProxyMode::Environment,
        retry: RetryPolicy::none(),
        ..ClientConfig::default()
    });
    let err = c.send(&Request::get("http://other.test/")).unwrap_err();
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

    // Variables are ignored unless Environment is selected.
    let c = Client::new(ClientConfig {
        address_policy: AddressPolicy::default(),
        retry: RetryPolicy::none(),
        ..ClientConfig::default()
    });
    let err = c.send(&Request::get("http://127.0.0.1:9/")).unwrap_err();
    assert!(matches!(err, HttpError::AddressDenied { .. }), "{err:?}");
}
