//! Outbound address policy: address classification and the SSRF guard behind every
//! [`crate::Client`] and [`crate::AsyncClient`] request.
//!
//! The classification mirrors `rightkit-browser`'s `policy.rs` (`classify_ip`,
//! `AddressClass`) with the same semantics, so a browser page and an HTTP call reach the
//! same verdict for the same address. A test cross-checks the two tables.
//!
//! How the guard works:
//!
//! 1. The host of the request URL (and of every redirect hop) is resolved once, by this
//!    crate's resolver. Each resolved address is classified. One denied address denies the
//!    whole host.
//! 2. Only the vetted addresses are handed to the connector, so the socket opens to an
//!    address that was checked. There is no second lookup between check and connect.
//! 3. Names that resolve to no address are denied.
//!
//! Default (public-web) policy denies loopback, RFC 1918, link-local (including
//! `169.254.169.254`), CGNAT `100.64/10`, multicast, reserved, unspecified and broadcast
//! addresses, IPv6 ULA `fc00::/7` and link-local `fe80::/10`, and IPv6 forms that embed an
//! IPv4 address (mapped, compatible, NAT64 `64:ff9b::/96`, 6to4 `2002::/16`) by the class of
//! the embedded IPv4 address. Link-local and cloud-metadata destinations cannot be allowed
//! by any hook.

use std::error::Error;
use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::str::FromStr;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use crate::error::{HttpError, Phase};

/// Address class of a destination. Everything except `Public` is denied by default.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum AddressClass {
    Public,
    Loopback,
    /// `169.254/16`, `fe80::/10`; includes the cloud metadata endpoint.
    LinkLocal,
    /// RFC 1918, CGNAT `100.64/10`, ULA `fc00::/7`, site-local `fec0::/10`.
    Private,
    /// IETF protocol assignments, documentation, benchmarking, `240/4`, broadcast,
    /// `2001:db8::/32`, Teredo.
    Reserved,
    Unspecified,
    Multicast,
}

/// Classify one address. IPv6 forms that embed an IPv4 address take the embedded class.
pub fn classify_ip(ip: IpAddr) -> AddressClass {
    match ip {
        IpAddr::V4(v4) => classify_v4(v4),
        IpAddr::V6(v6) => classify_v6(v6),
    }
}

fn classify_v4(ip: Ipv4Addr) -> AddressClass {
    let [a, b, c, _] = ip.octets();
    if a == 127 {
        AddressClass::Loopback
    } else if a == 0 {
        AddressClass::Unspecified
    } else if a == 169 && b == 254 {
        AddressClass::LinkLocal
    } else if a == 10
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || (a == 100 && (64..=127).contains(&b))
    {
        AddressClass::Private
    } else if (224..=239).contains(&a) {
        AddressClass::Multicast
    } else if a >= 240
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
    {
        AddressClass::Reserved
    } else {
        AddressClass::Public
    }
}

fn v4_from_segments(hi: u16, lo: u16) -> Ipv4Addr {
    Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8)
}

fn v4_from_octets(o: &[u8; 16], idx: [usize; 4]) -> Ipv4Addr {
    Ipv4Addr::new(o[idx[0]], o[idx[1]], o[idx[2]], o[idx[3]])
}

/// Every IPv4 address an IPv6 address can embed: IPv4-mapped `::ffff:a.b.c.d`, the
/// deprecated IPv4-compatible `::a.b.c.d`, NAT64 `64:ff9b::/96`, 6to4 `2002::/16`, and the
/// local-use NAT64 space `64:ff9b:1::/48` (RFC 8215). The local-use prefix may be deployed
/// at several lengths (RFC 6052 section 2.2), so each of the /48, /56, /64 and /96 layouts
/// is returned. `::`, `::1` and non-embedding addresses return nothing.
pub fn embedded_v4s(ip: IpAddr) -> Vec<Ipv4Addr> {
    let IpAddr::V6(v6) = ip else {
        return Vec::new();
    };
    if v6.is_loopback() || v6.is_unspecified() {
        return Vec::new();
    }
    let s = v6.segments();
    if let Some(v4) = v6.to_ipv4_mapped() {
        return vec![v4];
    }
    if s[..6] == [0, 0, 0, 0, 0, 0] || s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        return vec![v4_from_segments(s[6], s[7])];
    }
    if s[0] == 0x2002 {
        return vec![v4_from_segments(s[1], s[2])];
    }
    if s[0] == 0x64 && s[1] == 0xff9b && s[2] == 1 {
        let o = v6.octets();
        return vec![
            v4_from_octets(&o, [12, 13, 14, 15]),
            v4_from_octets(&o, [9, 10, 11, 12]),
            v4_from_octets(&o, [7, 9, 10, 11]),
            v4_from_octets(&o, [6, 7, 9, 10]),
        ];
    }
    Vec::new()
}

/// The IPv4 address embedded in an IPv6 address (see [`embedded_v4s`]): the first
/// non-public one when several layouts are possible, otherwise the first.
pub fn embedded_v4(ip: IpAddr) -> Option<Ipv4Addr> {
    let all = embedded_v4s(ip);
    all.iter()
        .copied()
        .find(|v4| classify_v4(*v4) != AddressClass::Public)
        .or_else(|| all.first().copied())
}

fn classify_v6(v6: Ipv6Addr) -> AddressClass {
    let s = v6.segments();
    if v6.is_loopback() {
        return AddressClass::Loopback;
    }
    if v6.is_unspecified() {
        return AddressClass::Unspecified;
    }
    let embedded = embedded_v4s(IpAddr::V6(v6));
    if !embedded.is_empty() {
        return embedded
            .into_iter()
            .map(classify_v4)
            .find(|c| *c != AddressClass::Public)
            .unwrap_or(AddressClass::Public);
    }
    if v6.is_multicast() {
        AddressClass::Multicast
    } else if s[0] & 0xffc0 == 0xfe80 {
        AddressClass::LinkLocal
    } else if s[0] & 0xfe00 == 0xfc00 || s[0] & 0xffc0 == 0xfec0 {
        AddressClass::Private
    } else if (s[0] == 0x2001 && s[1] == 0x0db8) || (s[0] == 0x2001 && s[1] == 0) {
        AddressClass::Reserved
    } else {
        AddressClass::Public
    }
}

/// Addresses no hook may allow: link-local (which holds `169.254.169.254`), AWS's IPv6
/// infrastructure block `fd00:ec2::/32` (metadata `fd00:ec2::254`, DNS `fd00:ec2::253`),
/// and the metadata addresses outside link-local: Alibaba `100.100.100.200`, Azure's
/// wire server `168.63.129.16`, and Oracle/other `192.0.0.192`. IPv6 forms embedding one
/// of these are covered too. Returns why the address is blocked.
fn hard_block(ip: IpAddr) -> Option<&'static str> {
    fn one(ip: IpAddr) -> Option<&'static str> {
        if classify_ip(ip) == AddressClass::LinkLocal {
            return Some("is link-local (cloud metadata range)");
        }
        match ip {
            IpAddr::V4(v4)
                if [[100, 100, 100, 200], [168, 63, 129, 16], [192, 0, 0, 192]]
                    .contains(&v4.octets()) =>
            {
                Some("is a cloud metadata address")
            }
            IpAddr::V6(v6) if v6.segments()[0] == 0xfd00 && v6.segments()[1] == 0x0ec2 => {
                Some("is in the AWS IPv6 metadata block fd00:ec2::/32")
            }
            _ => None,
        }
    }
    one(ip).or_else(|| {
        embedded_v4s(ip)
            .into_iter()
            .find_map(|v4| one(IpAddr::V4(v4)))
    })
}

/// How a host string spelled an IP address.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum HostIpKind {
    /// Four decimal parts without leading zeros: `1.1.1.1`.
    Ipv4DottedQuad,
    /// Any other IPv4 spelling `inet_aton` accepts: `127.1`, `0177.0.0.1`, `0x7f.0.0.1`,
    /// `2130706433`, `10.1.2`, and the dotted quad with a trailing dot.
    Ipv4Legacy,
    /// An IPv6 literal, with or without brackets, including IPv4-mapped forms.
    Ipv6,
}

/// A host string that is an IP address, as parsed by [`parse_host_ip`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ParsedHostIp {
    pub addr: IpAddr,
    pub kind: HostIpKind,
    /// True for a dotted quad or an IPv6 literal, which every parser reads the same way.
    /// False for [`HostIpKind::Ipv4Legacy`] spellings, which downstream parsers disagree on.
    pub is_canonical: bool,
}

/// Parse a host string as an IP address the way `inet_aton` and system resolvers do, and say
/// how it was spelled. The input is a host as it appears in a URL: ASCII case does not
/// matter, IPv6 may be bracketed, and one trailing dot is allowed (and makes an IPv4
/// spelling non-canonical).
///
/// Accepted, with the result:
///
/// | Input | `addr` | `kind` | `is_canonical` |
/// |---|---|---|---|
/// | `1.1.1.1` | 1.1.1.1 | `Ipv4DottedQuad` | true |
/// | `127.1` | 127.0.0.1 | `Ipv4Legacy` | false |
/// | `10.1.2` | 10.1.0.2 | `Ipv4Legacy` | false |
/// | `0177.0.0.1` (octal parts) | 127.0.0.1 | `Ipv4Legacy` | false |
/// | `0x7f.0.0.1`, `0x7f000001` (hex) | 127.0.0.1 | `Ipv4Legacy` | false |
/// | `2130706433` (one 32-bit part) | 127.0.0.1 | `Ipv4Legacy` | false |
/// | `1.1.1.1.` (trailing dot) | 1.1.1.1 | `Ipv4Legacy` | false |
/// | `::1`, `[::1]`, `::ffff:127.0.0.1` | as written | `Ipv6` | true |
///
/// `inet_aton` rules: one to four parts; each part is decimal, octal (a leading `0`) or hex
/// (`0x`); every part but the last is at most 255; the last part fills the remaining bytes
/// (so `a.b.c` has a 16-bit last part and `a.b` a 24-bit one). Returns `None` for hostnames,
/// for more than four parts, for out-of-range parts, for a digit outside the part's radix
/// (`08.1`), and for empty parts.
pub fn parse_host_ip(host: &str) -> Option<ParsedHostIp> {
    let trimmed = host.trim_matches(['[', ']']);
    if let Ok(ip) = trimmed.parse::<IpAddr>() {
        let (kind, is_canonical) = match ip {
            IpAddr::V4(_) => (HostIpKind::Ipv4DottedQuad, true),
            IpAddr::V6(_) => (HostIpKind::Ipv6, true),
        };
        return Some(ParsedHostIp {
            addr: ip,
            kind,
            is_canonical,
        });
    }
    let (body, trailing_dot) = match trimmed.strip_suffix('.') {
        Some(b) => (b, true),
        None => (trimmed, false),
    };
    let v4 = parse_legacy_ipv4(body)?;
    let canonical = !trailing_dot && body.eq_ignore_ascii_case(&v4.to_string());
    Some(ParsedHostIp {
        addr: IpAddr::V4(v4),
        kind: if canonical {
            HostIpKind::Ipv4DottedQuad
        } else {
            HostIpKind::Ipv4Legacy
        },
        is_canonical: canonical,
    })
}

/// Parse an IPv4 address with the `inet_aton` rules described at [`parse_host_ip`]. This
/// accepts every spelling, canonical or not, and does not accept a trailing dot, brackets
/// or IPv6; use [`parse_host_ip`] when the spelling matters.
pub fn parse_legacy_ipv4(host: &str) -> Option<Ipv4Addr> {
    let parts: Vec<&str> = host.split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut values = Vec::with_capacity(parts.len());
    for part in &parts {
        let (digits, radix) =
            if let Some(hex) = part.strip_prefix("0x").or_else(|| part.strip_prefix("0X")) {
                (hex, 16)
            } else if part.len() > 1 && part.starts_with('0') {
                (&part[1..], 8)
            } else {
                (*part, 10)
            };
        if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
            return None;
        }
        values.push(u64::from_str_radix(digits, radix).ok()?);
    }
    let (last, init) = values.split_last()?;
    if init.iter().any(|v| *v > 255) {
        return None;
    }
    let tail_bytes = 4 - init.len();
    if *last >= 1u64 << (8 * tail_bytes) {
        return None;
    }
    let mut addr: u32 = 0;
    for v in init {
        addr = (addr << 8) | *v as u32;
    }
    addr = addr.checked_shl(8 * tail_bytes as u32).unwrap_or(0) | *last as u32;
    Some(Ipv4Addr::from(addr))
}

/// Hostnames that always mean "cloud instance metadata".
fn is_metadata_host(host: &str) -> bool {
    matches!(
        host,
        "metadata" | "metadata.google.internal" | "metadata.goog" | "instance-data"
    ) || host.ends_with(".metadata.google.internal")
}

/// Lowercase, strip IPv6 brackets and one trailing dot.
pub(crate) fn normalize_host(host: &str) -> String {
    let h = host.trim_matches(['[', ']']).to_ascii_lowercase();
    h.strip_suffix('.').map(str::to_string).unwrap_or(h)
}

/// A CIDR range such as `10.0.0.0/8` or `fd00::/8`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct IpNet {
    addr: IpAddr,
    prefix: u8,
}

/// `IpNet` text or prefix length was invalid.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseIpNetError(pub String);

impl fmt::Display for ParseIpNetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid CIDR range: {}", self.0)
    }
}
impl Error for ParseIpNetError {}

impl IpNet {
    /// `prefix` is bits of the network part: at most 32 for IPv4, 128 for IPv6.
    pub fn new(addr: IpAddr, prefix: u8) -> Result<Self, ParseIpNetError> {
        let max = if addr.is_ipv4() { 32 } else { 128 };
        if prefix > max {
            return Err(ParseIpNetError(format!("prefix {prefix} exceeds {max}")));
        }
        Ok(Self { addr, prefix })
    }

    pub fn addr(&self) -> IpAddr {
        self.addr
    }

    pub fn prefix(&self) -> u8 {
        self.prefix
    }

    /// True when `ip` is in the range. Families must match; use [`embedded_v4`] first to
    /// test an IPv6 form against an IPv4 range.
    pub fn contains(&self, ip: &IpAddr) -> bool {
        match (self.addr, ip) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = if self.prefix == 0 {
                    0
                } else {
                    u32::MAX << (32 - u32::from(self.prefix))
                };
                u32::from(net) & mask == u32::from(*ip) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = if self.prefix == 0 {
                    0
                } else {
                    u128::MAX << (128 - u32::from(self.prefix))
                };
                u128::from(net) & mask == u128::from(*ip) & mask
            }
            _ => false,
        }
    }
}

impl FromStr for IpNet {
    type Err = ParseIpNetError;

    /// `a.b.c.d/len`, `x::/len`, or a bare address (a single-host range).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (addr, prefix) = match s.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (s, None),
        };
        let addr: IpAddr = addr
            .trim()
            .parse()
            .map_err(|_| ParseIpNetError(s.to_string()))?;
        let prefix = match prefix {
            Some(p) => p
                .trim()
                .parse::<u8>()
                .map_err(|_| ParseIpNetError(s.to_string()))?,
            None => {
                if addr.is_ipv4() {
                    32
                } else {
                    128
                }
            }
        };
        Self::new(addr, prefix)
    }
}

impl fmt::Display for IpNet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

/// Why an address or host was refused. Converts to [`crate::HttpError::AddressDenied`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AddressDenial {
    pub host: String,
    /// The offending address; `None` when the denial is about the name or scheme.
    pub addr: Option<IpAddr>,
    pub class: Option<AddressClass>,
    pub reason: String,
}

impl fmt::Display for AddressDenial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "address denied for host '{}': {}",
            self.host, self.reason
        )
    }
}
impl Error for AddressDenial {}

type DenyHook = Arc<dyn Fn(&IpAddr) -> Option<String> + Send + Sync>;
type AllowHook = Arc<dyn Fn(&IpAddr) -> bool + Send + Sync>;
type ResolverHook = Arc<dyn Fn(&str) -> Vec<IpAddr> + Send + Sync>;

#[derive(Clone)]
enum Mode {
    PublicWeb,
    FirstParty(Vec<String>),
}

#[derive(Clone)]
struct AllowRule {
    hosts: Vec<String>,
    /// Applies to every host, not only the listed ones (see [`AddressPolicy::allow_if_any_host`]).
    any_host: bool,
    pred: AllowHook,
}

/// Outbound address policy carried by [`crate::ClientConfig::address_policy`].
///
/// The default is public-web policy: only public addresses are reachable. Callers add
/// extra denials with [`AddressPolicy::deny_ranges`] and [`AddressPolicy::deny_if`], and
/// open specific non-public destinations with [`AddressPolicy::allow_if`]. Custom denials
/// always win over allowances.
#[derive(Clone)]
pub struct AddressPolicy {
    mode: Mode,
    deny_ranges: Vec<IpNet>,
    deny_if: Vec<DenyHook>,
    allow: Vec<AllowRule>,
    /// First-party hosts allowed to resolve to non-public addresses.
    private_ok: Vec<String>,
    /// Follow an HTTPS to HTTP redirect (refused by default).
    allow_downgrade: bool,
    /// Accept non-canonical IPv4 host spellings (refused by default).
    allow_legacy_ipv4: bool,
    /// Refuse a host whose answer mixes loopback with non-loopback addresses.
    refuse_mixed: bool,
    resolver: Option<ResolverHook>,
}

/// Why [`AddressPolicy::resolve_vetted_async`] refused a host.
#[derive(Debug)]
pub enum ResolveError {
    /// A private address (RFC 1918, CGNAT or unique-local) that the policy would admit once the
    /// caller approves it, for example through [`AddressPolicy::allow_private_resolution`].
    Private { addr: IpAddr },
    /// Refused outright: link-local or metadata, a denied range, a custom denial, or a name rule.
    Blocked {
        addr: Option<IpAddr>,
        class: Option<AddressClass>,
        reason: String,
    },
    /// The name did not resolve, or the lookup failed.
    Dns(HttpError),
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveError::Private { addr } => {
                write!(f, "address {addr} is private and needs approval")
            }
            ResolveError::Blocked { reason, .. } => write!(f, "address refused: {reason}"),
            ResolveError::Dns(e) => write!(f, "name lookup failed: {e}"),
        }
    }
}

impl Error for ResolveError {}

impl Default for AddressPolicy {
    fn default() -> Self {
        Self::public_web()
    }
}

impl fmt::Debug for AddressPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("AddressPolicy");
        match &self.mode {
            Mode::PublicWeb => d.field("mode", &"public-web"),
            Mode::FirstParty(hosts) => d.field("mode", &format_args!("first-party {hosts:?}")),
        };
        d.field("deny_ranges", &self.deny_ranges)
            .field("deny_if", &self.deny_if.len())
            .field(
                "allow_if_hosts",
                &self
                    .allow
                    .iter()
                    .flat_map(|r| r.hosts.iter())
                    .collect::<Vec<_>>(),
            )
            .field("private_resolution_hosts", &self.private_ok)
            .field("allow_insecure_redirects", &self.allow_downgrade)
            .field("allow_legacy_ipv4", &self.allow_legacy_ipv4)
            .field("refuse_mixed_loopback", &self.refuse_mixed)
            .field("custom_resolver", &self.resolver.is_some())
            .finish()
    }
}

impl AddressPolicy {
    /// Public-web policy: only public addresses are reachable. This is the default.
    pub fn public_web() -> Self {
        Self {
            mode: Mode::PublicWeb,
            deny_ranges: Vec::new(),
            deny_if: Vec::new(),
            allow: Vec::new(),
            private_ok: Vec::new(),
            allow_downgrade: false,
            allow_legacy_ipv4: false,
            refuse_mixed: false,
            resolver: None,
        }
    }

    /// First-party policy: only the listed hosts (exact match, ASCII case-insensitive, one
    /// trailing dot ignored), only over HTTPS, on any redirect hop as well. The resolved
    /// addresses of those hosts are classified with the public-web classes too, so a listed
    /// name that resolves to a private address is refused; opt a host out with
    /// [`AddressPolicy::allow_private_resolution`]. Link-local and metadata destinations stay
    /// denied and the custom denials below still apply.
    pub fn first_party(hosts: Vec<String>) -> Self {
        Self {
            mode: Mode::FirstParty(hosts.iter().map(|h| normalize_host(h)).collect()),
            ..Self::public_web()
        }
    }

    /// True for a policy built with [`AddressPolicy::first_party`].
    pub fn is_first_party(&self) -> bool {
        matches!(self.mode, Mode::FirstParty(_))
    }

    /// Accept non-canonical IPv4 host spellings (`127.1`, `0177.0.0.1`, `0x7f.0.0.1`,
    /// `2130706433`, `1.1.1.1.`) and read them as `inet_aton` does. They are refused by
    /// default with [`crate::HttpError::AddressDenied`], because parsers disagree about
    /// them (some read `0177.0.0.1` as decimal 177), and that mismatch is an SSRF bypass.
    /// No legitimate URL spells a host this way. Cloud metadata and the address classes
    /// still apply to an accepted spelling.
    pub fn allow_legacy_ipv4(mut self) -> Self {
        self.allow_legacy_ipv4 = true;
        self
    }

    /// Follow redirects from HTTPS to plain HTTP. Refused by default: the redirect target
    /// (often a signed URL) would travel unencrypted and the answer could not be trusted.
    pub fn allow_insecure_redirects(mut self) -> Self {
        self.allow_downgrade = true;
        self
    }

    /// False for a redirect from HTTPS to plain HTTP, unless
    /// [`AddressPolicy::allow_insecure_redirects`] was set. Other transports use it to apply
    /// the same rule.
    pub fn allows_redirect(&self, from_scheme: &str, to_scheme: &str) -> bool {
        self.allow_downgrade
            || !from_scheme.eq_ignore_ascii_case("https")
            || !to_scheme.eq_ignore_ascii_case("http")
    }

    /// Refuse an HTTPS to HTTP redirect unless [`AddressPolicy::allow_insecure_redirects`].
    pub(crate) fn check_redirect_scheme(
        &self,
        from_scheme: &str,
        to_scheme: &str,
        host: &str,
    ) -> Result<(), AddressDenial> {
        if self.allows_redirect(from_scheme, to_scheme) {
            return Ok(());
        }
        Err(denial(
            &normalize_host(host),
            None,
            None,
            "redirect from HTTPS to HTTP refused",
        ))
    }

    /// Let a first-party `host` resolve to non-public addresses (loopback, RFC 1918, ...),
    /// for example an on-premises server. Exact host match like [`AddressPolicy::first_party`].
    /// Link-local and metadata addresses, `deny_ranges` and `deny_if` still refuse. It has no
    /// effect on hosts that are not first-party.
    pub fn allow_private_resolution(mut self, host: impl Into<String>) -> Self {
        self.private_ok.push(normalize_host(&host.into()));
        self
    }

    /// Deny these ranges in addition to the class rules. An IPv6 address that embeds an
    /// IPv4 address is also tested by its embedded IPv4 address.
    pub fn deny_ranges(mut self, ranges: Vec<IpNet>) -> Self {
        self.deny_ranges.extend(ranges);
        self
    }

    /// Deny addresses for which `f` returns `Some(text)`. The text becomes the `reason` of
    /// [`crate::HttpError::AddressDenied`]. `f` also sees the embedded IPv4 address of an
    /// IPv6 form. Several hooks run in the order added.
    pub fn deny_if<F>(mut self, f: F) -> Self
    where
        F: Fn(&IpAddr) -> Option<String> + Send + Sync + 'static,
    {
        self.deny_if.push(Arc::new(f));
        self
    }

    /// Let the explicitly listed `hosts` (exact match, like [`AddressPolicy::first_party`])
    /// reach addresses for which `pred` is true, even when their class is denied by default.
    /// It does not apply to other hosts, never overrides link-local or metadata denials, and
    /// never overrides `deny_ranges` or `deny_if`.
    pub fn allow_if<F>(mut self, hosts: Vec<String>, pred: F) -> Self
    where
        F: Fn(&IpAddr) -> bool + Send + Sync + 'static,
    {
        self.allow.push(AllowRule {
            hosts: hosts.iter().map(|h| normalize_host(h)).collect(),
            any_host: false,
            pred: Arc::new(pred),
        });
        self
    }

    /// Like [`AddressPolicy::allow_if`], but `pred` applies to every host: any name may reach
    /// an address for which `pred` is true, even when its class is denied by default. Link-local
    /// and metadata destinations, `deny_ranges` and `deny_if` still refuse. Use it only when the
    /// predicate is the whole policy for the destination.
    pub fn allow_if_any_host<F>(mut self, pred: F) -> Self
    where
        F: Fn(&IpAddr) -> bool + Send + Sync + 'static,
    {
        self.allow.push(AllowRule {
            hosts: Vec::new(),
            any_host: true,
            pred: Arc::new(pred),
        });
        self
    }

    /// Allow loopback addresses (`127/8`, `::1`, and IPv4 forms embedded in IPv6) for every host,
    /// for a local-first app whose loopback services are its own. Everything else stays public-only.
    pub fn allow_loopback(self) -> Self {
        self.allow_if_any_host(|ip| classify_ip(*ip) == AddressClass::Loopback)
    }

    /// Local-first policy: loopback plus RFC 1918 IPv4 (`10/8`, `172.16/12`, `192.168/16`) for
    /// every host, public-only otherwise. Link-local and metadata destinations stay hard-blocked,
    /// and CGNAT and IPv6 unique-local addresses are not admitted by this rule.
    pub fn local_first(self) -> Self {
        self.allow_if_any_host(|ip| {
            if classify_ip(*ip) == AddressClass::Loopback {
                return true;
            }
            let v4 = match ip {
                IpAddr::V4(v) => Some(*v),
                IpAddr::V6(_) => embedded_v4(*ip),
            };
            v4.is_some_and(|v| v.is_private())
        })
    }

    /// Refuse a host whose DNS answer mixes loopback with non-loopback addresses. A host that
    /// resolves to both is the shape of a DNS-rebinding attempt against a loopback service, and
    /// `allow_loopback` or `local_first` would otherwise admit it.
    pub fn refuse_mixed_loopback(mut self) -> Self {
        self.refuse_mixed = true;
        self
    }

    /// Replace the system resolver (offline tests, pinned DNS, split-horizon setups). The
    /// hook gets the normalized host name and returns every address for it.
    pub fn with_resolver<F>(mut self, f: F) -> Self
    where
        F: Fn(&str) -> Vec<IpAddr> + Send + Sync + 'static,
    {
        self.resolver = Some(Arc::new(f));
        self
    }

    /// Resolve `host` once and vet every address with exactly the rules the HTTP clients
    /// use: the address classes, `deny_ranges`, `deny_if`, `allow_if`, the first-party host
    /// pin and its private-resolution opt-outs, metadata names, and the rule that a name with
    /// no address is denied. Returns the vetted socket addresses on `port`. The error is the
    /// same [`HttpError::AddressDenied`] the clients return; a failed lookup is a
    /// [`HttpError::Transport`] or [`HttpError::Timeout`] in [`Phase::Connect`].
    ///
    /// Other transports (a WebSocket, a raw TCP or gRPC client) use it to apply the identical
    /// policy, and **must connect to the returned addresses and not look the name up again**.
    /// A transport that cannot pin the connection to these addresses leaves a DNS-rebinding
    /// window between this check and its own lookup.
    ///
    /// The host is treated as reached over HTTPS: a first-party policy checks the host pin
    /// but cannot see the caller's transport, so requiring TLS stays the caller's job. A
    /// system lookup is bounded at ten seconds.
    pub fn resolve_vetted(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, HttpError> {
        self.admit("https", host)?;
        let host = normalize_host(host);
        let addrs = match self.fixed_addrs(&host) {
            Some(addrs) => addrs,
            None => match self.lookup_blocking(&host, port, Some(Duration::from_secs(10))) {
                Ok(addrs) => addrs,
                Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                    return Err(HttpError::timeout("name lookup", Phase::Connect))
                }
                Err(_) => return Err(HttpError::transport("host not found", Phase::Connect)),
            },
        };
        let vetted = self.vet(&host, addrs)?;
        Ok(vetted
            .into_iter()
            .map(|ip| SocketAddr::new(ip, port))
            .collect())
    }

    /// True when some `allow_if` rule lists `host`.
    fn host_listed(&self, host: &str) -> bool {
        self.allow
            .iter()
            .any(|rule| rule.hosts.iter().any(|h| h == host))
    }

    /// Vet a destination that a proxy will resolve, so its address cannot be checked or
    /// pinned here. IP literals (and `localhost`) are classified like any address. Names
    /// are refused when they are `localhost`, `*.localhost`, `*.local`, `*.internal`, or
    /// have a single label, unless an `allow_if` rule lists the host. First-party hosts are
    /// pinned by [`AddressPolicy::admit`] instead and skip the name rules.
    pub(crate) fn vet_via_proxy(&self, host: &str) -> Result<(), AddressDenial> {
        let host = normalize_host(host);
        if let Some(addrs) = self.fixed_addrs(&host) {
            return self.vet(&host, addrs).map(|_| ());
        }
        if matches!(self.mode, Mode::FirstParty(_)) || self.host_listed(&host) {
            return Ok(());
        }
        let reason = if host.ends_with(".local") {
            Some("mDNS name is refused through a proxy")
        } else if host.ends_with(".internal") {
            Some("internal name is refused through a proxy")
        } else if !host.contains('.') {
            Some("single-label name is refused through a proxy")
        } else {
            None
        };
        match reason {
            Some(r) => Err(denial(
                &host,
                None,
                Some(AddressClass::Private),
                format!("{r} (through a proxy only names and literals can be checked)"),
            )),
            None => Ok(()),
        }
    }

    /// Name- and scheme-level admission, before any lookup: metadata host names, and the
    /// first-party host and HTTPS pin.
    ///
    /// `host` should be the host exactly as the URL spelled it (trailing dot included), so a
    /// non-canonical IPv4 spelling can be refused.
    pub(crate) fn admit(&self, scheme: &str, host: &str) -> Result<(), AddressDenial> {
        if !self.allow_legacy_ipv4 {
            if let Some(parsed) = parse_host_ip(host) {
                if !parsed.is_canonical {
                    return Err(denial(
                        &normalize_host(host),
                        Some(parsed.addr),
                        Some(classify_ip(parsed.addr)),
                        format!(
                            "non-canonical IPv4 spelling is refused (it reads as {})",
                            parsed.addr
                        ),
                    ));
                }
            }
        }
        let host = normalize_host(host);
        if is_metadata_host(&host) {
            return Err(denial(
                &host,
                None,
                Some(AddressClass::LinkLocal),
                "host is a cloud metadata endpoint",
            ));
        }
        if let Mode::FirstParty(hosts) = &self.mode {
            if !scheme.eq_ignore_ascii_case("https") {
                return Err(denial(
                    &host,
                    None,
                    None,
                    "first-party client allows HTTPS only",
                ));
            }
            if !hosts.contains(&host) {
                return Err(denial(
                    &host,
                    None,
                    None,
                    "host is not in the first-party host list",
                ));
            }
        }
        Ok(())
    }

    /// Addresses that need no lookup: IP literals and `localhost` names.
    pub(crate) fn fixed_addrs(&self, host: &str) -> Option<Vec<IpAddr>> {
        let host = normalize_host(host);
        if let Some(parsed) = parse_host_ip(&host) {
            return Some(vec![parsed.addr]);
        }
        if host == "localhost" || host.ends_with(".localhost") {
            return Some(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]);
        }
        None
    }

    /// Resolve `host` with the configured hook or the system resolver. `limit` bounds the
    /// lookup, hook included. `Ok(empty)` means the name resolved to nothing.
    pub(crate) fn lookup_blocking(
        &self,
        host: &str,
        port: u16,
        limit: Option<Duration>,
    ) -> io::Result<Vec<IpAddr>> {
        let host = normalize_host(host);
        let hook = self.resolver.clone();
        let run = move || -> io::Result<Vec<IpAddr>> {
            match hook {
                Some(hook) => Ok(hook(&host)),
                None => Ok((host, port).to_socket_addrs()?.map(|a| a.ip()).collect()),
            }
        };
        match limit {
            None => run(),
            Some(limit) => {
                let (tx, rx) = mpsc::sync_channel(1);
                std::thread::spawn(move || tx.send(run()).ok());
                match rx.recv_timeout(limit) {
                    Ok(r) => r,
                    Err(_) => Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "name lookup timed out",
                    )),
                }
            }
        }
    }

    /// The configured resolver hook, for the async resolver.
    #[cfg(feature = "async")]
    pub(crate) fn resolver_hook(&self) -> Option<ResolverHook> {
        self.resolver.clone()
    }

    /// Classify every resolved address and return them when all pass. One denied address
    /// denies the host. Names with no address are denied.
    pub(crate) fn vet(&self, host: &str, addrs: Vec<IpAddr>) -> Result<Vec<IpAddr>, AddressDenial> {
        self.vet_with(host, addrs)
    }

    fn vet_with(&self, host: &str, addrs: Vec<IpAddr>) -> Result<Vec<IpAddr>, AddressDenial> {
        let host = normalize_host(host);
        if addrs.is_empty() {
            return Err(denial(
                &host,
                None,
                None,
                "host did not resolve to any address",
            ));
        }
        if self.refuse_mixed {
            let loopback = addrs
                .iter()
                .any(|ip| classify_ip(*ip) == AddressClass::Loopback);
            let other = addrs
                .iter()
                .any(|ip| classify_ip(*ip) != AddressClass::Loopback);
            if loopback && other {
                return Err(denial(
                    &host,
                    None,
                    None,
                    "host resolves to both loopback and non-loopback addresses",
                ));
            }
        }
        let literal = parse_host_ip(&host).is_some();
        for &ip in &addrs {
            let class = classify_ip(ip);
            if let Some(why) = hard_block(ip) {
                return Err(denial(
                    &host,
                    Some(ip),
                    Some(class),
                    format!("{} {why}", subject(literal, ip)),
                ));
            }
            let forms: Vec<IpAddr> = std::iter::once(ip)
                .chain(embedded_v4s(ip).into_iter().map(IpAddr::V4))
                .collect();
            for form in forms.iter() {
                if let Some(net) = self.deny_ranges.iter().find(|n| n.contains(form)) {
                    return Err(denial(
                        &host,
                        Some(ip),
                        Some(class),
                        format!("{} is in denied range {net}", subject(literal, ip)),
                    ));
                }
                for hook in &self.deny_if {
                    if let Some(text) = hook(form) {
                        return Err(denial(&host, Some(ip), Some(class), text));
                    }
                }
            }
            let private_ok =
                matches!(self.mode, Mode::FirstParty(_)) && self.private_ok.contains(&host);
            if class != AddressClass::Public && !private_ok && !self.allowed(&host, &ip) {
                let reason = if literal {
                    format!("address {ip} is {class:?}")
                } else {
                    format!("host resolves to {class:?} address {ip}")
                };
                return Err(denial(&host, Some(ip), Some(class), reason));
            }
        }
        Ok(addrs)
    }

    fn allowed(&self, host: &str, ip: &IpAddr) -> bool {
        self.allow
            .iter()
            .any(|rule| (rule.any_host || rule.hosts.iter().any(|h| h == host)) && (rule.pred)(ip))
    }

    #[cfg(feature = "async")]
    /// True when `ip` is an address an approval could admit: a private class that no
    /// `deny_ranges` entry or `deny_if` hook refuses.
    fn approvable(&self, ip: IpAddr) -> bool {
        classify_ip(ip) == AddressClass::Private && !self.hard_denied(ip)
    }

    #[cfg(feature = "async")]
    /// The hard-deny test [`AddressPolicy::vet`] applies: a metadata block, a `deny_ranges`
    /// entry or a `deny_if` hook, checked on the address and on every IPv4 form embedded in it
    /// (IPv4-mapped, NAT64 and similar), so no spelling of a denied address is approvable.
    fn hard_denied(&self, ip: IpAddr) -> bool {
        hard_block(ip).is_some()
            || std::iter::once(ip)
                .chain(embedded_v4s(ip).into_iter().map(IpAddr::V4))
                .any(|form| {
                    self.deny_ranges.iter().any(|n| n.contains(&form))
                        || self.deny_if.iter().any(|hook| hook(&form).is_some())
                })
    }

    #[cfg(feature = "async")]
    /// Map a denial from [`AddressPolicy::vet`] onto the async resolver's error. A private
    /// address is `Private` only when every address of the answer is either admitted already or
    /// approvable; anything else is `Blocked`, so a hard block is never hidden behind an approval.
    fn to_resolve_error(
        &self,
        host: &str,
        denial: AddressDenial,
        addrs: &[IpAddr],
    ) -> ResolveError {
        let ok = |ip: &IpAddr| {
            !self.hard_denied(*ip)
                && (classify_ip(*ip) == AddressClass::Public
                    || self.allowed(host, ip)
                    || self.approvable(*ip))
        };
        match denial.addr {
            Some(addr)
                if classify_ip(addr) == AddressClass::Private
                    && self.approvable(addr)
                    && addrs.iter().all(ok) =>
            {
                ResolveError::Private { addr }
            }
            _ => ResolveError::Blocked {
                addr: denial.addr,
                class: denial.class,
                reason: denial.reason,
            },
        }
    }

    /// Async form of [`AddressPolicy::resolve_vetted`] for an async caller (tokio, no
    /// `spawn_blocking`). It applies the same rules and returns the same vetted addresses, and
    /// it distinguishes the refusals an app can act on: a `Private` address may be approved,
    /// while `Blocked` (link-local, metadata, a denied range, a name rule) never is. A DNS
    /// failure or timeout is `Dns`. The lookup is bounded at ten seconds. The same
    /// connect-to-the-returned-addresses rule as `resolve_vetted` applies.
    #[cfg(feature = "async")]
    pub async fn resolve_vetted_async(
        &self,
        host: &str,
        port: u16,
    ) -> Result<Vec<SocketAddr>, ResolveError> {
        let host = normalize_host(host);
        self.admit("https", &host)
            .map_err(|d| self.to_resolve_error(&host, d, &[]))?;
        let addrs: Vec<IpAddr> = match self.fixed_addrs(&host) {
            Some(addrs) => addrs,
            None => match self.resolver_hook() {
                Some(hook) => hook(&host),
                None => match tokio::time::timeout(
                    Duration::from_secs(10),
                    tokio::net::lookup_host((host.as_str(), port)),
                )
                .await
                {
                    Ok(Ok(found)) => found.map(|a| a.ip()).collect(),
                    Ok(Err(_)) => {
                        return Err(ResolveError::Dns(HttpError::transport(
                            "host not found",
                            Phase::Connect,
                        )))
                    }
                    Err(_) => {
                        return Err(ResolveError::Dns(HttpError::timeout(
                            "name lookup",
                            Phase::Connect,
                        )))
                    }
                },
            },
        };
        if addrs.is_empty() {
            return Err(ResolveError::Dns(HttpError::transport(
                "host not found",
                Phase::Connect,
            )));
        }
        match self.vet(&host, addrs.clone()) {
            Ok(vetted) => Ok(vetted
                .into_iter()
                .map(|ip| SocketAddr::new(ip, port))
                .collect()),
            Err(d) => Err(self.to_resolve_error(&host, d, &addrs)),
        }
    }
}

fn subject(literal: bool, ip: IpAddr) -> String {
    if literal {
        format!("address {ip}")
    } else {
        format!("resolved address {ip}")
    }
}

fn denial(
    host: &str,
    addr: Option<IpAddr>,
    class: Option<AddressClass>,
    reason: impl Into<String>,
) -> AddressDenial {
    AddressDenial {
        host: host.to_string(),
        addr,
        class,
        reason: reason.into(),
    }
}

/// Find an [`AddressDenial`] anywhere in an error's source chain.
pub(crate) fn denial_in_chain(err: &(dyn Error + 'static)) -> Option<AddressDenial> {
    let mut current: Option<&(dyn Error + 'static)> = Some(err);
    while let Some(e) = current {
        if let Some(d) = e.downcast_ref::<AddressDenial>() {
            return Some(d.clone());
        }
        current = e.source();
    }
    None
}

thread_local! {
    /// True while the current thread is running a request that goes through the proxy.
    static VIA_PROXY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Marks the current thread's ureq call as routed through the proxy, so the resolver
/// treats the proxy endpoint (host and port) as trusted configuration. Without the mark
/// the same host and port is an ordinary destination and gets the full check, which is the
/// case for `NO_PROXY` hosts.
pub(crate) struct ViaProxy(bool);

impl ViaProxy {
    pub(crate) fn enter(on: bool) -> Self {
        Self(VIA_PROXY.with(|c| c.replace(on)))
    }
}

impl Drop for ViaProxy {
    fn drop(&mut self) {
        VIA_PROXY.with(|c| c.set(self.0));
    }
}

/// `ureq` keeps at most this many addresses per lookup (its `ResolvedSocketAddrs` capacity).
const MAX_RESOLVED_ADDRS: usize = 16;

/// Resolver for `ureq`: admits the URI, resolves it once, vets every address, and returns
/// only the vetted socket addresses. `ureq`'s `TcpConnector` connects to exactly the
/// addresses this returns (`ConnectionDetails::addrs`), so the check and the connection
/// use the same lookup.
#[derive(Debug)]
pub(crate) struct VettingResolver {
    pub(crate) policy: Arc<AddressPolicy>,
    /// Proxy endpoints `(normalized host, port)`: resolved but trusted (configuration, not
    /// input), so no address-class denial and no first-party host pin, but only while the
    /// call is marked [`ViaProxy`]. Usually one; a per-request system proxy configuration
    /// can name one per scheme.
    pub(crate) proxies: Vec<(String, u16)>,
    /// Bound for a name lookup when the request has no resolve or global timeout.
    pub(crate) lookup_limit: Duration,
}

impl ureq::unversioned::resolver::Resolver for VettingResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        _config: &ureq::config::Config,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        let scheme = uri.scheme_str().unwrap_or("").to_ascii_lowercase();
        let default_port = match scheme.as_str() {
            "http" => 80,
            "https" => 443,
            _ => return Err(ureq::Error::BadUri("unsupported scheme".into())),
        };
        let raw_host = uri
            .host()
            .ok_or_else(|| ureq::Error::BadUri("missing host".into()))?;
        let host = normalize_host(raw_host);
        let port = uri.port_u16().unwrap_or(default_port);
        let is_proxy = VIA_PROXY.with(|c| c.get())
            && self.proxies.iter().any(|(h, p)| *h == host && *p == port);
        if !is_proxy && !matches!(scheme.as_str(), "http" | "https") {
            return Err(ureq::Error::BadUri("unsupported scheme".into()));
        }
        if !is_proxy {
            self.policy.admit(&scheme, raw_host).map_err(to_ureq)?;
        }
        let addrs = match self.policy.fixed_addrs(&host) {
            Some(addrs) => addrs,
            None => {
                let limit = if timeout.after.is_not_happening() {
                    self.lookup_limit
                } else {
                    (*timeout.after).min(self.lookup_limit)
                };
                match self.policy.lookup_blocking(&host, port, Some(limit)) {
                    Ok(addrs) => addrs,
                    Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                        return Err(ureq::Error::Timeout(timeout.reason))
                    }
                    Err(_) => return Err(ureq::Error::HostNotFound),
                }
            }
        };
        let vetted = if is_proxy {
            if addrs.is_empty() {
                return Err(ureq::Error::HostNotFound);
            }
            addrs
        } else {
            self.policy.vet(&host, addrs).map_err(to_ureq)?
        };
        let mut out = ureq::unversioned::resolver::Resolver::empty(self);
        for ip in vetted.into_iter().take(MAX_RESOLVED_ADDRS) {
            out.push(std::net::SocketAddr::new(ip, port));
        }
        Ok(out)
    }
}

fn to_ureq(d: AddressDenial) -> ureq::Error {
    ureq::Error::Io(io::Error::new(io::ErrorKind::PermissionDenied, d))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn ipnet_parses_and_matches() {
        let net: IpNet = "10.0.0.0/8".parse().unwrap();
        assert!(net.contains(&ip("10.255.0.1")));
        assert!(!net.contains(&ip("11.0.0.1")));
        assert!(!net.contains(&ip("::1")));
        let v6: IpNet = "fd00::/8".parse().unwrap();
        assert!(v6.contains(&ip("fd12::1")));
        assert!(!v6.contains(&ip("fe80::1")));
        let host: IpNet = "203.0.113.9".parse().unwrap();
        assert_eq!(host.prefix(), 32);
        assert!(host.contains(&ip("203.0.113.9")) && !host.contains(&ip("203.0.113.8")));
        assert!("10.0.0.0/33".parse::<IpNet>().is_err());
        assert!("nonsense".parse::<IpNet>().is_err());
        let all: IpNet = "0.0.0.0/0".parse().unwrap();
        assert!(all.contains(&ip("8.8.8.8")));
    }

    #[test]
    fn embedded_forms_extract_the_v4() {
        assert_eq!(
            embedded_v4(ip("::ffff:10.1.2.3")),
            Some("10.1.2.3".parse().unwrap())
        );
        assert_eq!(
            embedded_v4(ip("::10.1.2.3")),
            Some("10.1.2.3".parse().unwrap())
        );
        assert_eq!(
            embedded_v4(ip("64:ff9b::a00:1")),
            Some("10.0.0.1".parse().unwrap())
        );
        assert_eq!(
            embedded_v4(ip("2002:0a00:0001::1")),
            Some("10.0.0.1".parse().unwrap())
        );
        assert_eq!(embedded_v4(ip("::1")), None);
        assert_eq!(embedded_v4(ip("::")), None);
        assert_eq!(embedded_v4(ip("2606:4700::1")), None);
        assert_eq!(embedded_v4(ip("1.2.3.4")), None);
    }

    #[test]
    fn host_ip_parsing_reports_the_spelling() {
        let k = |h: &str| parse_host_ip(h).map(|p| (p.addr, p.kind, p.is_canonical));
        let v4 = |s: &str| IpAddr::V4(s.parse().unwrap());
        assert_eq!(
            k("1.1.1.1"),
            Some((v4("1.1.1.1"), HostIpKind::Ipv4DottedQuad, true))
        );
        for (spelling, want) in [
            ("127.1", "127.0.0.1"),
            ("10.1.2", "10.1.0.2"),
            ("0177.0.0.1", "127.0.0.1"),
            ("0x7f.0.0.1", "127.0.0.1"),
            ("0X7F000001", "127.0.0.1"),
            ("2130706433", "127.0.0.1"),
            ("017700000001", "127.0.0.1"),
            ("0251.0376.0251.0376", "169.254.169.254"),
            ("1.1.1.1.", "1.1.1.1"),
            ("0", "0.0.0.0"),
        ] {
            assert_eq!(
                k(spelling),
                Some((v4(want), HostIpKind::Ipv4Legacy, false)),
                "{spelling}"
            );
        }
        for v6 in [
            "::1",
            "[::1]",
            "::ffff:127.0.0.1",
            "[::ffff:7f00:1]",
            "64:ff9b::a00:1",
        ] {
            let p = parse_host_ip(v6).unwrap();
            assert!(p.is_canonical && p.kind == HostIpKind::Ipv6, "{v6}");
        }
        for not_ip in [
            "example.com",
            "1.2.3.4.5",
            "256.1.1.1",
            "1.2.3.256",
            "4294967296",
            "08.1",
            "0x",
            "1..2",
            "",
            ".",
            "1.2.3.",
            "a.b.c.d",
            "127.0.0.1x",
        ] {
            let got = parse_host_ip(not_ip);
            // "1.2.3." has a trailing dot on a valid 3-part spelling: that is a legacy IPv4.
            if not_ip == "1.2.3." {
                assert!(got.is_some());
            } else {
                assert!(got.is_none(), "{not_ip}: {got:?}");
            }
        }
        assert_eq!(
            parse_legacy_ipv4("0x7f.1"),
            Some("127.0.0.1".parse().unwrap())
        );
        assert_eq!(parse_legacy_ipv4("1.1.1.1."), None);
        assert_eq!(parse_legacy_ipv4("::1"), None);
    }

    #[test]
    fn non_canonical_spellings_are_refused_by_default_and_allowed_on_request() {
        let strict = AddressPolicy::default();
        for h in ["0177.0.0.1", "0x7f.1", "2130706433", "127.1", "1.1.1.1."] {
            let d = strict.admit("http", h).unwrap_err();
            assert!(d.reason.contains("non-canonical"), "{h}: {d}");
            assert!(d.addr.is_some(), "{h}");
        }
        for h in [
            "1.1.1.1",
            "::ffff:1.1.1.1",
            "[::1]",
            "example.com",
            "example.com.",
        ] {
            assert!(strict.admit("http", h).is_ok(), "{h}");
        }
        let lax = AddressPolicy::default().allow_legacy_ipv4();
        assert!(lax.admit("http", "127.1").is_ok());
        // The address classes still apply to an accepted spelling.
        assert!(lax.vet("127.1", lax.fixed_addrs("127.1").unwrap()).is_err());
    }

    #[test]
    fn host_normalization() {
        assert_eq!(normalize_host("[::1]"), "::1");
        assert_eq!(normalize_host("Example.COM."), "example.com");
    }

    #[test]
    fn vet_denies_empty_resolution() {
        let d = AddressPolicy::default()
            .vet("nowhere.test", vec![])
            .unwrap_err();
        assert_eq!(d.addr, None);
        assert!(d.reason.contains("did not resolve"), "{d}");
    }
}

#[cfg(test)]
mod loopback_tests {
    use super::*;

    fn answers(addrs: Vec<IpAddr>) -> AddressPolicy {
        AddressPolicy::public_web().with_resolver(move |_| addrs.clone())
    }

    #[test]
    fn loopback_is_admitted_only_when_allowed() {
        let lo: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(answers(vec![lo]).resolve_vetted("svc.test", 80).is_err());
        assert!(answers(vec![lo])
            .allow_loopback()
            .resolve_vetted("svc.test", 80)
            .is_ok());
    }

    #[test]
    fn local_first_admits_rfc1918_but_not_cgnat_link_local_or_metadata() {
        let rfc1918: IpAddr = "192.168.1.5".parse().unwrap();
        assert!(answers(vec![rfc1918])
            .local_first()
            .resolve_vetted("svc.test", 80)
            .is_ok());
        let meta: IpAddr = "169.254.169.254".parse().unwrap();
        assert!(answers(vec![meta])
            .local_first()
            .resolve_vetted("svc.test", 80)
            .is_err());
        let cgnat: IpAddr = "100.64.0.1".parse().unwrap();
        assert!(answers(vec![cgnat])
            .local_first()
            .resolve_vetted("svc.test", 80)
            .is_err());
    }

    #[test]
    fn mixed_loopback_answers_are_refused_when_asked() {
        let mixed: Vec<IpAddr> = vec![
            "127.0.0.1".parse().unwrap(),
            "93.184.216.34".parse().unwrap(),
        ];
        assert!(answers(mixed.clone())
            .allow_loopback()
            .resolve_vetted("svc.test", 80)
            .is_ok());
        assert!(answers(mixed)
            .allow_loopback()
            .refuse_mixed_loopback()
            .resolve_vetted("svc.test", 80)
            .is_err());
    }
}

/// Where credential headers may be sent: one canonical origin plus a whole-segment path
/// prefix, for example `https://api.example.com/api`. Configured per request with
/// [`crate::Request::credential_scope`]; the clients strip credential-named and
/// secret-valued headers on the first request and on every redirect hop whose URL is
/// outside the scope, exactly as they already do for a cross-origin hop. The scope never
/// admits a destination: the outbound address policy still runs on every hop, so a
/// metadata or link-local target inside the path prefix stays denied.
///
/// Matching is by decoded path segment, never by string prefix: scope `/api` admits
/// `/api` and `/api/users`, and refuses `/apievil`, a `..` or `.` segment (raw or
/// percent-encoded), a segment that decodes to text containing `/` or `\`, and any path
/// containing a backslash. Origin comparison is canonical (scheme and host lowercased,
/// one trailing dot ignored, default ports folded), the same form the redirect rules use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialScope {
    scheme: String,
    host: String,
    port: u16,
    segments: Vec<String>,
}

impl CredentialScope {
    /// Parse an absolute `http`/`https` URL naming the scope, such as
    /// `https://api.example.com:8443/api`. `None` for anything malformed, a URL with
    /// userinfo, a query or a fragment, or a path the segment rules refuse (above).
    pub fn parse(scope: &str) -> Option<Self> {
        if scope.contains(['?', '#']) {
            return None;
        }
        let (scheme, host, port, segments, userinfo) = credential_url_parts(scope)?;
        if userinfo {
            return None;
        }
        Some(Self {
            scheme,
            host,
            port,
            segments,
        })
    }

    /// True when credentials scoped here may be sent to `url`: same canonical origin and
    /// a path whose decoded segments start with this scope's segments.
    pub fn allows(&self, url: &str) -> bool {
        let Some((scheme, host, port, segments, _)) = credential_url_parts(url) else {
            return false;
        };
        scheme == self.scheme
            && host == self.host
            && port == self.port
            && segments.len() >= self.segments.len()
            && segments[..self.segments.len()] == self.segments[..]
    }
}

/// `(scheme, canonical host, port, decoded path segments, had userinfo)` of an absolute
/// http(s) URL, or `None` when the URL (or its path) is outside every credential scope:
/// another scheme, no host, whitespace/control/backslash anywhere, a bad port, or a path
/// segment that is `.`/`..` or decodes to text containing a separator.
fn credential_url_parts(url: &str) -> Option<(String, String, u16, Vec<String>, bool)> {
    let p = url_authority_parts(url)?;
    let path = p.tail.split(['?', '#']).next().unwrap_or("");
    let segments = credential_path_segments(path)?;
    Some((p.scheme, p.host, p.port, segments, p.userinfo))
}

/// Canonical origin and shape of an absolute http(s) URL string, before any path rules.
struct UrlAuthorityParts<'a> {
    scheme: String,
    /// Lowercased, brackets and one trailing dot removed (see `normalize_host`).
    host: String,
    port: u16,
    /// The authority carried `user[:password]@`.
    userinfo: bool,
    /// Everything from the first `/`, `?` or `#` after the authority (possibly empty).
    tail: &'a str,
}

/// Split an absolute http(s) URL into its canonical origin, the userinfo flag and the rest.
/// `None` for another scheme, no host, whitespace/control/backslash anywhere, or a bad
/// port. Shared by [`CredentialScope`], [`ApprovedOrigins`] and the redirect shape check, so
/// all three read an origin the same way.
fn url_authority_parts(url: &str) -> Option<UrlAuthorityParts<'_>> {
    if url
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
    {
        return None;
    }
    let (scheme, rest) = url.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let scheme = scheme.to_ascii_lowercase();
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    let (host_port, userinfo) = match authority.rsplit_once('@') {
        Some((_, host_port)) => (host_port, true),
        None => (authority, false),
    };
    let (raw_host, port) = if let Some(inner) = host_port.strip_prefix('[') {
        let (literal, after) = inner.split_once(']')?;
        let port = match after {
            "" => None,
            _ => Some(after.strip_prefix(':')?.parse().ok()?),
        };
        (literal, port)
    } else if let Some((host, port)) = host_port.rsplit_once(':') {
        if port.is_empty() {
            (host, None)
        } else {
            (host, Some(port.parse().ok()?))
        }
    } else {
        (host_port, None)
    };
    if raw_host.is_empty() {
        return None;
    }
    let port = port.unwrap_or(if scheme == "https" { 443 } else { 80 });
    Some(UrlAuthorityParts {
        scheme,
        host: normalize_host(raw_host),
        port,
        userinfo,
        tail,
    })
}

/// Canonical `(scheme, host, port)` of an absolute http(s) URL and whether its authority
/// carries userinfo; `None` when the string is not a URL this crate will follow.
pub(crate) fn url_origin_parts(url: &str) -> Option<((String, String, u16), bool)> {
    let parts = url_authority_parts(url)?;
    Some(((parts.scheme, parts.host, parts.port), parts.userinfo))
}

/// The origins a request's redirects may reach besides the origin the request itself
/// started at, set with [`crate::Request::approved_redirect_origins`]. An origin is scheme,
/// host and port, compared canonically (scheme and host lowercased, one trailing dot
/// ignored, default ports folded), the same form [`CredentialScope`] and the redirect rules
/// use.
///
/// The request's own origin is always approved, so a same-origin redirect never needs an
/// entry; `ApprovedOrigins::new()` therefore means "this origin only". A redirect hop
/// outside the set is refused before anything is sent to it, with
/// [`crate::HttpError::RedirectRefused`] and [`RedirectRefusal::UnapprovedOrigin`].
///
/// The set decides where a redirect may go, not what it carries: a hop to another approved
/// origin still drops credential-named and secret-valued headers, and the address policy
/// still runs on every hop. A request without a set follows redirects to any origin the
/// address policy admits, as before.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApprovedOrigins {
    origins: Vec<(String, String, u16)>,
}

impl ApprovedOrigins {
    /// The request's own origin only.
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse origins such as `https://downloads.example.com` or
    /// `https://cdn.example.com:8443`. A single trailing `/` is accepted. Returns `None` when
    /// any entry is malformed, is not `http`/`https`, or carries userinfo, a path, a query
    /// or a fragment; no partial set is returned.
    pub fn parse<I, S>(origins: I) -> Option<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut set = Self::new();
        for origin in origins {
            set = set.with_origin(origin.as_ref())?;
        }
        Some(set)
    }

    /// Add one origin (same rules as [`ApprovedOrigins::parse`]); `None` when it is refused.
    pub fn with_origin(mut self, origin: &str) -> Option<Self> {
        let parts = url_authority_parts(origin)?;
        if parts.userinfo || !(parts.tail.is_empty() || parts.tail == "/") {
            return None;
        }
        let key = (parts.scheme, parts.host, parts.port);
        if !self.origins.contains(&key) {
            self.origins.push(key);
        }
        Some(self)
    }

    /// True when `url` is at one of the listed origins. The request's own origin is not
    /// listed here; the clients add it when they check a hop.
    pub fn allows(&self, url: &str) -> bool {
        url_authority_parts(url).is_some_and(|parts| {
            self.origins
                .iter()
                .any(|(s, h, p)| *s == parts.scheme && *h == parts.host && *p == parts.port)
        })
    }
}

/// Why a redirect target was refused by [`crate::HttpError::RedirectRefused`] or
/// [`check_redirect_target`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum RedirectRefusal {
    /// The target's origin is neither the request's origin nor in its [`ApprovedOrigins`].
    UnapprovedOrigin,
    /// The redirect leaves `https` for `http`. Returned by [`check_redirect_target`] only;
    /// the clients report this refusal as [`crate::HttpError::AddressDenied`] (see
    /// [`AddressPolicy::allow_insecure_redirects`]).
    SchemeDowngrade,
    /// The target's authority carries userinfo (`user:pass@host`).
    Credentials,
    /// The target is not a shape this check accepts: another scheme, no host, a bad port, or
    /// whitespace, control characters or a backslash anywhere in the URL.
    Malformed,
}

impl fmt::Display for RedirectRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UnapprovedOrigin => "redirect target origin is not approved",
            Self::SchemeDowngrade => "redirect from https to http",
            Self::Credentials => "redirect target carries credentials",
            Self::Malformed => "redirect target is malformed",
        })
    }
}

impl Error for RedirectRefusal {}

/// String-level guard for one redirect hop, for transports that follow redirects
/// themselves. `request_url` is the URL the call started at (its origin is always approved),
/// `from_url` the URL that answered with the redirect and `location` its `Location` value.
/// Returns the absolute target (fragment dropped) when the hop is acceptable.
///
/// Refuses, in this order: a `Location` that does not resolve to an absolute http(s) URL, or
/// that holds whitespace, control characters or a backslash ([`RedirectRefusal::Malformed`]);
/// an `https` to `http` hop ([`RedirectRefusal::SchemeDowngrade`], no opt-in here, unlike
/// [`AddressPolicy::allow_insecure_redirects`] on the clients); a target with userinfo
/// ([`RedirectRefusal::Credentials`]); and a target outside the request's origin and
/// `approved` ([`RedirectRefusal::UnapprovedOrigin`]). It reads strings only and neither
/// resolves a host nor applies an [`AddressPolicy`]. The clients run the same shape and
/// origin rules on every hop.
pub fn check_redirect_target(
    request_url: &str,
    from_url: &str,
    location: &str,
    approved: &ApprovedOrigins,
) -> Result<String, RedirectRefusal> {
    let next =
        crate::redirect::resolve_location(from_url, location).ok_or(RedirectRefusal::Malformed)?;
    let from_scheme = crate::redirect::scheme_of(from_url).ok_or(RedirectRefusal::Malformed)?;
    url_origin_parts(&next).ok_or(RedirectRefusal::Malformed)?;
    let to_scheme = crate::redirect::scheme_of(&next).ok_or(RedirectRefusal::Malformed)?;
    if from_scheme == "https" && to_scheme == "http" {
        return Err(RedirectRefusal::SchemeDowngrade);
    }
    crate::redirect::vet_target(request_url, &next, Some(approved))?;
    Ok(next)
}

/// Decoded, non-empty path segments, or `None` when a segment is `.`/`..` (as written or
/// after decoding), decodes to text containing `/` or `\`, or carries a malformed escape.
fn credential_path_segments(path: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for raw in path.split('/') {
        if raw.is_empty() {
            continue;
        }
        let bytes = raw.as_bytes();
        let mut decoded = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' {
                let digits = bytes.get(i + 1..i + 3)?;
                if !digits.iter().all(u8::is_ascii_hexdigit) {
                    return None;
                }
                let text = std::str::from_utf8(digits).ok()?;
                decoded.push(u8::from_str_radix(text, 16).ok()?);
                i += 3;
            } else {
                decoded.push(bytes[i]);
                i += 1;
            }
        }
        let segment = String::from_utf8(decoded).ok()?;
        if segment == "." || segment == ".." || segment.contains(['/', '\\']) {
            return None;
        }
        // A second decoding layer must not smuggle a separator or traversal either
        // (`%252e%252e`, `a%252fb`): a server that decodes twice would see them.
        if second_layer_is_unsafe(&segment) {
            return None;
        }
        out.push(segment);
    }
    Some(out)
}

/// True when decoding `segment` one more time would yield `.`/`..` or text containing
/// `/` or `\`. Only valid `%XX` escapes count; a bare `%` stays literal.
fn second_layer_is_unsafe(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let mut any_escape = false;
    while i < bytes.len() {
        let escape = if bytes[i] == b'%' {
            bytes
                .get(i + 1..i + 3)
                .filter(|d| d.len() == 2 && d.iter().all(u8::is_ascii_hexdigit))
                .and_then(|d| std::str::from_utf8(d).ok())
                .and_then(|t| u8::from_str_radix(t, 16).ok())
        } else {
            None
        };
        match escape {
            Some(byte) => {
                any_escape = true;
                decoded.push(byte);
                i += 3;
            }
            None => {
                decoded.push(bytes[i]);
                i += 1;
            }
        }
    }
    if !any_escape {
        return false;
    }
    match String::from_utf8(decoded) {
        Ok(text) => text == "." || text == ".." || text.contains(['/', '\\']),
        Err(_) => true,
    }
}

/// Why [`validate_public_https_url`] refused a URL.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum UrlPolicyError {
    /// The URL is not `https://`, or it has no `scheme://` separator.
    NotHttps,
    /// The authority carries userinfo (`user:pass@host`).
    Credentials,
    /// The host is empty.
    NoHost,
    /// The host is a non-public address, a local or internal name, or a single-label name.
    NonPublicHost,
    /// The authority or host is not a shape this check accepts (a legacy IPv4 spelling such as
    /// `0x7f.1`, or an unclosed IPv6 bracket).
    Malformed,
}

impl fmt::Display for UrlPolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotHttps => "URL must use https",
            Self::Credentials => "URL must not carry credentials",
            Self::NoHost => "URL has no host",
            Self::NonPublicHost => "URL host is not a public address",
            Self::Malformed => "URL is malformed",
        })
    }
}

impl Error for UrlPolicyError {}

/// Check that `url` is a plain public `https` URL: scheme `https` (any case), no userinfo, a
/// host that is a public IP literal or a multi-label name, and no `localhost`, `.localhost`,
/// `.local` or `.internal` name. The check reads the string only; it does not resolve the host.
/// The SSRF guard of [`crate::Client`] still vets the resolved addresses on every request.
/// Query strings and fragments are allowed and not inspected.
pub fn validate_public_https_url(url: &str) -> Result<(), UrlPolicyError> {
    let (scheme, rest) = url.split_once("://").ok_or(UrlPolicyError::NotHttps)?;
    if !scheme.eq_ignore_ascii_case("https") {
        return Err(UrlPolicyError::NotHttps);
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    if authority.contains('@') {
        return Err(UrlPolicyError::Credentials);
    }
    // Split the authority into host and optional port first. A bracketed host is an IPv6
    // literal; anything else splits at the last colon.
    let (bracketed, raw_host, port) = if let Some(inner) = authority.strip_prefix('[') {
        let (literal, after) = inner.split_once(']').ok_or(UrlPolicyError::Malformed)?;
        let port = match after {
            "" => None,
            _ => Some(after.strip_prefix(':').ok_or(UrlPolicyError::Malformed)?),
        };
        (true, literal, port)
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (false, host, Some(port)),
            None => (false, authority, None),
        }
    };
    if let Some(port) = port {
        validate_url_port(port)?;
    }
    if bracketed {
        let addr = raw_host
            .parse::<Ipv6Addr>()
            .map_err(|_| UrlPolicyError::Malformed)?;
        return if classify_ip(IpAddr::V6(addr)) == AddressClass::Public {
            Ok(())
        } else {
            Err(UrlPolicyError::NonPublicHost)
        };
    }
    // Normalize before any classification: decode percent escapes, lowercase, and drop the
    // trailing root dots, so `localhost.` and `%31%32%37.0.0.1` cannot pass as public names.
    // Delimiters are rejected here, for unbracketed text only.
    let host = normalize_url_host(raw_host)?;
    if let Some(parsed) = parse_host_ip(&host) {
        if parsed.kind == HostIpKind::Ipv4Legacy {
            return Err(UrlPolicyError::Malformed);
        }
        return if classify_ip(parsed.addr) == AddressClass::Public {
            Ok(())
        } else {
            Err(UrlPolicyError::NonPublicHost)
        };
    }
    let local_name = host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal");
    if local_name || !host.contains('.') {
        return Err(UrlPolicyError::NonPublicHost);
    }
    Ok(())
}

/// Accept an empty port (the scheme default) or a decimal port in `1..=65535`.
fn validate_url_port(port: &str) -> Result<(), UrlPolicyError> {
    if port.is_empty() {
        return Ok(());
    }
    if !port.bytes().all(|b| b.is_ascii_digit()) {
        return Err(UrlPolicyError::Malformed);
    }
    match port.parse::<u16>() {
        Ok(0) | Err(_) => Err(UrlPolicyError::Malformed),
        Ok(_) => Ok(()),
    }
}

/// Percent-decode a URL host, lowercase it, and strip trailing root dots. Escapes must be
/// two hex digits and the decoded host must not contain delimiters, controls or whitespace.
fn normalize_url_host(raw: &str) -> Result<String, UrlPolicyError> {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let digits = bytes.get(i + 1..i + 3).ok_or(UrlPolicyError::Malformed)?;
            if !digits.iter().all(u8::is_ascii_hexdigit) {
                return Err(UrlPolicyError::Malformed);
            }
            let text = std::str::from_utf8(digits).map_err(|_| UrlPolicyError::Malformed)?;
            decoded.push(u8::from_str_radix(text, 16).map_err(|_| UrlPolicyError::Malformed)?);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    let text = String::from_utf8(decoded).map_err(|_| UrlPolicyError::Malformed)?;
    if text.chars().any(|c| {
        c.is_control()
            || c.is_whitespace()
            || matches!(c, '/' | '\\' | '?' | '#' | '@' | '%' | '[' | ']' | ':')
    }) {
        return Err(UrlPolicyError::Malformed);
    }
    let host = text.to_ascii_lowercase();
    let host = host.trim_end_matches('.');
    if host.is_empty() {
        return Err(UrlPolicyError::NoHost);
    }
    Ok(host.to_string())
}

#[cfg(test)]
mod public_url_tests {
    use super::*;

    #[test]
    fn plain_public_https_urls_pass() {
        assert_eq!(
            validate_public_https_url("https://example.com/path?q=1#frag"),
            Ok(())
        );
        assert_eq!(
            validate_public_https_url("HTTPS://Example.COM:8443"),
            Ok(())
        );
        assert_eq!(validate_public_https_url("https://93.184.216.34/x"), Ok(()));
        assert_eq!(
            validate_public_https_url("https://[2606:4700::1111]/"),
            Ok(())
        );
    }

    #[test]
    fn non_https_and_credentials_are_refused() {
        assert_eq!(
            validate_public_https_url("http://example.com"),
            Err(UrlPolicyError::NotHttps)
        );
        assert_eq!(
            validate_public_https_url("example.com"),
            Err(UrlPolicyError::NotHttps)
        );
        assert_eq!(
            validate_public_https_url("https://user:pw@example.com"),
            Err(UrlPolicyError::Credentials)
        );
    }

    #[test]
    fn local_and_private_hosts_are_refused() {
        for url in [
            "https://localhost/",
            "https://api.localhost/",
            "https://printer.local/",
            "https://db.internal/",
            "https://intranet/",
            "https://127.0.0.1/",
            "https://10.0.0.5/",
            "https://169.254.169.254/latest",
            "https://[::1]/",
            "https://[fd00::1]/",
        ] {
            assert_eq!(
                validate_public_https_url(url),
                Err(UrlPolicyError::NonPublicHost),
                "{url}"
            );
        }
    }

    #[test]
    fn legacy_ipv4_spellings_and_empty_hosts_are_refused() {
        assert_eq!(
            validate_public_https_url("https://0x7f.1/"),
            Err(UrlPolicyError::Malformed)
        );
        assert_eq!(
            validate_public_https_url("https:///path"),
            Err(UrlPolicyError::NoHost)
        );
        assert_eq!(
            validate_public_https_url("https://[::1/"),
            Err(UrlPolicyError::Malformed)
        );
    }

    #[test]
    fn trailing_dot_local_names_are_refused() {
        for url in [
            "https://localhost./",
            "https://LOCALHOST../x",
            "https://printer.local./",
            "https://db.internal./",
            "https://api.localhost./",
            "https://intranet./",
            "https://127.0.0.1./",
        ] {
            assert_eq!(
                validate_public_https_url(url),
                Err(UrlPolicyError::NonPublicHost),
                "{url}"
            );
        }
    }

    #[test]
    fn percent_encoded_hosts_are_decoded_before_classification() {
        assert_eq!(
            validate_public_https_url("https://%31%32%37.0.0.1/"),
            Err(UrlPolicyError::NonPublicHost)
        );
        assert_eq!(
            validate_public_https_url("https://%31%32%37.0.0.1./"),
            Err(UrlPolicyError::NonPublicHost)
        );
        assert_eq!(
            validate_public_https_url("https://%6c%6f%63%61%6c%68%6f%73%74/"),
            Err(UrlPolicyError::NonPublicHost)
        );
        assert_eq!(
            validate_public_https_url("https://%2fexample.com/"),
            Err(UrlPolicyError::Malformed)
        );
        assert_eq!(
            validate_public_https_url("https://exa%3Fmple.com/"),
            Err(UrlPolicyError::Malformed)
        );
        assert_eq!(
            validate_public_https_url("https://example%2.com/"),
            Err(UrlPolicyError::Malformed)
        );
        assert_eq!(
            validate_public_https_url("https://%+1.example.com/"),
            Err(UrlPolicyError::Malformed)
        );
        assert_eq!(
            validate_public_https_url("https://%25.example.com/"),
            Err(UrlPolicyError::Malformed)
        );
    }

    #[test]
    fn trailing_dot_public_names_and_valid_ports_pass() {
        assert_eq!(validate_public_https_url("https://example.com./x"), Ok(()));
        assert_eq!(
            validate_public_https_url("https://example.com:443/"),
            Ok(())
        );
        assert_eq!(validate_public_https_url("https://example.com:/"), Ok(()));
        assert_eq!(
            validate_public_https_url("https://[2606:4700::1111]:8443/"),
            Ok(())
        );
    }

    #[test]
    fn invalid_ports_are_refused() {
        for url in [
            "https://example.com:0/",
            "https://example.com:65536/",
            "https://example.com:abc/",
            "https://example.com:+443/",
            "https://example.com:80:90/",
            "https://[2606:4700::1111]:0/",
            "https://[2606:4700::1111]x/",
        ] {
            assert_eq!(
                validate_public_https_url(url),
                Err(UrlPolicyError::Malformed),
                "{url}"
            );
        }
    }
}
