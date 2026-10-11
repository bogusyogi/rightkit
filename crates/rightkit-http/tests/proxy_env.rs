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

    // Audit finding 2: with NO_PROXY=* the proxy's own host and port is an ordinary direct
    // destination. It gets the full check and is not exempted as the proxy endpoint.
    std::env::set_var("ALL_PROXY", "http://proxy.example:3128");
    std::env::remove_var("HTTP_PROXY");
    std::env::set_var("NO_PROXY", "*");
    let c = Client::new(ClientConfig {
        address_policy: AddressPolicy::default()
            .with_resolver(|_| vec!["10.0.0.5".parse::<IpAddr>().unwrap()]),
        proxy: ProxyMode::Environment,
        retry: RetryPolicy::none(),
        request_timeout: Duration::from_secs(5),
        ..ClientConfig::default()
    });
    let err = c
        .send(&Request::get("http://proxy.example:3128/"))
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
    // A first-party client keeps its host pin for that name, too.
    let fp = Client::new(ClientConfig {
        address_policy: AddressPolicy::first_party(vec!["api.example.com".into()])
            .with_resolver(|_| vec!["10.0.0.5".parse::<IpAddr>().unwrap()]),
        proxy: ProxyMode::Environment,
        retry: RetryPolicy::none(),
        ..ClientConfig::default()
    });
    let err = fp
        .send(&Request::get("https://proxy.example:3128/"))
        .unwrap_err();
    assert!(matches!(err, HttpError::AddressDenied { .. }), "{err:?}");

    // Credentials in an environment proxy never reach an error or the diagnostic, and the
    // proxy's private address is accepted because the environment configured it.
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    std::env::set_var(
        "ALL_PROXY",
        format!("http://svc-user:svc-pass-4@127.0.0.1:{dead}"),
    );
    std::env::set_var("NO_PROXY", "unrelated.test");
    let c = Client::new(ClientConfig {
        retry: RetryPolicy::none(),
        proxy: ProxyMode::Environment,
        request_timeout: Duration::from_secs(5),
        ..ClientConfig::default()
    });
    let d = c.proxy_diagnostic().unwrap();
    assert!(d.from_environment && d.endpoint_exempt_from_class_check);
    assert_eq!(d.endpoint, format!("http://127.0.0.1:{dead}"));
    for text in [
        format!("{d:?}"),
        // A literal private destination is refused by name/literal rules.
        {
            let e = c.send(&Request::get("http://10.1.2.3/")).unwrap_err();
            assert!(matches!(e, HttpError::AddressDenied { .. }), "{e:?}");
            format!("{e} {e:?}")
        },
        // An allowed destination reaches the (dead) proxy, which fails: the error is a
        // transport error that must not carry the credentials.
        {
            let e = c
                .send(&Request::get("http://app.example.com/"))
                .unwrap_err();
            assert!(matches!(e, HttpError::Transport { .. }), "{e:?}");
            format!("{e} {e:?}")
        },
    ] {
        assert!(
            !text.contains("svc-pass-4") && !text.contains("svc-user"),
            "{text}"
        );
    }
}
