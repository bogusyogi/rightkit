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
