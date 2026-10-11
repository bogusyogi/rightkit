//! Outbound address policy against the blocking client: resolver denial per address class,
//! redirect hops, cross-origin header stripping, first-party mode, custom denials, IPv6
//! forms that embed IPv4, and an in-process server reachable only through `allow_if`.
//!
//! Names are resolved by `AddressPolicy::with_resolver`, so no test touches real DNS.

use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rightkit_http::testing::{loopback_policy, MockResponse, MockServer};
use rightkit_http::*;

#[path = "common/refused.rs"]
mod refused;

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

fn client(policy: AddressPolicy, max_redirects: u32) -> Client {
    Client::new(ClientConfig {
        address_policy: policy,
        max_redirects,
        retry: RetryPolicy::none(),
        request_timeout: Duration::from_secs(5),
        response_timeout: Some(Duration::from_secs(5)),
        ..ClientConfig::default()
    })
}

/// Names for the tests; each maps to the address list in the table.
fn dns(table: &'static [(&'static str, &'static [&'static str])]) -> AddressPolicy {
    AddressPolicy::default().with_resolver(move |host| {
        table
            .iter()
            .find(|(name, _)| *name == host)
            .map(|(_, addrs)| addrs.iter().map(|a| ip(a)).collect())
            .unwrap_or_default()
    })
}

struct Denied {
    host: String,
    addr: Option<IpAddr>,
    class: Option<AddressClass>,
    reason: String,
}

fn denied(err: HttpError) -> Denied {
    match err {
        HttpError::AddressDenied {
            host,
            addr,
            class,
            reason,
        } => Denied {
            host,
            addr,
            class,
            reason,
        },
        other => panic!("expected AddressDenied, got {other:?}"),
    }
}

fn get_err(c: &Client, url: &str) -> HttpError {
    c.send(&Request::get(url)).unwrap_err()
}

#[test]
fn default_client_refuses_the_loopback_mock_server() {
    let server = MockServer::start(|_, _| MockResponse::text(200, "ok"));
    let c = Client::new(ClientConfig::default());
    let d = denied(get_err(&c, &server.url("/")));
    assert_eq!(d.class, Some(AddressClass::Loopback));
    assert_eq!(d.addr, Some(ip("127.0.0.1")));
    assert_eq!(server.request_count(), 0, "nothing may reach the server");
}

#[test]
fn resolver_denies_every_address_class() {
    static TABLE: &[(&str, &[&str])] = &[
        ("loop.test", &["127.0.0.1"]),
        ("loop6.test", &["::1"]),
        ("rfc1918-a.test", &["10.1.2.3"]),
        ("rfc1918-b.test", &["172.16.0.9"]),
        ("rfc1918-c.test", &["192.168.1.1"]),
        ("linklocal.test", &["169.254.1.1"]),
        ("metadata-ip.test", &["169.254.169.254"]),
        ("cgnat.test", &["100.64.0.1"]),
        ("multicast.test", &["224.0.0.1"]),
        ("multicast6.test", &["ff02::1"]),
        ("reserved.test", &["240.0.0.1"]),
        ("bench.test", &["198.18.0.1"]),
        ("broadcast.test", &["255.255.255.255"]),
        ("unspecified.test", &["0.0.0.0"]),
        ("unspecified6.test", &["::"]),
        ("ula.test", &["fd00:ec2::254"]),
        ("linklocal6.test", &["fe80::1"]),
    ];
    let expected = [
        ("loop.test", AddressClass::Loopback),
        ("loop6.test", AddressClass::Loopback),
        ("rfc1918-a.test", AddressClass::Private),
        ("rfc1918-b.test", AddressClass::Private),
        ("rfc1918-c.test", AddressClass::Private),
        ("linklocal.test", AddressClass::LinkLocal),
        ("metadata-ip.test", AddressClass::LinkLocal),
        ("cgnat.test", AddressClass::Private),
        ("multicast.test", AddressClass::Multicast),
        ("multicast6.test", AddressClass::Multicast),
        ("reserved.test", AddressClass::Reserved),
        ("bench.test", AddressClass::Reserved),
        ("broadcast.test", AddressClass::Reserved),
        ("unspecified.test", AddressClass::Unspecified),
        ("unspecified6.test", AddressClass::Unspecified),
        ("ula.test", AddressClass::Private),
        ("linklocal6.test", AddressClass::LinkLocal),
    ];
    let c = client(dns(TABLE), 0);
    for (name, class) in expected {
        let d = denied(get_err(&c, &format!("http://{name}:9/")));
        assert_eq!(d.class, Some(class), "{name}");
        assert_eq!(d.host, name);
        assert!(d.addr.is_some(), "{name}");
    }
}

#[test]
fn ipv6_forms_embedding_ipv4_take_the_embedded_class() {
    static TABLE: &[(&str, &[&str])] = &[
        ("mapped-loop.test", &["::ffff:127.0.0.1"]),
        ("mapped-meta.test", &["::ffff:169.254.169.254"]),
        ("mapped-priv.test", &["::ffff:10.0.0.1"]),
        ("compat-loop.test", &["::127.0.0.1"]),
        ("nat64-loop.test", &["64:ff9b::7f00:1"]),
        ("nat64-meta.test", &["64:ff9b::a9fe:a9fe"]),
        ("6to4-priv.test", &["2002:c0a8:0101::1"]),
        ("6to4-loop.test", &["2002:7f00:0001::1"]),
    ];
    let expected = [
        ("mapped-loop.test", AddressClass::Loopback),
        ("mapped-meta.test", AddressClass::LinkLocal),
        ("mapped-priv.test", AddressClass::Private),
        ("compat-loop.test", AddressClass::Loopback),
        ("nat64-loop.test", AddressClass::Loopback),
        ("nat64-meta.test", AddressClass::LinkLocal),
        ("6to4-priv.test", AddressClass::Private),
        ("6to4-loop.test", AddressClass::Loopback),
    ];
    let c = client(dns(TABLE), 0);
    for (name, class) in expected {
        let d = denied(get_err(&c, &format!("http://{name}/")));
        assert_eq!(d.class, Some(class), "{name}");
    }
    // Literal forms in the URL itself.
    for (url, class) in [
        ("http://[::ffff:127.0.0.1]/", AddressClass::Loopback),
        ("http://[::ffff:a9fe:a9fe]/", AddressClass::LinkLocal),
        ("http://[64:ff9b::a00:1]/", AddressClass::Private),
        ("http://[2002:7f00:1::1]/", AddressClass::Loopback),
    ] {
        let d = denied(get_err(&c, url));
        assert_eq!(d.class, Some(class), "{url}");
    }
}

#[test]
fn ip_literals_localhost_and_metadata_names_are_denied() {
    let c = client(AddressPolicy::default(), 0);
    for (url, class) in [
        ("http://127.0.0.1:9/", AddressClass::Loopback),
        ("http://[::1]:9/", AddressClass::Loopback),
        ("http://10.0.0.1/", AddressClass::Private),
        (
            "http://169.254.169.254/latest/meta-data/",
            AddressClass::LinkLocal,
        ),
        ("http://localhost:9/", AddressClass::Loopback),
        ("http://app.localhost:9/", AddressClass::Loopback),
        ("http://LOCALHOST.:9/", AddressClass::Loopback),
        (
            "http://metadata.google.internal/computeMetadata/v1/",
            AddressClass::LinkLocal,
        ),
        ("http://instance-data/latest/", AddressClass::LinkLocal),
    ] {
        let d = denied(get_err(&c, url));
        assert_eq!(d.class, Some(class), "{url}");
    }
}

#[test]
fn numeric_host_spellings_never_reach_loopback() {
    // The platform resolver may or may not expand these; either way no connection is made
    // to loopback. A refusal must be the policy's, or a failed lookup.
    let server = MockServer::start(|_, _| MockResponse::text(200, "ok"));
    let c = client(AddressPolicy::default(), 0);
    for url in [
        "http://2130706433/",
        "http://127.1/",
        "http://0x7f.0.0.1/",
        "http://0177.0.0.1/",
    ] {
        match c.send(&Request::get(url)).unwrap_err() {
            HttpError::AddressDenied { class, .. } => {
                assert_eq!(class, Some(AddressClass::Loopback), "{url}")
            }
            HttpError::Transport { .. } | HttpError::Timeout { .. } => {}
            other => panic!("{url}: unexpected {other:?}"),
        }
    }
    assert_eq!(server.request_count(), 0);
}

#[test]
fn names_that_resolve_to_nothing_are_denied() {
    static TABLE: &[(&str, &[&str])] = &[];
    let c = client(dns(TABLE), 0);
    let d = denied(get_err(&c, "http://nothing.test/"));
    assert_eq!(d.addr, None);
    assert_eq!(d.class, None);
    assert!(d.reason.contains("did not resolve"), "{}", d.reason);
}

#[test]
fn one_denied_address_denies_the_whole_host() {
    static TABLE: &[(&str, &[&str])] = &[
        ("mixed.test", &["93.184.216.34", "10.0.0.1"]),
        ("mixed6.test", &["2606:4700:4700::1111", "fd00::1"]),
    ];
    let c = client(dns(TABLE), 0);
    let d = denied(get_err(&c, "http://mixed.test/"));
    assert_eq!(d.addr, Some(ip("10.0.0.1")));
    assert_eq!(d.class, Some(AddressClass::Private));
    let d = denied(get_err(&c, "http://mixed6.test/"));
    assert_eq!(d.addr, Some(ip("fd00::1")));
}

#[test]
fn connection_uses_the_vetted_address_not_a_second_lookup() {
    // `pinned.test` exists only in the policy's resolver. The request reaches the mock
    // server at 127.0.0.1, so the socket opened to exactly the address the policy returned.
    let server = MockServer::start(|_, _| MockResponse::text(200, "via-vetted-address"));
    static TABLE: &[(&str, &[&str])] = &[("pinned.test", &["127.0.0.1"])];
    let policy = dns(TABLE).allow_if(vec!["pinned.test".into()], |a| a.is_loopback());
    let c = client(policy, 0);
    let resp = c
        .send(&Request::get(format!(
            "http://pinned.test:{}/p",
            server.port()
        )))
        .unwrap();
    assert_eq!(resp.text(), "via-vetted-address");
    let seen = &server.requests()[0];
    assert_eq!(
        seen.header("host"),
        Some(format!("pinned.test:{}", server.port()).as_str())
    );
}

#[test]
fn allow_if_applies_only_to_listed_hosts_and_never_to_link_local() {
    let server = MockServer::start(|_, _| MockResponse::text(200, "ok"));
    let policy = AddressPolicy::default().allow_if(vec!["127.0.0.1".into()], |a| a.is_loopback());
    let c = client(policy, 0);
    assert_eq!(c.send(&Request::get(server.url("/"))).unwrap().status, 200);
    // `localhost` is loopback too, but it is not the listed host.
    let d = denied(get_err(&c, &format!("http://localhost:{}/", server.port())));
    assert_eq!(d.class, Some(AddressClass::Loopback));
    // The predicate narrows it: a listed host with a non-matching address is still denied.
    let narrow = AddressPolicy::default()
        .with_resolver(|_| vec![ip("10.0.0.1")])
        .allow_if(vec!["lan.test".into()], |a| a.is_loopback());
    let d = denied(get_err(&client(narrow, 0), "http://lan.test/"));
    assert_eq!(d.class, Some(AddressClass::Private));
    // Link-local and metadata cannot be allowed by any hook.
    let all = AddressPolicy::default().allow_if(
        vec!["169.254.169.254".into(), "metadata.google.internal".into()],
        |_| true,
    );
    let c = client(all, 0);
    for url in [
        "http://169.254.169.254/",
        "http://metadata.google.internal/",
    ] {
        let d = denied(get_err(&c, url));
        assert_eq!(d.class, Some(AddressClass::LinkLocal), "{url}");
    }
}

#[test]
fn deny_ranges_beat_allow_if_and_cover_embedded_ipv4() {
    let server = MockServer::start(|_, _| MockResponse::text(200, "ok"));
    let nets: Vec<IpNet> = vec!["127.0.0.0/8".parse().unwrap()];
    let c = client(loopback_policy().deny_ranges(nets), 0);
    let d = denied(get_err(&c, &server.url("/")));
    assert!(d.reason.contains("127.0.0.0/8"), "{}", d.reason);
    assert_eq!(server.request_count(), 0);

    // A public address can be denied by range, and an IPv6 form is tested by its embedded
    // IPv4 address against an IPv4 range.
    let policy = AddressPolicy::default()
        .with_resolver(|host| match host {
            "pub.test" => vec![ip("8.8.8.8")],
            "embed.test" => vec![ip("::ffff:10.1.2.3")],
            _ => vec![],
        })
        .allow_if(vec!["embed.test".into()], |_| true)
        .deny_ranges(vec![
            "8.8.8.0/24".parse().unwrap(),
            "10.0.0.0/8".parse().unwrap(),
        ]);
    let c = client(policy, 0);
    let d = denied(get_err(&c, "http://pub.test/"));
    assert_eq!(d.class, Some(AddressClass::Public));
    assert!(d.reason.contains("8.8.8.0/24"), "{}", d.reason);
    let d = denied(get_err(&c, "http://embed.test/"));
    assert!(d.reason.contains("10.0.0.0/8"), "{}", d.reason);
}

#[test]
fn deny_if_supplies_the_error_text_and_sees_embedded_ipv4() {
    let policy = AddressPolicy::default()
        .with_resolver(|host| match host {
            "pub.test" => vec![ip("8.8.4.4")],
            "embed.test" => vec![ip("::ffff:93.184.216.34")],
            _ => vec![],
        })
        .deny_if(|a| {
            (*a == ip("8.8.4.4")).then(|| "resolver pair is blocked by app policy".to_string())
        })
        .deny_if(|a| {
            a.is_ipv4()
                .then(|| format!("v4 {a} denied"))
                .filter(|_| *a == ip("93.184.216.34"))
        });
    let c = client(policy, 0);
    let d = denied(get_err(&c, "http://pub.test/"));
    assert_eq!(d.reason, "resolver pair is blocked by app policy");
    assert_eq!(d.addr, Some(ip("8.8.4.4")));
    let d = denied(get_err(&c, "http://embed.test/"));
    assert_eq!(d.reason, "v4 93.184.216.34 denied");
    assert_eq!(d.addr, Some(ip("::ffff:93.184.216.34")));
}

#[test]
fn redirect_to_a_denied_address_is_refused_before_connecting() {
    let hits = Arc::new(Mutex::new(0usize));
    let counter = hits.clone();
    let target = MockServer::start(move |_, _| {
        *counter.lock().unwrap() += 1;
        MockResponse::text(200, "secret-target")
    });
    let policy = loopback_policy();
    let c = client(policy, 5);

    // Link-local metadata endpoint.
    let a = MockServer::start(|_, _| {
        MockResponse::new(302).header("Location", "http://169.254.169.254/latest/meta-data/")
    });
    let d = denied(get_err(&c, &a.url("/start")));
    assert_eq!(d.class, Some(AddressClass::LinkLocal));
    assert_eq!(d.host, "169.254.169.254");

    // Loopback under a host the caller did not list (`localhost`, not `127.0.0.1`).
    let port = target.port();
    let a = MockServer::start(move |_, _| {
        MockResponse::new(301).header("Location", &format!("http://localhost:{port}/x"))
    });
    let c2 = client(
        AddressPolicy::default().allow_if(vec!["127.0.0.1".into()], |a| a.is_loopback()),
        5,
    );
    let d = denied(get_err(&c2, &a.url("/start")));
    assert_eq!(d.class, Some(AddressClass::Loopback));
    assert_eq!(
        *hits.lock().unwrap(),
        0,
        "the redirect target must never be contacted"
    );

    // Private address behind a name, on the second hop of a chain.
    let b =
        MockServer::start(|_, _| MockResponse::new(302).header("Location", "http://lan.test/p"));
    let b_url = b.url("/");
    let a = MockServer::start(move |_, _| MockResponse::new(302).header("Location", &b_url));
    let policy = loopback_policy().with_resolver(|h| {
        if h == "lan.test" {
            vec![ip("192.168.0.20")]
        } else {
            vec![]
        }
    });
    let d = denied(get_err(&client(policy, 5), &a.url("/")));
    assert_eq!(d.class, Some(AddressClass::Private));
    assert_eq!(d.host, "lan.test");
    assert_eq!(a.request_count(), 1);
    assert_eq!(b.request_count(), 1);
}

#[test]
fn redirects_follow_up_to_the_limit() {
    let server = MockServer::start(|req, _| match req.path() {
        "/a" => MockResponse::new(302).header("Location", "/b"),
        "/b" => MockResponse::new(301).header("Location", "c"),
        "/c" => MockResponse::text(200, "landed"),
        _ => MockResponse::text(404, "no"),
    });
    // Enough hops.
    let resp = client(loopback_policy(), 2)
        .send(&Request::get(server.url("/a")))
        .unwrap();
    assert_eq!(resp.text(), "landed");
    let paths: Vec<String> = server
        .requests()
        .iter()
        .map(|r| r.path().to_string())
        .collect();
    assert_eq!(paths, ["/a", "/b", "/c"]);

    // One hop short, error variant.
    let err = client(loopback_policy(), 1)
        .send(&Request::get(server.url("/a")))
        .unwrap_err();
    assert!(
        matches!(&err, HttpError::Transport { message, connect_phase: false, .. } if message == "too many redirects"),
        "{err:?}"
    );

    // One hop short, return-the-3xx variant.
    let c = Client::new(ClientConfig {
        address_policy: loopback_policy(),
        max_redirects: 1,
        max_redirects_will_error: false,
        retry: RetryPolicy::none(),
        ..ClientConfig::default()
    });
    let resp = c.send_any(&Request::get(server.url("/a"))).unwrap();
    assert_eq!(resp.status, 301);

    // Zero never follows.
    let before = server.request_count();
    let resp = client(loopback_policy(), 0)
        .send_any(&Request::get(server.url("/a")))
        .unwrap();
    assert_eq!(resp.status, 302);
    assert_eq!(server.request_count(), before + 1);
}

#[test]
fn redirect_method_and_body_rules() {
    let server = MockServer::start(|req, _| match (req.method.as_str(), req.path()) {
        (_, "/to302") => MockResponse::new(302).header("Location", "/final"),
        (_, "/to307") => MockResponse::new(307).header("Location", "/final"),
        _ => MockResponse::text(200, "final"),
    });
    let c = client(loopback_policy(), 3);
    // POST + 302 becomes a bodiless GET.
    let resp = c
        .send(
            &Request::post(server.url("/to302"))
                .json(&serde_json::json!({"a": 1}))
                .unwrap(),
        )
        .unwrap();
    assert_eq!(resp.text(), "final");
    let seen = &server.requests();
    assert_eq!(seen[1].method, "GET");
    assert!(seen[1].body.is_empty());
    assert_eq!(seen[1].header("content-type"), None);
    // POST + 307 is not followed (a body must not be replayed); the 3xx comes back.
    let resp = c
        .send_any(&Request::post(server.url("/to307")).body(b"payload".to_vec()))
        .unwrap();
    assert_eq!(resp.status, 307);
    assert_eq!(server.request_count(), 3);
    // GET + 307 keeps the method.
    let resp = c.send(&Request::get(server.url("/to307"))).unwrap();
    assert_eq!(resp.text(), "final");
}

#[test]
fn cross_origin_hops_drop_credentials_and_same_origin_hops_keep_them() {
    let b = MockServer::start(|_, _| MockResponse::text(200, "at-b"));
    let b_port = b.port();
    let a = MockServer::start(move |req, _| match req.path() {
        "/cross" => MockResponse::new(302)
            .header("Location", &format!("http://b.test:{b_port}/landing?x=1")),
        "/same" => MockResponse::new(302).header("Location", "/landing"),
        _ => MockResponse::text(200, "at-a"),
    });
    static TABLE: &[(&str, &[&str])] = &[("b.test", &["127.0.0.1"])];
    let policy = dns(TABLE).allow_if(vec!["127.0.0.1".into(), "b.test".into()], |x| {
        x.is_loopback()
    });
    let c = client(policy, 3);
    let build = |path: &str| {
        Request::get(a.url(path))
            .bearer("bearer-value-1")
            .secret_header("X-Service-Key", "service-value-2")
            .secret_header("X-Opaque", "opaque-value-3")
            .header("Cookie", "sid=4")
            .header("Accept", "text/plain")
    };

    let resp = c.send(&build("/cross")).unwrap();
    assert_eq!(resp.text(), "at-b");
    let landed = &b.requests()[0];
    assert_eq!(landed.path(), "/landing");
    assert_eq!(landed.header("authorization"), None);
    assert_eq!(landed.header("x-service-key"), None);
    assert_eq!(landed.header("x-opaque"), None);
    assert_eq!(landed.header("cookie"), None);
    assert_eq!(landed.header("accept"), Some("text/plain"));
    // The first hop carried everything.
    let first = &a.requests()[0];
    assert_eq!(first.header("authorization"), Some("Bearer bearer-value-1"));
    assert_eq!(first.header("x-service-key"), Some("service-value-2"));

    let resp = c.send(&build("/same")).unwrap();
    assert_eq!(resp.text(), "at-a");
    let landed = a
        .requests()
        .into_iter()
        .find(|r| r.path() == "/landing")
        .unwrap();
    assert_eq!(
        landed.header("authorization"),
        Some("Bearer bearer-value-1")
    );
    assert_eq!(landed.header("x-service-key"), Some("service-value-2"));
}

#[test]
fn a_header_holding_a_recorded_value_is_stripped_cross_origin_whatever_its_name() {
    let b = MockServer::start(|_, _| MockResponse::text(200, "at-b"));
    let b_port = b.port();
    let a = MockServer::start(move |_, _| {
        MockResponse::new(302).header("Location", &format!("http://b.test:{b_port}/"))
    });
    static TABLE: &[(&str, &[&str])] = &[("b.test", &["127.0.0.1"])];
    let policy = dns(TABLE).allow_if(vec!["127.0.0.1".into(), "b.test".into()], |x| {
        x.is_loopback()
    });
    let config = ClientConfig {
        address_policy: policy,
        max_redirects: 2,
        retry: RetryPolicy::none(),
        ..ClientConfig::default()
    }
    .redact_values(vec!["configured-value-9".to_string()]);
    let c = Client::new(config);
    c.send(&Request::get(a.url("/")).header("X-Whatever", "prefix configured-value-9 suffix"))
        .unwrap();
    assert_eq!(b.requests()[0].header("x-whatever"), None);
}

#[test]
fn first_party_pins_hosts_and_requires_https() {
    static TABLE: &[(&str, &[&str])] = &[
        ("license.test", &["127.0.0.1"]),
        ("evil.test", &["93.184.216.34"]),
        ("meta.test", &["169.254.169.254"]),
    ];
    let policy = AddressPolicy::first_party(vec!["License.Test".into()])
        .allow_private_resolution("LICENSE.test")
        .with_resolver(|host| dns_lookup(TABLE, host));
    let c = client(policy, 0);

    // Plain HTTP to a listed host.
    let d = denied(get_err(&c, "http://license.test/"));
    assert!(d.reason.contains("HTTPS only"), "{}", d.reason);
    // An unlisted host, even a perfectly public one, and no lookup is needed to refuse it.
    let d = denied(get_err(&c, "https://evil.test/"));
    assert!(d.reason.contains("first-party host list"), "{}", d.reason);
    // Suffix and prefix look-alikes are not matches.
    for url in [
        "https://sub.license.test/",
        "https://license.test.evil.test/",
        "https://xlicense.test/",
    ] {
        let d = denied(get_err(&c, url));
        assert!(
            d.reason.contains("first-party host list"),
            "{url}: {}",
            d.reason
        );
    }
    // A listed host that is opted in for private resolution passes: it resolves to loopback
    // here and proceeds to the connection (refused or failing TLS, not denied by the policy).
    let err = get_err(&c, "https://LICENSE.test.:1/");
    assert!(!matches!(err, HttpError::AddressDenied { .. }), "{err:?}");
}

fn dns_lookup(table: &[(&str, &[&str])], host: &str) -> Vec<IpAddr> {
    table
        .iter()
        .find(|(name, _)| *name == host)
        .map(|(_, addrs)| addrs.iter().map(|a| ip(a)).collect())
        .unwrap_or_default()
}

#[test]
fn first_party_still_refuses_link_local_and_honours_custom_denials() {
    static TABLE: &[(&str, &[&str])] = &[
        ("meta.test", &["169.254.169.254"]),
        ("lan.test", &["10.9.9.9"]),
    ];
    let policy = AddressPolicy::first_party(vec!["meta.test".into(), "lan.test".into()])
        .with_resolver(|host| dns_lookup(TABLE, host))
        .deny_ranges(vec!["10.9.0.0/16".parse().unwrap()]);
    let c = client(policy, 0);
    let d = denied(get_err(&c, "https://meta.test/"));
    assert_eq!(d.class, Some(AddressClass::LinkLocal));
    let d = denied(get_err(&c, "https://lan.test/"));
    assert!(d.reason.contains("10.9.0.0/16"), "{}", d.reason);
}

#[test]
fn first_party_constructors_use_the_first_party_policy() {
    let config = ClientConfig::first_party(vec!["api.example.com".into()]);
    assert!(config.address_policy.is_first_party());
    let c = Client::first_party(vec!["api.example.com".into()]);
    assert!(c.config().address_policy.is_first_party());
    let d = denied(get_err(&c, "https://other.example.com/"));
    assert!(d.reason.contains("first-party host list"));
    assert!(!ClientConfig::default().address_policy.is_first_party());
}

#[test]
fn address_denied_text_never_carries_the_url_query_or_userinfo() {
    let c = client(AddressPolicy::default(), 0);
    let err = get_err(
        &c,
        "http://user:pass-word-77@10.0.0.1/path?api_key=KEY-ABC-123&b=2",
    );
    for text in [err.to_string(), format!("{err:?}")] {
        for needle in ["KEY-ABC-123", "pass-word-77", "api_key", "user:"] {
            assert!(!text.contains(needle), "{needle} leaked in {text}");
        }
    }
    let d = denied(err);
    assert_eq!(d.host, "10.0.0.1");

    // Custom deny text is scrubbed of configured values too.
    let policy = AddressPolicy::default()
        .with_resolver(|_| vec![ip("93.184.216.34")])
        .deny_if(|_| Some("blocked while using token tok-abc-999".to_string()));
    let c = Client::new(
        ClientConfig {
            address_policy: policy,
            retry: RetryPolicy::none(),
            ..ClientConfig::default()
        }
        .redact_values(vec!["tok-abc-999".into()]),
    );
    let err = get_err(&c, "http://pub.test/");
    assert!(!format!("{err} {err:?}").contains("tok-abc-999"));
}

#[test]
fn errors_events_and_debug_never_echo_a_url_query_or_userinfo() {
    // Retry events and status errors from a real server, with the key in the query.
    let server = MockServer::start(|_, _| MockResponse::text(503, "busy"));
    let events = Arc::new(Mutex::new(Vec::<RetryEvent>::new()));
    let sink = events.clone();
    let c = Client::new(ClientConfig {
        address_policy: loopback_policy(),
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
        ..ClientConfig::default()
    });
    let req = Request::get(format!("{}?key=QUERY-KEY-1&z=9", server.url("/v1/data")));
    let err = c.send(&req).unwrap_err();
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].url,
        format!("{}?[redacted]", server.url("/v1/data"))
    );
    for text in [
        err.to_string(),
        format!("{err:?}"),
        format!("{:?}", events[0]),
        format!("{req:?}"),
    ] {
        assert!(
            !text.contains("QUERY-KEY-1") && !text.contains("z=9"),
            "{text}"
        );
    }
    assert!(format!("{req:?}").contains("?[redacted]"));

    // Connection failure: nothing listens on the port; ureq's text must not echo the URL.
    let refused = refused::RefusedPort::hold();
    let closed = refused.port();
    let c = client(loopback_policy(), 0);
    let err = c
        .send(&Request::get(format!(
            "http://user:userinfo-pass-5@127.0.0.1:{closed}/p?key=QUERY-KEY-2"
        )))
        .unwrap_err();
    let text = format!("{err} {err:?}");
    assert!(
        !text.contains("QUERY-KEY-2") && !text.contains("userinfo-pass-5"),
        "{text}"
    );

    // Unparsable URL: InvalidRequest must not echo it.
    let err = c
        .send(&Request::get("http://exa mple.com/?key=QUERY-KEY-3"))
        .unwrap_err();
    assert!(matches!(err, HttpError::InvalidRequest(_)), "{err:?}");
    assert!(!format!("{err} {err:?}").contains("QUERY-KEY-3"));
    // So must a redirect whose Location carries a key into an unusable target.
    let a = MockServer::start(|_, _| {
        MockResponse::new(302).header("Location", "ftp://files.example/?key=QUERY-KEY-4")
    });
    let c = client(loopback_policy(), 3);
    let err = get_err(&c, &a.url("/"));
    assert!(matches!(err, HttpError::InvalidRequest(_)), "{err:?}");
    assert!(!format!("{err} {err:?}").contains("QUERY-KEY-4"));
}

#[test]
fn redirect_chain_error_after_a_key_bearing_hop_is_scrubbed() {
    // The second hop's URL carries a key; the error from hop two must not echo it.
    let b = MockServer::start(|_, _| {
        MockResponse::new(302).header("Location", "http://169.254.169.254/?key=HOP-KEY-7")
    });
    let b_url = format!("{}?key=HOP-KEY-6", b.url("/"));
    let a = MockServer::start(move |_, _| MockResponse::new(302).header("Location", &b_url));
    let err = get_err(&client(loopback_policy(), 4), &a.url("/"));
    let d = denied(err);
    assert_eq!(d.class, Some(AddressClass::LinkLocal));
    assert!(!format!("{}{}", d.host, d.reason).contains("HOP-KEY"));
}

#[test]
fn download_obeys_the_address_policy_too() {
    let server = MockServer::start(|_, _| MockResponse::text(200, "data"));
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("address-policy-dl");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let c = Client::new(ClientConfig {
        retry: RetryPolicy::none(),
        ..ClientConfig::default()
    });
    let err = c
        .download_to_path(
            &server.url("/f"),
            &dir.join("f.bin"),
            DownloadOptions::default(),
        )
        .unwrap_err();
    match err {
        DownloadError::Http(HttpError::AddressDenied { class, .. }) => {
            assert_eq!(class, Some(AddressClass::Loopback))
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(server.request_count(), 0);
}

#[test]
fn first_party_hosts_resolving_to_private_addresses_are_denied_by_default() {
    static TABLE: &[(&str, &[&str])] = &[
        ("onprem.test", &["10.20.30.40"]),
        ("loop.test", &["127.0.0.1"]),
        ("fine.test", &["93.184.216.34"]),
        ("mixed.test", &["93.184.216.34", "192.168.1.9"]),
        ("meta.test", &["169.254.169.254"]),
    ];
    let hosts = || -> Vec<String> {
        [
            "onprem.test",
            "loop.test",
            "fine.test",
            "mixed.test",
            "meta.test",
        ]
        .iter()
        .map(|h| h.to_string())
        .collect()
    };
    let c = client(
        AddressPolicy::first_party(hosts()).with_resolver(|h| dns_lookup(TABLE, h)),
        0,
    );
    let d = denied(get_err(&c, "https://onprem.test/"));
    assert_eq!(d.class, Some(AddressClass::Private));
    assert_eq!(d.addr, Some(ip("10.20.30.40")));
    let d = denied(get_err(&c, "https://loop.test/"));
    assert_eq!(d.class, Some(AddressClass::Loopback));
    let d = denied(get_err(&c, "https://mixed.test/"));
    assert_eq!(d.addr, Some(ip("192.168.1.9")));

    // A public address passes the policy (the connection itself then fails or succeeds on
    // the network, which this test does not depend on: only the policy verdict matters).
    let verdict = AddressPolicy::first_party(hosts())
        .with_resolver(|h| dns_lookup(TABLE, h))
        .resolve_vetted("fine.test", 443);
    assert!(verdict.is_ok());

    // Opting one host out: only that host, and never link-local.
    let policy = AddressPolicy::first_party(hosts())
        .allow_private_resolution("onprem.test")
        .allow_private_resolution("meta.test")
        .with_resolver(|h| dns_lookup(TABLE, h));
    assert!(policy.resolve_vetted("onprem.test", 443).is_ok());
    assert!(policy.resolve_vetted("loop.test", 443).is_err());
    assert!(matches!(
        policy.resolve_vetted("meta.test", 443),
        Err(HttpError::AddressDenied {
            class: Some(AddressClass::LinkLocal),
            ..
        })
    ));
    // A custom denial still applies to an opted-in host.
    let policy = policy.deny_ranges(vec!["10.20.0.0/16".parse().unwrap()]);
    assert!(policy.resolve_vetted("onprem.test", 443).is_err());
    // Opting a host out has no effect outside first-party mode.
    let public = AddressPolicy::default()
        .allow_private_resolution("onprem.test")
        .with_resolver(|h| dns_lookup(TABLE, h));
    assert!(public.resolve_vetted("onprem.test", 443).is_err());
}

#[test]
fn resolve_vetted_gives_other_transports_the_same_policy() {
    static TABLE: &[(&str, &[&str])] = &[
        ("ok.test", &["93.184.216.34", "2606:4700:4700::1111"]),
        ("priv.test", &["10.0.0.7"]),
        ("hooked.test", &["8.8.4.4"]),
        ("none.test", &[]),
    ];
    let policy = AddressPolicy::default()
        .with_resolver(|h| dns_lookup(TABLE, h))
        .deny_if(|a| (*a == ip("8.8.4.4")).then(|| "blocked by app".to_string()));

    // Allowed: every address comes back with the port.
    let addrs = policy.resolve_vetted("ok.test", 8443).unwrap();
    assert_eq!(
        addrs,
        vec![
            std::net::SocketAddr::new(ip("93.184.216.34"), 8443),
            std::net::SocketAddr::new(ip("2606:4700:4700::1111"), 8443),
        ]
    );
    // Literals and the case/trailing-dot spellings of a name.
    assert_eq!(
        policy.resolve_vetted("93.184.216.34", 1).unwrap(),
        vec![std::net::SocketAddr::new(ip("93.184.216.34"), 1)]
    );
    assert!(policy.resolve_vetted("OK.test.", 1).is_ok());

    // Denied by class, with the same typed error the clients return.
    match policy.resolve_vetted("priv.test", 443).unwrap_err() {
        HttpError::AddressDenied {
            host, addr, class, ..
        } => {
            assert_eq!(host, "priv.test");
            assert_eq!(addr, Some(ip("10.0.0.7")));
            assert_eq!(class, Some(AddressClass::Private));
        }
        other => panic!("{other:?}"),
    }
    for literal in [
        "127.0.0.1",
        "169.254.169.254",
        "::ffff:10.0.0.1",
        "localhost",
    ] {
        assert!(
            matches!(
                policy.resolve_vetted(literal, 1),
                Err(HttpError::AddressDenied { .. })
            ),
            "{literal}"
        );
    }
    // Denied by hook, with the hook's text.
    match policy.resolve_vetted("hooked.test", 443).unwrap_err() {
        HttpError::AddressDenied { reason, .. } => assert_eq!(reason, "blocked by app"),
        other => panic!("{other:?}"),
    }
    // No address at all.
    assert!(matches!(
        policy.resolve_vetted("none.test", 443),
        Err(HttpError::AddressDenied { addr: None, .. })
    ));
    // Metadata names and an allow_if escape follow the client rules.
    assert!(policy
        .resolve_vetted("metadata.google.internal", 80)
        .is_err());
    let open = AddressPolicy::default()
        .with_resolver(|h| dns_lookup(TABLE, h))
        .allow_if(vec!["priv.test".into()], |a| *a == ip("10.0.0.7"));
    assert_eq!(
        open.resolve_vetted("priv.test", 80).unwrap(),
        vec![std::net::SocketAddr::new(ip("10.0.0.7"), 80)]
    );
    // First-party rules: host pin, and the public-class check on the listed host.
    let fp = AddressPolicy::first_party(vec!["priv.test".into(), "ok.test".into()])
        .with_resolver(|h| dns_lookup(TABLE, h));
    assert!(fp.resolve_vetted("ok.test", 443).is_ok());
    assert!(fp.resolve_vetted("priv.test", 443).is_err());
    assert!(fp.resolve_vetted("hooked.test", 443).is_err());
}
