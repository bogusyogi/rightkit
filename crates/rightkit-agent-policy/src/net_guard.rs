//! Network egress guard — private-IP classification and DNS-resolving SSRF check.
//!
//! Pure logic (`is_private_ip`) + one async helper (`resolve_then_reject_private`).
//! No I/O besides DNS resolution; no external dependencies beyond `tokio::net`.
//! Imported by networked fetch callers so
//! the check is authoritative and cannot diverge.

use rightkit_http::netpolicy::{classify_ip, AddressClass, IpNet};
use std::net::{IpAddr, SocketAddr};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum NetGuardError {
    #[error("private or reserved network target: {0}")]
    PrivateTarget(String),
    #[error("DNS resolution failed for '{0}': {1}")]
    ResolutionFailed(String, String),
    #[error("target URL must include a port or known scheme")]
    NoPort,
    #[error("trusted CIDR is invalid: {0}")]
    InvalidTrustedCidr(String),
    #[error("DNS resolution returned no addresses for '{0}'")]
    NoResolvedAddresses(String),
}

/// Explicit, target-scoped exceptions for browser navigation. Empty policy is
/// fail-closed: non-loopback private targets remain blocked. A trusted domain
/// is exact-or-subdomain matching, while a trusted CIDR must cover every
/// otherwise-blocked resolved address; a rebinding answer cannot smuggle an
/// unrelated private address through an exception.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TrustedNavigationPolicy {
    trusted_domains: Vec<String>,
    trusted_cidrs: Vec<TrustedCidr>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TrustedCidr {
    network: IpAddr,
    prefix: u8,
    net: IpNet,
}

impl TrustedNavigationPolicy {
    pub fn from_parts(
        trusted_domains: impl IntoIterator<Item = String>,
        trusted_cidrs: impl IntoIterator<Item = String>,
    ) -> Result<Self, NetGuardError> {
        let mut domains = trusted_domains
            .into_iter()
            .filter_map(|domain| normalize_domain(&domain))
            .collect::<Vec<_>>();
        domains.sort();
        domains.dedup();

        let mut cidrs = trusted_cidrs
            .into_iter()
            .map(|cidr| TrustedCidr::parse(&cidr))
            .collect::<Result<Vec<_>, _>>()?;
        cidrs.sort_by_key(|cidr| (cidr.network.to_string(), cidr.prefix));
        cidrs.dedup();

        Ok(Self {
            trusted_domains: domains,
            trusted_cidrs: cidrs,
        })
    }

    pub fn trusted_domains(&self) -> &[String] {
        &self.trusted_domains
    }

    pub fn trusted_cidrs(&self) -> Vec<String> {
        self.trusted_cidrs.iter().map(ToString::to_string).collect()
    }

    pub fn trusts_host(&self, host: &str) -> bool {
        let Some(host) = normalize_domain(host) else {
            return false;
        };
        self.trusted_domains
            .iter()
            .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
    }

    fn permits(&self, host: &str, ip: IpAddr, allowlisted_host: bool) -> bool {
        self.trusted_cidrs.iter().any(|cidr| cidr.contains(ip))
            || (allowlisted_host && self.trusts_host(host))
    }
}

/// Link-local ranges include cloud instance-metadata routes. Unlike private
/// development networks, they are never eligible for a trusted navigation
/// exception, including when a hostname is rebound to one of these addresses.
fn is_hard_blocked_navigation_ip(ip: IpAddr) -> bool {
    classify_ip(ip) == AddressClass::LinkLocal
}

impl TrustedCidr {
    fn parse(raw: &str) -> Result<Self, NetGuardError> {
        let Some((address, prefix)) = raw.trim().split_once('/') else {
            return Err(NetGuardError::InvalidTrustedCidr(raw.to_string()));
        };
        let network = address
            .parse::<IpAddr>()
            .map_err(|_| NetGuardError::InvalidTrustedCidr(raw.to_string()))?;
        let prefix = prefix
            .parse::<u8>()
            .map_err(|_| NetGuardError::InvalidTrustedCidr(raw.to_string()))?;
        let net = IpNet::new(network, prefix)
            .map_err(|_| NetGuardError::InvalidTrustedCidr(raw.to_string()))?;
        Ok(Self {
            network,
            prefix,
            net,
        })
    }

    fn contains(&self, ip: IpAddr) -> bool {
        self.net.contains(&ip)
    }
}

impl std::fmt::Display for TrustedCidr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.network, self.prefix)
    }
}

fn normalize_domain(raw: &str) -> Option<String> {
    let domain = raw
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase();
    (!domain.is_empty() && !domain.contains('/') && !domain.contains(char::is_whitespace))
        .then_some(domain)
}

/// Returns `true` if `ip` is in any private, loopback, link-local, CGNAT,
/// multicast, or otherwise non-public range.
///
/// IPv4 ranges covered: 0.0.0.0/8, 10/8, 127/8, 100.64/10 (CGNAT),
/// 169.254/16 (link-local / IMDS), 172.16-31/12, 192.168/16, 224+/4 (multicast/reserved).
///
/// IPv6 ranges covered: loopback (::1), unspecified (::), multicast (ff::/8),
/// ULA (fc00::/7), link-local (fe80::/10).
pub fn is_private_ip(ip: IpAddr) -> bool {
    // Classification is `rightkit-http`'s outbound address policy (embedded IPv4 forms
    // of IPv6 addresses included); anything that is not public is non-public here.
    classify_ip(ip) != AddressClass::Public
}

/// Private ranges eligible for an explicitly approved provider route. Other
/// reserved/private ranges remain blocked even after approval.
fn is_approved_private_ip(ip: IpAddr) -> bool {
    classify_ip(ip) == AddressClass::Private
}

/// Resolves `host` at `port` via DNS and rejects if ANY resolved address is
/// private/reserved. Also handles IPv6 literals in the fast path before DNS.
///
/// Fast path for literal IPs (both v4 and v6): parse directly, no DNS call.
/// Fast path for "localhost": always rejected without DNS (avoids OS resolver quirks).
///
/// # Errors
/// - `NetGuardError::PrivateTarget` if any resolved address is private
/// - `NetGuardError::ResolutionFailed` if DNS lookup fails
/// - `NetGuardError::NoPort` if `port` is `None` and the scheme has no default
pub async fn resolve_then_reject_private(host: &str, port: u16) -> Result<(), NetGuardError> {
    resolve_then_reject(host, port, false).await
}

/// Same as [`resolve_then_reject_private`] but PERMITS loopback targets
/// (localhost, 127.0.0.0/8, ::1) — for tools that legitimately reach local dev
/// servers (e.g. `browser_fetch`) while still blocking LAN, CGNAT, link-local /
/// metadata, and any hostname that DNS-rebinds to those ranges.
pub async fn resolve_then_reject_private_allow_loopback(
    host: &str,
    port: u16,
) -> Result<(), NetGuardError> {
    resolve_then_reject(host, port, true).await
}

/// Resolve one provider endpoint, validate every answer, & return exact
/// addresses for a pinned HTTP client. Explicit approval permits only
/// RFC1918, CGNAT, & ULA targets; loopback remains available without approval.
pub async fn resolve_provider_endpoint(
    host: &str,
    port: u16,
    approved_private: bool,
) -> Result<Vec<SocketAddr>, NetGuardError> {
    let blocked = |ip: IpAddr| {
        is_hard_blocked_navigation_ip(ip)
            || (is_private_ip(ip)
                && !ip.is_loopback()
                && (!approved_private || !is_approved_private_ip(ip)))
    };
    let stripped = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = stripped.parse::<IpAddr>() {
        if blocked(ip) {
            return Err(NetGuardError::PrivateTarget(format!(
                "{ip} is a private/reserved address"
            )));
        }
        return Ok(vec![SocketAddr::new(ip, port)]);
    }

    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| NetGuardError::ResolutionFailed(host.to_string(), e.to_string()))?;
    let mut resolved = Vec::new();
    for addr in addrs {
        if blocked(addr.ip()) {
            return Err(NetGuardError::PrivateTarget(format!(
                "hostname '{}' resolves to private/reserved address {}",
                host,
                addr.ip()
            )));
        }
        resolved.push(addr);
    }
    if resolved.is_empty() {
        return Err(NetGuardError::NoResolvedAddresses(host.to_string()));
    }
    Ok(resolved)
}

/// Browser-only navigation guard with explicit trusted domain/CIDR exceptions.
/// Loopback remains available for local development; all other private targets
/// require a caller-provided policy and are checked again for each redirect.
pub async fn resolve_then_reject_private_allow_loopback_with_trust(
    host: &str,
    port: u16,
    trusted: &TrustedNavigationPolicy,
) -> Result<(), NetGuardError> {
    resolve_then_reject_private_allow_loopback_with_trust_and_allowlist(host, port, trusted, |_| {
        false
    })
    .await
}

/// Same guard with a caller-owned, target-scoped domain allowlist decision.
/// The callback is evaluated for the requested hostname only; CIDR validation
/// still applies independently and link-local metadata ranges remain blocked.
pub async fn resolve_then_reject_private_allow_loopback_with_trust_and_allowlist(
    host: &str,
    port: u16,
    trusted: &TrustedNavigationPolicy,
    allowlisted_host: impl FnOnce(&str) -> bool,
) -> Result<(), NetGuardError> {
    resolve_then_reject_with_trust(host, port, true, trusted, allowlisted_host(host)).await
}

async fn resolve_then_reject(
    host: &str,
    port: u16,
    allow_loopback: bool,
) -> Result<(), NetGuardError> {
    let blocked = |ip: IpAddr| is_private_ip(ip) && !(allow_loopback && ip.is_loopback());

    // Fast path: "localhost". Permitted when loopback is allowed; otherwise always
    // rejected (avoids OS resolver returning ::1 or 127.x).
    if host.eq_ignore_ascii_case("localhost") {
        if allow_loopback {
            return Ok(());
        }
        return Err(NetGuardError::PrivateTarget(
            "localhost is a loopback address".to_string(),
        ));
    }

    // Fast path: literal IP (v4 OR v6, including bracket-stripped IPv6).
    // Strip brackets that reqwest/URL parsers leave on IPv6 literals.
    let stripped = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = stripped.parse::<IpAddr>() {
        if blocked(ip) {
            return Err(NetGuardError::PrivateTarget(format!(
                "{ip} is a private/reserved address"
            )));
        }
        return Ok(());
    }

    // Async DNS resolve path — covers DNS-rebinding attack vectors.
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| NetGuardError::ResolutionFailed(host.to_string(), e.to_string()))?;
    reject_resolved_addresses(host, addrs, blocked)
}

async fn resolve_then_reject_with_trust(
    host: &str,
    port: u16,
    allow_loopback: bool,
    trusted: &TrustedNavigationPolicy,
    allowlisted_host: bool,
) -> Result<(), NetGuardError> {
    let blocked = |ip: IpAddr| {
        is_hard_blocked_navigation_ip(ip)
            || (is_private_ip(ip)
                && !(allow_loopback && ip.is_loopback())
                && !trusted.permits(host, ip, allowlisted_host))
    };

    let stripped = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = stripped.parse::<IpAddr>() {
        if blocked(ip) {
            return Err(NetGuardError::PrivateTarget(format!(
                "{ip} is a private/reserved address"
            )));
        }
        return Ok(());
    }

    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| NetGuardError::ResolutionFailed(host.to_string(), e.to_string()))?;
    reject_resolved_addresses(host, addrs, blocked)
}

/// Validate every DNS answer, including all addresses returned during a
/// rebinding or dual-stack response. Empty answers fail closed rather than
/// being treated as proof that egress is safe.
fn reject_resolved_addresses(
    host: &str,
    addrs: impl IntoIterator<Item = std::net::SocketAddr>,
    blocked: impl Fn(IpAddr) -> bool,
) -> Result<(), NetGuardError> {
    let mut resolved = false;
    for addr in addrs {
        resolved = true;
        if blocked(addr.ip()) {
            return Err(NetGuardError::PrivateTarget(format!(
                "hostname '{}' resolves to private/reserved address {}",
                host,
                addr.ip()
            )));
        }
    }
    if !resolved {
        return Err(NetGuardError::NoResolvedAddresses(host.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    // --- is_private_ip: IPv4 ---

    #[test]
    fn loopback_v4_is_private() {
        assert!(is_private_ip("127.0.0.1".parse().unwrap()));
        assert!(is_private_ip("127.255.255.255".parse().unwrap()));
    }

    #[test]
    fn rfc1918_is_private() {
        assert!(is_private_ip("10.0.0.1".parse().unwrap()));
        assert!(is_private_ip("172.16.0.1".parse().unwrap()));
        assert!(is_private_ip("172.31.255.255".parse().unwrap()));
        assert!(is_private_ip("192.168.1.1".parse().unwrap()));
    }

    #[test]
    fn cgnat_is_private() {
        assert!(is_private_ip("100.64.0.1".parse().unwrap()));
        assert!(is_private_ip("100.127.255.255".parse().unwrap()));
        // Just outside CGNAT — public
        assert!(!is_private_ip("100.63.255.255".parse::<IpAddr>().unwrap()));
        assert!(!is_private_ip("100.128.0.1".parse::<IpAddr>().unwrap()));
    }

    #[test]
    fn link_local_imds_is_private() {
        assert!(is_private_ip("169.254.169.254".parse().unwrap())); // AWS IMDS
        assert!(is_private_ip("169.254.0.1".parse().unwrap()));
    }

    #[test]
    fn multicast_and_reserved_is_private() {
        assert!(is_private_ip("224.0.0.1".parse().unwrap()));
        assert!(is_private_ip("255.255.255.255".parse().unwrap()));
    }

    #[test]
    fn documentation_benchmark_and_ipv4_mapped_ranges_are_private() {
        for ip in [
            "192.0.2.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "2001:db8::1",
        ] {
            assert!(is_private_ip(ip.parse().unwrap()), "{ip} must be blocked");
        }
    }

    #[test]
    fn mapped_link_local_is_hard_blocked_even_with_trusted_exception() {
        let mapped: IpAddr = "::ffff:169.254.169.254".parse().unwrap();
        assert!(is_hard_blocked_navigation_ip(mapped));

        let policy = TrustedNavigationPolicy::from_parts(
            vec!["metadata.example".to_string()],
            vec!["::ffff:0:0/96".to_string()],
        )
        .unwrap();
        assert!(policy.permits("metadata.example", mapped, true));
        assert!(is_hard_blocked_navigation_ip(mapped));
    }

    #[test]
    fn every_resolved_address_is_checked_for_rebinding() {
        let addrs = [
            std::net::SocketAddr::from(([93, 184, 216, 34], 443)),
            std::net::SocketAddr::from(([169, 254, 169, 254], 443)),
        ];
        let err = reject_resolved_addresses("rebound.example", addrs, is_private_ip).unwrap_err();
        assert!(matches!(err, NetGuardError::PrivateTarget(_)));
    }

    #[test]
    fn empty_dns_answer_fails_closed() {
        let err = reject_resolved_addresses(
            "empty.example",
            std::iter::empty::<std::net::SocketAddr>(),
            |_| false,
        )
        .unwrap_err();
        assert!(matches!(err, NetGuardError::NoResolvedAddresses(host) if host == "empty.example"));
    }

    #[test]
    fn public_v4_is_not_private() {
        assert!(!is_private_ip("1.2.3.4".parse::<IpAddr>().unwrap()));
        assert!(!is_private_ip("8.8.8.8".parse::<IpAddr>().unwrap()));
        assert!(!is_private_ip("93.184.216.34".parse::<IpAddr>().unwrap()));
    }

    // --- is_private_ip: IPv6 ---

    #[test]
    fn loopback_v6_is_private() {
        assert!(is_private_ip("::1".parse().unwrap()));
    }

    #[test]
    fn unspecified_v6_is_private() {
        assert!(is_private_ip("::".parse().unwrap()));
    }

    #[test]
    fn ula_v6_is_private() {
        assert!(is_private_ip("fc00::1".parse().unwrap()));
        assert!(is_private_ip("fd00::1".parse().unwrap()));
        assert!(is_private_ip(
            "fdff:ffff:ffff:ffff:ffff:ffff:ffff:ffff".parse().unwrap()
        ));
    }

    #[test]
    fn link_local_v6_is_private() {
        assert!(is_private_ip("fe80::1".parse().unwrap()));
    }

    #[test]
    fn public_v6_is_not_private() {
        assert!(!is_private_ip(
            "2001:4860:4860::8888".parse::<IpAddr>().unwrap()
        ));
        assert!(!is_private_ip(
            "2606:4700:4700::1111".parse::<IpAddr>().unwrap()
        ));
    }

    // --- resolve_then_reject_private ---

    #[tokio::test]
    async fn localhost_rejected_without_dns() {
        let err = resolve_then_reject_private("localhost", 80)
            .await
            .unwrap_err();
        assert!(matches!(err, NetGuardError::PrivateTarget(_)));
    }

    #[tokio::test]
    async fn localhost_case_insensitive() {
        let err = resolve_then_reject_private("LOCALHOST", 80)
            .await
            .unwrap_err();
        assert!(matches!(err, NetGuardError::PrivateTarget(_)));
    }

    #[tokio::test]
    async fn literal_private_v4_fast_path() {
        let err = resolve_then_reject_private("192.168.1.1", 443)
            .await
            .unwrap_err();
        assert!(matches!(err, NetGuardError::PrivateTarget(_)));
    }

    #[tokio::test]
    async fn literal_loopback_v4_fast_path() {
        let err = resolve_then_reject_private("127.0.0.1", 8080)
            .await
            .unwrap_err();
        assert!(matches!(err, NetGuardError::PrivateTarget(_)));
    }

    #[tokio::test]
    async fn literal_private_v6_fast_path() {
        // Bare IPv6 literal (no brackets)
        let err = resolve_then_reject_private("::1", 80).await.unwrap_err();
        assert!(matches!(err, NetGuardError::PrivateTarget(_)));
    }

    #[tokio::test]
    async fn bracketed_ipv6_literal_fast_path() {
        // reqwest URL-parsed hosts come out as "[::1]" — strip brackets correctly
        let err = resolve_then_reject_private("[::1]", 443).await.unwrap_err();
        assert!(matches!(err, NetGuardError::PrivateTarget(_)));
    }

    #[tokio::test]
    async fn literal_public_v4_allowed() {
        // Numeric public IP must NOT be rejected
        resolve_then_reject_private("1.2.3.4", 443).await.unwrap();
    }

    #[tokio::test]
    async fn literal_public_v6_allowed() {
        resolve_then_reject_private("2001:4860:4860::8888", 443)
            .await
            .unwrap();
    }

    // --- resolve_then_reject_private_allow_loopback (browser_fetch local-dev path) ---

    #[tokio::test]
    async fn allow_loopback_permits_localhost() {
        resolve_then_reject_private_allow_loopback("localhost", 5173)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn allow_loopback_permits_127_and_v6_loopback() {
        resolve_then_reject_private_allow_loopback("127.0.0.1", 3000)
            .await
            .unwrap();
        resolve_then_reject_private_allow_loopback("::1", 3000)
            .await
            .unwrap();
        resolve_then_reject_private_allow_loopback("[::1]", 3000)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn provider_endpoint_requires_explicit_private_approval() {
        assert!(matches!(
            resolve_provider_endpoint("192.168.1.10", 8080, false).await,
            Err(NetGuardError::PrivateTarget(_))
        ));
        assert_eq!(
            resolve_provider_endpoint("192.168.1.10", 8080, true)
                .await
                .unwrap(),
            vec![std::net::SocketAddr::from(([192, 168, 1, 10], 8080))]
        );
    }

    #[tokio::test]
    async fn provider_endpoint_never_allows_metadata_even_when_approved() {
        assert!(matches!(
            resolve_provider_endpoint("169.254.169.254", 80, true).await,
            Err(NetGuardError::PrivateTarget(_))
        ));
    }

    #[tokio::test]
    async fn provider_approval_still_blocks_reserved_ranges() {
        for host in [
            "0.0.0.0",
            "224.0.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "203.0.113.1",
            "255.255.255.255",
            "::",
            "2001:db8::1",
            "ff02::1",
        ] {
            assert!(
                matches!(
                    resolve_provider_endpoint(host, 443, true).await,
                    Err(NetGuardError::PrivateTarget(_)),
                ),
                "reserved target must stay blocked: {host}"
            );
        }
    }

    #[tokio::test]
    async fn provider_approval_allows_intended_private_ranges() {
        for host in [
            "10.0.0.5",
            "100.64.0.5",
            "172.16.0.5",
            "192.168.0.5",
            "fc00::5",
        ] {
            resolve_provider_endpoint(host, 443, true)
                .await
                .unwrap_or_else(|error| {
                    panic!("approved private target rejected: {host}: {error}")
                });
        }
    }

    #[tokio::test]
    async fn allow_loopback_still_blocks_lan_and_cgnat_and_metadata() {
        // Loopback is permitted, but LAN / CGNAT / link-local-metadata are NOT.
        for host in ["192.168.1.10", "10.0.0.5", "100.64.0.1", "169.254.169.254"] {
            let err = resolve_then_reject_private_allow_loopback(host, 443)
                .await
                .unwrap_err();
            assert!(
                matches!(err, NetGuardError::PrivateTarget(_)),
                "{host} must still be blocked under allow_loopback"
            );
        }
    }

    #[test]
    fn trusted_navigation_requires_exact_explicit_domain_or_covering_cidr() {
        let policy = TrustedNavigationPolicy::from_parts(
            vec!["dev.internal.".to_string()],
            vec!["10.42.0.0/16".to_string()],
        )
        .unwrap();
        assert!(policy.trusts_host("dev.internal"));
        assert!(policy.trusts_host("api.dev.internal"));
        assert!(!policy.trusts_host("not-dev.internal"));
        assert!(policy.permits("other.example", "10.42.5.9".parse().unwrap(), false));
        assert!(!policy.permits("other.example", "10.43.5.9".parse().unwrap(), false));
        assert!(!policy.permits("dev.internal", "10.43.5.9".parse().unwrap(), false));
        assert!(policy.permits("dev.internal", "10.43.5.9".parse().unwrap(), true));
    }

    #[tokio::test]
    async fn trusted_cidr_permits_only_that_literal_private_target() {
        let policy =
            TrustedNavigationPolicy::from_parts(Vec::new(), vec!["192.168.44.0/24".to_string()])
                .unwrap();
        resolve_then_reject_private_allow_loopback_with_trust("192.168.44.12", 443, &policy)
            .await
            .unwrap();
        let err =
            resolve_then_reject_private_allow_loopback_with_trust("192.168.45.12", 443, &policy)
                .await
                .unwrap_err();
        assert!(matches!(err, NetGuardError::PrivateTarget(_)));
    }

    #[tokio::test]
    async fn trusted_cidr_never_overrides_link_local_metadata_ranges() {
        let policy =
            TrustedNavigationPolicy::from_parts(Vec::new(), vec!["169.254.0.0/16".to_string()])
                .unwrap();
        let err =
            resolve_then_reject_private_allow_loopback_with_trust("169.254.169.254", 80, &policy)
                .await
                .unwrap_err();
        assert!(matches!(err, NetGuardError::PrivateTarget(_)));

        let mapped_policy =
            TrustedNavigationPolicy::from_parts(Vec::new(), vec!["::ffff:0:0/96".to_string()])
                .unwrap();
        let mapped_err = resolve_then_reject_private_allow_loopback_with_trust(
            "::ffff:169.254.169.254",
            80,
            &mapped_policy,
        )
        .await
        .unwrap_err();
        assert!(matches!(mapped_err, NetGuardError::PrivateTarget(_)));
    }
}
