//! Proxy modes against the blocking client, with an in-process CONNECT proxy.
//!
//! Names come from `AddressPolicy::with_resolver`, so nothing touches real DNS. The proxy
//! and the targets are all on loopback; each test lists exactly the hosts it allows.

mod common;

use std::net::{IpAddr, TcpListener};
use std::time::Duration;

use common::TestProxy;
use rightkit_http::testing::{MockResponse, MockServer};
use rightkit_http::*;

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

fn lookup(host: &str) -> Vec<IpAddr> {
    match host {
        "direct.test" | "fp.test" => vec![ip("127.0.0.1")],
        _ => vec![],
    }
}

fn client(policy: AddressPolicy, proxy: &TestProxy) -> Client {
    Client::new(ClientConfig {
        address_policy: policy,
        proxy: ProxyMode::Explicit(proxy.url()),
        retry: RetryPolicy::none(),
        request_timeout: Duration::from_secs(5),
        response_timeout: Some(Duration::from_secs(5)),
        connect_timeout: Some(Duration::from_secs(3)),
        ..ClientConfig::default()
    })
}

fn denied(err: HttpError) -> (String, Option<AddressClass>) {
    match err {
        HttpError::AddressDenied { host, class, .. } => (host, class),
        other => panic!("expected AddressDenied, got {other:?}"),
    }
}

#[test]
fn a_proxy_on_a_private_address_is_trusted() {
    // The proxy listens on 127.0.0.1, which the default policy denies for destinations. As
    // configuration it is accepted, and the name travels to the proxy unresolved.
    let target = MockServer::start(|_, _| MockResponse::text(200, "at-target"));
    let target_addr = format!("127.0.0.1:{}", target.port()).parse().unwrap();
    let proxy = TestProxy::start_to(Some(target_addr));
    let c = client(AddressPolicy::default(), &proxy);
    let resp = c
        .send(&Request::get(format!(
            "http://app.example.com:{}/p",
            target.port()
        )))
        .unwrap();
    assert_eq!(resp.text(), "at-target");
    assert_eq!(
        proxy.connects(),
        vec![format!("app.example.com:{}", target.port())]
    );
}

#[test]
fn literal_destinations_are_denied_through_the_proxy() {
    let proxy = TestProxy::start();
    let c = client(AddressPolicy::default(), &proxy);
    for (url, class) in [
        (
            "http://169.254.169.254/latest/meta-data/",
            AddressClass::LinkLocal,
        ),
        ("http://10.1.2.3/", AddressClass::Private),
        ("http://[::1]:9/", AddressClass::Loopback),
        ("http://127.0.0.1:9/", AddressClass::Loopback),
        ("http://[::ffff:169.254.169.254]/", AddressClass::LinkLocal),
    ] {
        let (_, got) = denied(c.send(&Request::get(url)).unwrap_err());
        assert_eq!(got, Some(class), "{url}");
    }
    assert_eq!(proxy.total(), 0, "nothing may reach the proxy");
}

#[test]
fn deny_ranges_and_deny_if_apply_to_literals_through_the_proxy() {
    let proxy = TestProxy::start();
    let policy = AddressPolicy::default()
        .deny_ranges(vec!["93.184.216.0/24".parse().unwrap()])
        .deny_if(|a| (*a == ip("8.8.8.8")).then(|| "no resolvers".to_string()));
    let c = client(policy, &proxy);
    let err = c.send(&Request::get("http://93.184.216.34/")).unwrap_err();
    assert!(matches!(err, HttpError::AddressDenied { .. }), "{err:?}");
    let err = c.send(&Request::get("http://8.8.8.8/")).unwrap_err();
    assert!(err.to_string().contains("no resolvers"), "{err}");
    assert_eq!(proxy.total(), 0);
}

#[test]
fn name_rules_apply_through_the_proxy_unless_the_host_is_listed() {
    let target = MockServer::start(|_, _| MockResponse::text(200, "intranet"));
    let target_addr = format!("127.0.0.1:{}", target.port()).parse().unwrap();
    let proxy = TestProxy::start_to(Some(target_addr));
    let policy = AddressPolicy::default().allow_if(vec!["wiki".into()], |_| true);
    let c = client(policy, &proxy);
    for url in [
        "http://localhost:9/",
        "http://app.localhost:9/",
        "http://printer.local/",
        "http://db.corp.internal/",
        "http://intranet/",
        "http://metadata.google.internal/",
    ] {
        let err = c.send(&Request::get(url)).unwrap_err();
        assert!(
            matches!(err, HttpError::AddressDenied { .. }),
            "{url}: {err:?}"
        );
    }
    assert_eq!(proxy.total(), 0);
    // A listed single-label host is allowed through.
    let resp = c
        .send(&Request::get(format!("http://wiki:{}/", target.port())))
        .unwrap();
    assert_eq!(resp.text(), "intranet");
    assert_eq!(proxy.connects(), vec![format!("wiki:{}", target.port())]);
}

#[test]
fn redirect_hops_are_vetted_through_the_proxy_too() {
    let a = MockServer::start(|_, _| {
        MockResponse::new(302).header("Location", "http://169.254.169.254/latest/")
    });
    let addr = format!("127.0.0.1:{}", a.port()).parse().unwrap();
    let proxy = TestProxy::start_to(Some(addr));
    let c = Client::new(ClientConfig {
        proxy: ProxyMode::Explicit(proxy.url()),
        max_redirects: 3,
        retry: RetryPolicy::none(),
        ..ClientConfig::default()
    });
    let err = c
        .send(&Request::get(format!(
            "http://app.example.com:{}/",
            a.port()
        )))
        .unwrap_err();
    let (host, class) = denied(err);
    assert_eq!(host, "169.254.169.254");
    assert_eq!(class, Some(AddressClass::LinkLocal));
    assert_eq!(
        proxy.connects().len(),
        1,
        "only the first hop reached the proxy"
    );
}

/// A listener that accepts and drops every connection: a target that never speaks TLS.
struct DeadEnd {
    port: u16,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl DeadEnd {
    fn start() -> Self {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = std::thread::spawn(move || {
            for conn in listener.incoming() {
                if flag.load(Ordering::SeqCst) {
                    break;
                }
                drop(conn);
            }
        });
        Self {
            port,
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for DeadEnd {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[test]
fn first_party_clients_use_the_proxy_unless_configured_to_bypass_it() {
    let dead_end = DeadEnd::start();
    let port = dead_end.port;
    let proxy = TestProxy::start_to(Some(format!("127.0.0.1:{port}").parse().unwrap()));
    let policy = || {
        AddressPolicy::first_party(vec!["fp.test".into()])
            .allow_private_resolution("fp.test")
            .with_resolver(lookup)
            .allow_if(vec!["127.0.0.1".into()], |a| a.is_loopback())
    };
    let url = format!("https://fp.test:{port}/");

    // Default: the first-party request goes through the proxy.
    let c = Client::new(ClientConfig {
        address_policy: policy(),
        proxy: ProxyMode::Explicit(proxy.url()),
        retry: RetryPolicy::none(),
        ..ClientConfig::default()
    });
    let err = c.send(&Request::get(&url)).unwrap_err();
    assert!(!matches!(err, HttpError::AddressDenied { .. }), "{err:?}");
    assert_eq!(proxy.connects(), vec![format!("fp.test:{port}")]);

    // Bypass: it connects directly and the proxy is never contacted.
    let c = Client::new(ClientConfig {
        address_policy: policy(),
        proxy: ProxyMode::Explicit(proxy.url()),
        proxy_bypass_first_party: true,
        retry: RetryPolicy::none(),
        ..ClientConfig::default()
    });
    let err = c.send(&Request::get(&url)).unwrap_err();
    assert!(!matches!(err, HttpError::AddressDenied { .. }), "{err:?}");
    assert_eq!(
        proxy.connects().len(),
        1,
        "no new CONNECT with the bypass on"
    );

    // The first-party host pin still holds through the proxy.
    let err = c.send(&Request::get("https://other.test/")).unwrap_err();
    assert!(matches!(err, HttpError::AddressDenied { .. }), "{err:?}");
}

#[test]
fn proxy_credentials_never_appear_in_errors_or_debug() {
    let mode = ProxyMode::Explicit("http://svc-user:svc-pass-77@127.0.0.1:9".into());
    assert!(!format!("{mode:?}").contains("svc-pass-77"));
    let c = Client::new(ClientConfig {
        proxy: mode,
        retry: RetryPolicy::none(),
        ..ClientConfig::default()
    });
    // Nothing listens on the proxy port, so the call fails; the error must not echo the URL.
    let err = c
        .send(&Request::get("http://app.example.com/"))
        .unwrap_err();
    let text = format!("{err} {err:?}");
    assert!(
        !text.contains("svc-pass-77") && !text.contains("svc-user"),
        "{text}"
    );
}

#[test]
fn environment_mode_is_off_unless_asked_for() {
    // The default never reads the environment, so a process-wide variable cannot change it.
    assert!(matches!(ClientConfig::default().proxy, ProxyMode::None));
    let c = Client::new(ClientConfig::default());
    let err = c.send(&Request::get("http://127.0.0.1:9/")).unwrap_err();
    assert!(matches!(err, HttpError::AddressDenied { .. }), "{err:?}");
}

#[test]
fn non_http_proxies_are_rejected() {
    for url in ["socks5://127.0.0.1:1080", "ftp://x:1", "not a url"] {
        let r = Client::try_new(ClientConfig::default().proxy(ProxyMode::Explicit(url.into())));
        assert!(matches!(r, Err(HttpError::InvalidRequest(_))), "{url}");
    }
}
