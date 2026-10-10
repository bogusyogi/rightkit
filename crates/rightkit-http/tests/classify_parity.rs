//! `rightkit_http::netpolicy` mirrors `rightkit-browser`'s address classification. This test
//! reads the browser crate's *source* (no dependency on the crate) and checks every address
//! its own classification tests expect against this crate's `classify_ip`. When the browser
//! source is absent (a packaged crate), the table below still runs on its own.

use std::net::IpAddr;
use std::path::PathBuf;

use rightkit_http::netpolicy::{classify_ip, embedded_v4};
use rightkit_http::AddressClass;

fn class_named(name: &str) -> AddressClass {
    match name {
        "Public" => AddressClass::Public,
        "Loopback" => AddressClass::Loopback,
        "LinkLocal" => AddressClass::LinkLocal,
        "Private" => AddressClass::Private,
        "Reserved" => AddressClass::Reserved,
        "Unspecified" => AddressClass::Unspecified,
        "Multicast" => AddressClass::Multicast,
        other => panic!("unknown class {other}"),
    }
}

/// Body text of the test function `name`, up to the next `#[test]`.
fn test_body<'a>(source: &'a str, name: &str) -> &'a str {
    let start = source
        .find(&format!("fn {name}"))
        .unwrap_or_else(|| panic!("browser test {name} not found"));
    let rest = &source[start..];
    let end = rest[1..]
        .find("#[test]")
        .map(|i| i + 1)
        .unwrap_or(rest.len());
    &rest[..end]
}

/// `("addr", AddressClass::X)` rows and bare `"addr",` rows (expected `Public`).
fn expectations(body: &str) -> Vec<(String, AddressClass)> {
    let mut out = Vec::new();
    for line in body.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("(\"") {
            let Some((addr, tail)) = rest.split_once('"') else {
                continue;
            };
            let Some(class) = tail.split("AddressClass::").nth(1) else {
                continue;
            };
            let class: String = class.chars().take_while(|c| c.is_alphanumeric()).collect();
            out.push((addr.to_string(), class_named(&class)));
        } else if let Some(rest) = t.strip_prefix('"') {
            if let Some(addr) = rest.strip_suffix("\",") {
                if addr.parse::<IpAddr>().is_ok() {
                    out.push((addr.to_string(), AddressClass::Public));
                }
            }
        }
    }
    out
}

#[test]
fn matches_the_browser_crates_expectations() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../rightkit-browser/src/policy.rs");
    let Ok(source) = std::fs::read_to_string(&path) else {
        eprintln!("rightkit-browser source not found at {path:?}; skipping the cross-check");
        return;
    };
    let v4 = expectations(test_body(&source, "classifies_every_blocked_v4_range"));
    let v6 = expectations(test_body(
        &source,
        "classifies_every_blocked_v6_range_and_embedded_v4",
    ));
    // Guard against the parse silently matching nothing.
    assert!(v4.len() >= 25, "parsed only {} v4 expectations", v4.len());
    assert!(v6.len() >= 20, "parsed only {} v6 expectations", v6.len());
    assert!(v4.iter().any(|(_, c)| *c == AddressClass::Public));
    assert!(v6.iter().any(|(_, c)| *c == AddressClass::Public));
    for (addr, want) in v4.into_iter().chain(v6) {
        let ip: IpAddr = addr.parse().unwrap();
        assert_eq!(classify_ip(ip), want, "{addr}");
    }
}

#[test]
fn own_table_covers_every_denied_range_and_embedded_form() {
    use AddressClass::*;
    let cases = [
        ("127.0.0.1", Loopback),
        ("127.255.255.254", Loopback),
        ("0.0.0.0", Unspecified),
        ("0.1.2.3", Unspecified),
        ("169.254.169.254", LinkLocal),
        ("169.254.0.1", LinkLocal),
        ("10.0.0.1", Private),
        ("172.16.0.1", Private),
        ("172.31.255.255", Private),
        ("192.168.0.1", Private),
        ("100.64.0.1", Private),
        ("100.127.255.255", Private),
        ("224.0.0.1", Multicast),
        ("239.255.255.255", Multicast),
        ("240.0.0.1", Reserved),
        ("255.255.255.255", Reserved),
        ("192.0.0.1", Reserved),
        ("192.0.2.1", Reserved),
        ("198.18.0.1", Reserved),
        ("198.19.255.255", Reserved),
        ("198.51.100.1", Reserved),
        ("203.0.113.1", Reserved),
        ("8.8.8.8", Public),
        ("172.32.0.1", Public),
        ("172.15.255.255", Public),
        ("100.63.255.255", Public),
        ("100.128.0.1", Public),
        ("169.253.1.1", Public),
        ("::1", Loopback),
        ("::", Unspecified),
        ("fe80::1", LinkLocal),
        ("febf::1", LinkLocal),
        ("fc00::1", Private),
        ("fdff::1", Private),
        ("fec0::1", Private),
        ("ff02::1", Multicast),
        ("2001:db8::1", Reserved),
        ("2001::1", Reserved),
        ("::ffff:127.0.0.1", Loopback),
        ("::ffff:169.254.169.254", LinkLocal),
        ("::ffff:10.1.2.3", Private),
        ("::ffff:0.0.0.0", Unspecified),
        ("::ffff:8.8.8.8", Public),
        ("::127.0.0.1", Loopback),
        ("::10.0.0.1", Private),
        ("64:ff9b::7f00:1", Loopback),
        ("64:ff9b::a9fe:a9fe", LinkLocal),
        ("64:ff9b::a00:1", Private),
        ("64:ff9b::808:808", Public),
        ("2002:7f00:0001::1", Loopback),
        ("2002:c0a8:0101::1", Private),
        ("2002:a9fe:a9fe::1", LinkLocal),
        ("2002:0808:0808::1", Public),
        ("2606:4700:4700::1111", Public),
    ];
    for (addr, want) in cases {
        let ip: IpAddr = addr.parse().unwrap();
        assert_eq!(classify_ip(ip), want, "{addr}");
    }
    // `embedded_v4` agrees with the class decisions above.
    let mapped: IpAddr = "::ffff:10.1.2.3".parse().unwrap();
    assert_eq!(embedded_v4(mapped), Some("10.1.2.3".parse().unwrap()));
}
