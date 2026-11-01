//! URL, host and address helpers shared by every crate (audit D5, #210).
//!
//! Hand-written `https://` stripping lived in six places, the Instagram
//! shortcode rule differed between two of them (`p|reel` against
//! `p|reel|reels`), and the private-address list differed between the
//! classifier and the fetch guard. One implementation each, here.

use std::net::IpAddr;

use url::Url;

/// Parse an http(s) URL. A value with no scheme (`example.com/a`) is read as
/// `https://example.com/a`. `None` for any other scheme or an unparsable value.
fn parse_http(input: &str) -> Option<Url> {
    let trimmed = input.trim();
    let url = if trimmed.contains("://") {
        Url::parse(trimmed).ok()?
    } else {
        Url::parse(&format!("https://{trimmed}")).ok()?
    };
    matches!(url.scheme(), "http" | "https").then_some(url)
}

/// Lowercased host of an http(s) URL without a leading `www.` and without any
/// port or credentials. `None` for other schemes or a URL with no host.
pub fn host(input: &str) -> Option<String> {
    let url = parse_http(input)?;
    let host = url.host_str()?.to_ascii_lowercase();
    Some(host.strip_prefix("www.").map(str::to_owned).unwrap_or(host))
}

/// Instagram post code of a `/p/<code>/`, `/reel/<code>/` or `/reels/<code>/`
/// URL (any host); `None` when the path carries none of those markers.
pub fn instagram_shortcode(input: &str) -> Option<String> {
    let url = parse_http(input)?;
    let segments: Vec<&str> = url.path_segments()?.collect();
    segments.windows(2).find_map(|pair| {
        if !matches!(pair[0], "p" | "reel" | "reels") {
            return None;
        }
        let code: String = pair[1]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        (!code.is_empty()).then_some(code)
    })
}

/// True for an address a public fetch must never reach: loopback, private,
/// link-local, carrier-grade NAT, multicast, unspecified, documentation and
/// other reserved ranges, and IPv6 forms that embed one of those. The
/// classification is RightKit's (`rightkit_http::netpolicy`), the same one the
/// fetch transport enforces.
pub fn is_blocked_ip(ip: IpAddr) -> bool {
    rightkit_http::netpolicy::classify_ip(ip) != rightkit_http::AddressClass::Public
}

/// True when `host` is something a public fetch may target: not `localhost`
/// (or `*.localhost`) and not a blocked IP literal. A bracketed IPv6 literal is
/// accepted. Hostnames are not resolved here; this is the pure, string-level check.
pub fn is_public_host(host: &str) -> bool {
    let host = host.trim().trim_matches(['[', ']']).to_ascii_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host);
    if host.is_empty() || host == "localhost" || host.ends_with(".localhost") {
        return false;
    }
    host.parse::<IpAddr>().map_or(true, |ip| !is_blocked_ip(ip))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_strips_www_port_and_case() {
        assert_eq!(
            host("https://WWW.Example.com:8080/a?b#c"),
            Some("example.com".into())
        );
        assert_eq!(
            host("http://user:pw@example.com/"),
            Some("example.com".into())
        );
        assert_eq!(host("example.com/path"), Some("example.com".into()));
        assert_eq!(host("m.youtube.com/watch"), Some("m.youtube.com".into()));
    }

    #[test]
    fn host_rejects_other_schemes_and_junk() {
        assert_eq!(host("ftp://example.com/"), None);
        assert_eq!(host("file:///tmp/a.mp3"), None);
        assert_eq!(host(""), None);
        assert_eq!(host("   "), None);
    }

    #[test]
    fn shortcode_accepts_p_reel_and_reels() {
        assert_eq!(
            instagram_shortcode("https://www.instagram.com/p/AbC-12_x/?igsh=1"),
            Some("AbC-12_x".into())
        );
        assert_eq!(
            instagram_shortcode("https://instagram.com/reel/XYZ123/"),
            Some("XYZ123".into())
        );
        assert_eq!(
            instagram_shortcode("https://instagram.com/reels/QQ9/"),
            Some("QQ9".into())
        );
        assert_eq!(
            instagram_shortcode("https://www.instagram.com/some.user/p/Code9/"),
            Some("Code9".into())
        );
    }

    #[test]
    fn shortcode_is_none_without_a_marker_or_code() {
        assert_eq!(
            instagram_shortcode("https://www.instagram.com/some.user/"),
            None
        );
        assert_eq!(instagram_shortcode("https://www.instagram.com/p/"), None);
        assert_eq!(
            instagram_shortcode("https://www.instagram.com/explore/tags/p"),
            None
        );
        assert_eq!(instagram_shortcode("not a url"), None);
    }

    #[test]
    fn blocked_addresses_cover_the_private_ranges() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "192.0.2.1",
            "::1",
            "::",
            "fe80::1",
            "fc00::1",
        ] {
            assert!(is_blocked_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "8.8.8.8",
            "1.1.1.1",
            "93.184.216.34",
            "2606:4700:4700::1111",
        ] {
            assert!(!is_blocked_ip(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn public_host_refuses_local_names_and_blocked_literals() {
        for host in [
            "localhost",
            "LOCALHOST",
            "app.localhost",
            "localhost.",
            "127.0.0.1",
            "[::1]",
            "192.168.0.10",
            "",
        ] {
            assert!(!is_public_host(host), "{host}");
        }
        for host in [
            "example.com",
            "8.8.8.8",
            "[2606:4700:4700::1111]",
            "sub.example.org",
        ] {
            assert!(is_public_host(host), "{host}");
        }
    }
}
