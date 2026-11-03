//! Public-web URL guards for transports that are not this crate's client (a TLS-fingerprint
//! client, yt-dlp, a headless browser), plus a strict public-web client preset: a canonical-IP and
//! host-character string guard, a resolve-and-vet check, and refusal text helpers.

use std::net::IpAddr;

/// String-level guard for URLs handed to transports that are not this crate's client
/// (tls-client, yt-dlp, URL pre-checks). The host is normalised once (lowercase, one
/// trailing dot dropped) and every check below uses that form.
///
/// Rule for IPv4 spelling (parsed by rightkit-http `parse_host_ip`): only canonical dotted-quads (and IPv6 literals) are
/// classified. Any other spelling the OS resolver (`inet_aton`) would read as an IPv4
/// address (`127.1`, `2130706433`, `0177.0.0.1`, `0x7f.0.0.1`, even a public `8.8`) is
/// refused outright, because the downstream transports (Go, Python, a browser) do not
/// agree with each other or with rightkit-http about those spellings. Strings that are
/// not an IPv4 address under inet_aton rules (`1.2.3.4.5`, `09`, `3m.com`) are hostnames.
pub fn ensure_public_http_url(url: &str) -> Result<(), String> {
    let trimmed = url.trim();
    let lower_scheme = trimmed.to_ascii_lowercase();
    let rest = lower_scheme
        .strip_prefix("https://")
        .or_else(|| lower_scheme.strip_prefix("http://"))
        .ok_or_else(|| "only http and https URLs are allowed".to_string())?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let raw_host = parse_authority_host(host_port).ok_or_else(|| "URL has no host".to_string())?;
    let host = raw_host
        .strip_suffix('.')
        .unwrap_or(raw_host)
        .to_ascii_lowercase();
    if host.is_empty() {
        return Err("URL has no host".to_string());
    }
    check_host_characters(authority, &host)?;
    validate_port(host_port)?;
    if host == "localhost" || host.ends_with(".localhost") {
        return Err("refusing to fetch a localhost URL".to_string());
    }
    if let Some(parsed) = crate::parse_host_ip(&host) {
        if !parsed.is_canonical {
            return Err(format!(
                "refusing to fetch a non-canonical IP address: {host}"
            ));
        }
        use crate::netpolicy::classify_ip;
        let ip = parsed.addr;
        let blocked = classify_ip(ip) != crate::AddressClass::Public
            || strict_denied_ranges().iter().any(|net| net.contains(&ip));
        if blocked {
            return Err(format!(
                "refusing to fetch a private or loopback address: {host}"
            ));
        }
    }
    Ok(())
}

/// Refuse hosts that another URL parser or resolver could read as something other than
/// what this function classifies. `authority` is the text between `//` and the first
/// `/ ? #`; `host` is the normalised host.
///
/// - Non-ASCII numerals (fullwidth, Arabic-Indic, superscripts, ...) and non-ASCII
///   dot-like separators (U+3002, U+FF0E, U+FF61): IDNA/WHATWG parsers and OS resolvers
///   fold them to ASCII digits and dots, so `１２７.０.０.１` becomes 127.0.0.1.
/// - `%` (percent-encoded host bytes, and IPv6 zone identifiers such as `%25eth0`, which
///   can reach here inside brackets), any other non-ASCII character that is not a letter.
/// - A backslash, whitespace or control character anywhere in the authority: browsers
///   treat `\` as `/`, so `http://example.com\@127.0.0.1/` has host `example.com` here
///   but 127.0.0.1 there.
///
/// A host made only of letters (an internationalised name such as `bücher.de`) passes:
/// letters cannot be folded to digits, so no parser turns it into an IP literal.
/// (Decomposed forms, a base letter plus a combining mark, are not letters and are refused.)
fn check_host_characters(authority: &str, host: &str) -> Result<(), String> {
    let invalid = || "URL has an invalid host".to_string();
    if authority
        .chars()
        .any(|c| c == '\\' || c.is_whitespace() || c.is_control())
    {
        return Err(invalid());
    }
    for c in host.chars() {
        if c.is_ascii() {
            if c == '%' {
                return Err(invalid());
            }
            continue;
        }
        if c.is_numeric() || matches!(c, '\u{3002}' | '\u{FF0E}' | '\u{FF61}') {
            return Err(format!(
                "refusing to fetch a non-canonical IP address: {host}"
            ));
        }
        if !c.is_alphabetic() {
            return Err(invalid());
        }
    }
    Ok(())
}

/// The port part of `host:port` / `[v6]:port` must be empty or a number in 0..=65535.
fn validate_port(host_port: &str) -> Result<(), String> {
    let after_host = if host_port.starts_with('[') {
        host_port.split_once(']').map(|(_, r)| r).unwrap_or("")
    } else {
        host_port.split_once(':').map(|(_, r)| r).unwrap_or("")
    };
    let port = match after_host {
        "" => return Ok(()),
        r if host_port.starts_with('[') => r
            .strip_prefix(':')
            .ok_or_else(|| "URL has a malformed port".to_string())?,
        r => r,
    };
    if port.is_empty() {
        return Ok(());
    }
    if port.bytes().all(|b| b.is_ascii_digit()) && port.parse::<u16>().is_ok() {
        Ok(())
    } else {
        Err("URL has a malformed port".to_string())
    }
}

fn parse_authority_host(host_port: &str) -> Option<&str> {
    if let Some(stripped) = host_port.strip_prefix('[') {
        if let Some(end) = stripped.find(']') {
            return Some(stripped[..end].trim());
        }
        // Malformed bracketed host — reject rather than falling through
        // to the colon-split which would let an attacker bypass the
        // loopback/private guard (e.g. "http://[::1/path").
        return None;
    }
    Some(host_port.split(':').next().unwrap_or(host_port).trim())
}

/// Ranges denied on top of the `public_web` classes: ALL of 64:ff9b::/32
/// (NAT64, wider than the /96 rule) and 2002::/16 (6to4), regardless of the embedded
/// IPv4 address.
pub fn strict_denied_ranges() -> Vec<crate::IpNet> {
    use crate::IpNet;
    let nat64 = IpNet::new(
        IpAddr::V6(std::net::Ipv6Addr::new(0x64, 0xff9b, 0, 0, 0, 0, 0, 0)),
        32,
    )
    .expect("NAT64 prefix is valid");
    let sixto4 = IpNet::new(
        IpAddr::V6(std::net::Ipv6Addr::new(0x2002, 0, 0, 0, 0, 0, 0, 0)),
        16,
    )
    .expect("6to4 prefix is valid");
    vec![nat64, sixto4]
}

/// A stricter public-web address policy: `AddressPolicy::public_web` (which already denies loopback, RFC 1918, link-local
/// (incl. 169.254.169.254), CGNAT, multicast, reserved, IPv6 ULA/link-local and
/// resolves every hostname before connecting) plus the NAT64 and 6to4 ranges above.
pub fn strict_public_web_policy() -> crate::AddressPolicy {
    crate::AddressPolicy::public_web().deny_ranges(strict_denied_ranges())
}

/// Blocking public-web client. `timeout` replaces ureq's `timeout_global` (it is
/// applied to connect, response headers, the whole request, and body receive);
/// `max_redirects` is the number rightkit-http follows itself (0 = return the 3xx so
/// the caller walks hops; every hop is still resolved and vetted by the policy).
/// Only 429 responses are retried (see `rate_limit_retry_policy`). Proxies: `HTTP(S)_PROXY`/`NO_PROXY`
/// are honoured as ureq did (`ProxyMode::Environment`); see rightkit-http
/// `client.rs` ProxyMode docs: the destination is still class/name-checked and the
/// proxy endpoint is trusted configuration.
pub fn public_web_client(
    user_agent: &str,
    timeout: std::time::Duration,
    max_redirects: u32,
) -> crate::Client {
    crate::Client::new(crate::ClientConfig {
        user_agent: user_agent.to_string(),
        connect_timeout: Some(timeout),
        request_timeout: timeout,
        response_timeout: Some(timeout),
        recv_body_timeout: Some(timeout),
        max_redirects,
        max_redirects_will_error: true,
        retry: rate_limit_retry_policy(),
        address_policy: strict_public_web_policy(),
        proxy: crate::ProxyMode::Environment,
        ..crate::ClientConfig::default()
    })
}

/// Retry policy for public page fetches: a 429 (rate limited) is retried with exponential
/// backoff and jitter, honouring `Retry-After` (rightkit-http caps the wait). Other statuses
/// and transport errors fail at once, so a caller's fallback ladder keeps its timing.
/// rightkit-http only retries idempotent methods, and every caller here sends GET.
pub fn rate_limit_retry_policy() -> crate::RetryPolicy {
    crate::RetryPolicy {
        max_attempts: 3,
        retry_statuses: vec![429],
        honor_retry_after: true,
        ..crate::RetryPolicy::default()
    }
}

/// Text for a failed request, rebuilt from the typed error kind so the strings that
/// callers wrap match what ureq produced.
pub fn error_text(err: &crate::HttpError) -> String {
    use crate::HttpError;
    match err {
        HttpError::Status { status, .. } => format!("http status: {status}"),
        HttpError::Timeout { .. } => "timeout: global".to_string(),
        HttpError::BodyTooLarge { limit } => {
            format!("the response body is larger than request limit: {limit}")
        }
        HttpError::AddressDenied { host, .. } => {
            format!("{REFUSAL_PREFIX} a private or loopback address: {host}")
        }
        HttpError::Transport { message, .. } => message.clone(),
        HttpError::InvalidRequest(message) => format!("bad uri: {message}"),
        other => other.to_string(),
    }
}

/// True when the failure happened while reading the response body (old text:
/// `read ... body: {e}`), as opposed to connecting / waiting for headers /
/// redirecting (old text: `... fetch failed: {e}`).
pub fn is_body_phase(err: &crate::HttpError) -> bool {
    matches!(err, crate::HttpError::BodyTooLarge { .. }) || err.phase() == Some(crate::Phase::Body)
}

/// Every address-policy refusal string starts with this, whether it comes from the
/// string validator, the resolved-address check, or a typed `AddressDenied`.
pub const REFUSAL_PREFIX: &str = "refusing to fetch";

/// True when `message` is an address-policy refusal. Fetch fallbacks must stop on it:
/// a refused destination is never retried through another transport.
pub fn is_policy_refusal(message: &str) -> bool {
    message.contains(REFUSAL_PREFIX)
}

/// Read a response body to the end, failing when it exceeds `limit` bytes (ureq's
/// `.limit(n).read_to_vec()` behaviour and text). Error text has no prefix.
pub fn read_body_limited(
    reader: Box<dyn std::io::Read + Send>,
    limit: u64,
) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    reader
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err(format!(
            "the response body is larger than request limit: {limit}"
        ));
    }
    Ok(bytes)
}

/// `(host, port)` of an http(s) URL, or `None`.
fn host_and_port(url: &str) -> Option<(String, u16)> {
    let trimmed = url.trim();
    let lower = trimmed.to_ascii_lowercase();
    let (default_port, rest) = if let Some(r) = lower.strip_prefix("https://") {
        (443u16, r.to_string())
    } else {
        (80u16, lower.strip_prefix("http://")?.to_string())
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let host = parse_authority_host(host_port)?.to_string();
    let after_host = if host_port.starts_with('[') {
        host_port.split_once(']').map(|(_, r)| r).unwrap_or("")
    } else {
        host_port.split_once(':').map(|(_, r)| r).unwrap_or("")
    };
    let port = after_host
        .strip_prefix(':')
        .unwrap_or(after_host)
        .parse::<u16>()
        .unwrap_or(default_port);
    Some((host, port))
}

/// Resolve the URL's host through [`strict_public_web_policy`]
/// (`AddressPolicy::resolve_vetted`: same classes, NAT64/6to4 ranges and error as the
/// rightkit-http client) and refuse it when any resolved address is denied.
///
/// For transports that are NOT rightkit-http (the native TLS-fingerprint client).
/// `resolve_vetted` returns the vetted addresses, but the native client's request
/// payload cannot be told to connect to them, so it re-resolves on its own: this is a
/// check, NOT a pin, and a DNS-rebinding window remains for that transport.
/// A refusal reads like every other refusal (`REFUSAL_PREFIX`); a failed lookup does not.
pub fn vet_resolved_host(url: &str) -> Result<(), String> {
    use crate::HttpError;
    let (host, port) = host_and_port(url).ok_or_else(|| "URL has no host".to_string())?;
    match strict_public_web_policy().resolve_vetted(&host, port) {
        Ok(_) => Ok(()),
        Err(HttpError::AddressDenied { host, .. }) => Err(format!(
            "{REFUSAL_PREFIX} a private or loopback address: {host}"
        )),
        Err(other) => Err(format!("could not resolve {host}: {}", error_text(&other))),
    }
}

/// String guard plus resolve-and-check, in that order, for a URL handed to an external
/// tool (yt-dlp, the bundled browser) that does its own DNS. The string guard runs first,
/// so a literal or malformed host is refused without a lookup. Like `vet_resolved_host`
/// this is a check, not a pin: a DNS-rebinding window remains.
pub fn ensure_public_and_resolved(url: &str) -> Result<(), String> {
    ensure_public_http_url(url)?;
    vet_resolved_host(url)
}
