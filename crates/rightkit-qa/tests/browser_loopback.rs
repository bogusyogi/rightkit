//! Loopback allowance for the QA browser target: scoped to one origin, never a global disable.
#![cfg(feature = "browser")]

use rightkit_browser::NetworkPolicy;
use rightkit_qa::browser::{allow_loopback_origin, BrowserTarget, LoopbackOrigin};
use rightkit_qa::mock::{MockServer, Reply, Request};

fn check(p: &NetworkPolicy, url: &str, kind: &str) -> bool {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(p.check(url, kind))
        .is_ok()
}

#[test]
fn origin_parse_accepts_only_loopback_hosts() {
    let o = LoopbackOrigin::parse("http://127.0.0.1:5173/app?x=1").unwrap();
    assert_eq!(
        (o.scheme.as_str(), o.host.as_str(), o.port),
        ("http", "127.0.0.1", 5173)
    );
    assert_eq!(
        LoopbackOrigin::parse("https://LOCALHOST.").unwrap().port,
        443
    );
    assert_eq!(
        LoopbackOrigin::parse("http://[::1]:8080").unwrap().host,
        "::1"
    );
    assert!(LoopbackOrigin::parse("http://app.localhost:3000").is_ok());
    for bad in [
        "http://10.0.0.5:5173",
        "http://example.com:5173",
        "ftp://127.0.0.1:21",
        "127.0.0.1:5173",
        "http://user@127.0.0.1:5173",
        "http://127.0.0.1:notaport",
    ] {
        assert!(LoopbackOrigin::parse(bad).is_err(), "{bad} must be refused");
    }
}

#[test]
fn policy_allows_exactly_the_given_origin() {
    let p = allow_loopback_origin(NetworkPolicy::default(), "http://127.0.0.1:5173").unwrap();
    assert!(!p.unrestricted, "never a global disable");
    assert!(
        p.allow_classes.is_empty(),
        "loopback is not allowed as a class"
    );
    assert!(check(&p, "http://127.0.0.1:5173/", "Document"));
    assert!(check(&p, "http://127.0.0.1:5173/src/main.ts", "Script"));
    assert!(
        check(&p, "ws://127.0.0.1:5173/@vite/client", "WebSocket"),
        "dev-server HMR"
    );
    assert!(
        !check(&p, "http://127.0.0.1:5174/", "Document"),
        "other port"
    );
    assert!(
        !check(&p, "https://127.0.0.1:5173/", "Document"),
        "other scheme"
    );
    assert!(
        !check(&p, "http://localhost:5173/", "Document"),
        "other host is another origin"
    );
    assert!(
        !check(&p, "http://10.0.0.1:5173/", "Document"),
        "private range still blocked"
    );
    assert!(
        !check(&p, "http://169.254.169.254:5173/", "Document"),
        "metadata hard block"
    );
    // Defaults still block plain loopback.
    assert!(!check(
        &NetworkPolicy::default(),
        "http://127.0.0.1:5173/",
        "Document"
    ));
    // Chaining keeps both origins and nothing else.
    let p = allow_loopback_origin(p, "http://localhost:8787").unwrap();
    assert!(check(&p, "http://127.0.0.1:5173/", "Document"));
    assert!(check(&p, "http://localhost:8787/api", "Fetch"));
    assert!(!check(&p, "http://localhost:5173/", "Document"));
    assert!(allow_loopback_origin(NetworkPolicy::default(), "http://192.168.1.2:80").is_err());
}

#[test]
fn chrome_reaches_its_own_dev_server_and_nothing_else_on_loopback() {
    if let Err(e) = rightkit_browser::chrome_for_testing_path(None) {
        println!("[qa] SKIP live browser loopback: {e}");
        return;
    }
    let page = |title: &'static str| {
        move |_: &Request, _: &str| {
            Reply::bytes(
                "text/html; charset=utf-8",
                format!("<html><head><title>{title}</title></head><body>{title}</body></html>"),
            )
        }
    };
    let dev = MockServer::start_with(page("dev-server")).unwrap();
    let other = MockServer::start_with(page("other")).unwrap();
    let mut b = BrowserTarget::launch_for_origins(false, (800, 600), &[&dev.base]).unwrap();
    b.goto(&dev.url("/")).unwrap();
    let text = b.block_on(b.page().unwrap().text()).unwrap();
    assert!(text.contains("dev-server"), "{text}");
    let denied = b.goto(&other.url("/"));
    assert!(denied.is_err(), "a different loopback port stays blocked");
    assert!(
        other.seen().is_empty(),
        "the blocked server never saw a request"
    );
    b.stop();
}
