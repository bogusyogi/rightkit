//! Effect admission, network (SSRF/host) policy, and lifecycle events.
//!
//! Order per effect (EFF-001): the [`NetworkPolicy`] (policy stage) decides
//! first, then the caller-supplied [`AdmissionHook`] (approval stage) sees a
//! typed [`AdmissionRequest`]; a denial at either stage happens before any side
//! effect. Every network request a page makes, including redirect hops and
//! subresources, is checked again inside the browser before it leaves the
//! machine, and every TCP connection Chrome opens goes through an in-process
//! pinning proxy that re-checks the address it actually connects to.
//!
//! Default policy blocks:
//! * loopback (`127.0.0.0/8`, `::1`, `localhost`, `*.localhost`),
//! * link-local (`169.254.0.0/16`, `fe80::/10`) including the cloud metadata
//!   address `169.254.169.254` and the `metadata.google.internal` names. These
//!   are *hard* blocks: no allow list, address-class allowance, or callback can
//!   open them unless [`NetworkPolicy::hard_block_link_local`] is turned off,
//! * private (`10/8`, `172.16/12`, `192.168/16`, `100.64/10` CGNAT,
//!   `fc00::/7`, `fec0::/10`), unspecified (`0.0.0.0/8`, `::`), multicast,
//!   and reserved/documentation/benchmark ranges,
//! * IPv6 forms that embed one of the above IPv4 addresses
//!   (`::ffff:a.b.c.d` mapped, `::a.b.c.d` compatible, `64:ff9b::/96` NAT64,
//!   `2002::/16` 6to4),
//! * hosts that do not resolve (fail closed),
//! * every scheme other than `http`/`https` (`file:`, `ws:`, `chrome:`, ...)
//!   unless listed in [`NetworkPolicy::allowed_schemes`]. `about:blank`, and
//!   `data:`/`blob:` *subresources*, never touch the network and are allowed;
//!   `data:`/`blob:` *documents* need the scheme to be allowed.
//!
//! Hostnames are resolved and every resolved address is checked (the worst
//! address decides). The resolution is cached and shared with the pinning
//! proxy, and the proxy only connects to addresses the request check vetted,
//! so a name that rebinds from public to private between check and connect
//! cannot reach a blocked address.
//!
//! Traffic page interception never sees (WebSocket handshakes, requests from
//! dedicated/shared/service workers, Chrome's own fetchers) still reaches the
//! pinning proxy. When a per-request callback is set, the proxy runs the same
//! check for every `host:port` no page check of this session allowed, with
//! resource type `Connect` and URL `https://host:port/` (a plain-HTTP proxy
//! request is checked with its full URL); a refusal is a typed `Denied` event
//! with `what = "Connect"` and nothing is connected (#925).
//!
//! Known gaps (documented, not silently covered):
//! * For that non-intercepted traffic the callback decides per `host:port`
//!   only; path-level decisions apply only to intercepted requests. A closed
//!   allowlist must return `Deny` for everything else: `Default` (or `false`)
//!   leaves public destinations allowed, as for any request.
//! * Once a page check allows a `host:port`, later non-intercepted traffic to
//!   the same pair is not decided again.
//! * [`crate::BrowserSession::pinned_connections`] also records failed
//!   connection attempts, so it is not an egress audit on its own.
//! * WebRTC UDP is restricted to proxied routes by Chrome flag; this is not
//!   independently verified by a test.

use futures::future::BoxFuture;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Admission (approval stage)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ActionKind {
    Navigate,
    Reload,
    History,
    Click,
    DoubleClick,
    Hover,
    Drag,
    Wheel,
    Press,
    TypeText,
    Fill,
    Select,
    Upload,
    Eval,
}

/// One typed request to perform an effect. Typed text is never included, only
/// its length, so admission logs stay content-free.
#[derive(Clone, Debug)]
pub struct AdmissionRequest {
    pub session_id: String,
    pub page_id: String,
    pub action: ActionKind,
    /// Destination for `Navigate`; otherwise the page's current URL.
    pub url: Option<String>,
    /// Human-readable target (selector, ref, point, key chord, file path).
    pub target: Option<String>,
    /// Extra detail: script source for `Eval`, `chars=N` for typing.
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Decision {
    Allow,
    Deny(String),
}

pub type AdmissionHook =
    Arc<dyn Fn(AdmissionRequest) -> BoxFuture<'static, Decision> + Send + Sync>;

// ---------------------------------------------------------------------------
// Address classification
// ---------------------------------------------------------------------------

/// Address class of a destination. Everything except `Public` is blocked by default.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum AddressClass {
    Public,
    Loopback,
    /// `169.254/16`, `fe80::/10`; includes the cloud metadata endpoint.
    LinkLocal,
    /// RFC 1918, CGNAT `100.64/10`, ULA `fc00::/7`, site-local `fec0::/10`.
    Private,
    /// IETF protocol assignments, documentation, benchmarking, `240/4`,
    /// broadcast, `2001:db8::/32`, Teredo.
    Reserved,
    Unspecified,
    Multicast,
}

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

fn classify_v6(v6: Ipv6Addr) -> AddressClass {
    let s = v6.segments();
    if v6.is_loopback() {
        return AddressClass::Loopback;
    }
    if v6.is_unspecified() {
        return AddressClass::Unspecified;
    }
    // ::ffff:a.b.c.d (IPv4-mapped).
    if let Some(v4) = v6.to_ipv4_mapped() {
        return classify_v4(v4);
    }
    // ::a.b.c.d (deprecated IPv4-compatible).
    if s[..6] == [0, 0, 0, 0, 0, 0] {
        return classify_v4(v4_from_segments(s[6], s[7]));
    }
    // 64:ff9b::/96 (NAT64) embeds an IPv4 address in the low 32 bits.
    if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        return classify_v4(v4_from_segments(s[6], s[7]));
    }
    // 2002::/16 (6to4) embeds an IPv4 address in bits 16..48.
    if s[0] == 0x2002 {
        return classify_v4(v4_from_segments(s[1], s[2]));
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

/// Hostnames that always mean "cloud instance metadata".
fn is_metadata_host(host: &str) -> bool {
    matches!(
        host,
        "metadata" | "metadata.google.internal" | "metadata.goog" | "instance-data"
    ) || host.ends_with(".metadata.google.internal")
}

/// Lowercase, strip IPv6 brackets and one trailing dot.
fn normalize_host(host: &str) -> String {
    let h = host.trim_matches(['[', ']']).to_ascii_lowercase();
    h.strip_suffix('.').map(str::to_string).unwrap_or(h)
}

// ---------------------------------------------------------------------------
// Network policy (policy stage)
// ---------------------------------------------------------------------------

/// Why the network policy refused a request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BlockReason {
    InvalidUrl(String),
    /// The scheme is not in `allowed_schemes`.
    Scheme(String),
    NoHost,
    /// The host has no addresses (fail closed).
    Unresolved {
        host: String,
    },
    /// The host is, or resolves to, a blocked address.
    Address {
        host: String,
        ip: IpAddr,
        class: AddressClass,
    },
    /// A cloud metadata hostname.
    MetadataHost {
        host: String,
    },
    /// The per-request callback denied it.
    Callback(String),
}

impl BlockReason {
    /// True when no allow list, class allowance, or callback can override it.
    pub fn is_hard(&self, hard_block_link_local: bool) -> bool {
        hard_block_link_local
            && matches!(
                self,
                BlockReason::MetadataHost { .. }
                    | BlockReason::Address {
                        class: AddressClass::LinkLocal,
                        ..
                    }
            )
    }
}

impl fmt::Display for BlockReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlockReason::InvalidUrl(e) => write!(f, "unparseable url: {e}"),
            BlockReason::Scheme(s) => write!(f, "scheme '{s}' is blocked"),
            BlockReason::NoHost => write!(f, "request has no host"),
            BlockReason::Unresolved { host } => write!(f, "host '{host}' did not resolve"),
            BlockReason::Address { host, ip, class } => {
                if host == &ip.to_string() {
                    write!(f, "address {ip} is {class:?}")
                } else {
                    write!(f, "host '{host}' resolves to {class:?} address {ip}")
                }
            }
            BlockReason::MetadataHost { host } => {
                write!(f, "host '{host}' is a cloud metadata endpoint")
            }
            BlockReason::Callback(r) => write!(f, "denied by policy callback: {r}"),
        }
    }
}

/// Typed network-policy denial returned as [`crate::BrowserError::Blocked`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicyDenial {
    pub url: String,
    pub resource_type: String,
    pub reason: BlockReason,
}

impl fmt::Display for PolicyDenial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}): {}", self.url, self.resource_type, self.reason)
    }
}

/// One network request about to leave the browser, as seen by the allow callback.
#[derive(Clone, Debug)]
pub struct BrowserRequest {
    pub url: String,
    pub scheme: String,
    /// Normalized host; empty for host-less schemes (`file:`, `data:`).
    pub host: String,
    pub port: Option<u16>,
    /// Worst class across `addresses`; `None` for host-less schemes.
    pub class: Option<AddressClass>,
    /// Every address the host resolved to (all are checked).
    pub addresses: Vec<IpAddr>,
    /// `Navigation`, `Document`, `Image`, `Script`, `Fetch`, ...
    pub resource_type: String,
    /// What the defaults would do: `None` allows, `Some(reason)` blocks.
    pub default_block: Option<BlockReason>,
}

/// Former name, kept for existing callers.
pub type NetworkRequest = BrowserRequest;

/// Answer of the per-request allow callback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PolicyDecision {
    /// Allow, overriding a default block (hard blocks still apply).
    Allow,
    /// Deny, even if the defaults would allow.
    Deny(String),
    /// Apply the configured defaults.
    Default,
}

/// `true` allows, `false` defers to the defaults.
impl From<bool> for PolicyDecision {
    fn from(b: bool) -> Self {
        if b {
            PolicyDecision::Allow
        } else {
            PolicyDecision::Default
        }
    }
}

pub type AllowCallback = Arc<dyn Fn(&BrowserRequest) -> PolicyDecision + Send + Sync>;
pub type Resolver = Arc<dyn Fn(&str) -> Vec<IpAddr> + Send + Sync>;

/// Per-host resolution shared by the request check and the pinning proxy.
type ResolvedCache = Arc<Mutex<HashMap<String, (Instant, Vec<IpAddr>)>>>;

/// Typed SSRF/host policy. See the module docs for defaults.
#[derive(Clone)]
pub struct NetworkPolicy {
    /// Hosts (exact, case-insensitive) allowed even when their address class is blocked.
    pub allow_hosts: Vec<String>,
    /// Address classes allowed for every host (e.g. `Loopback` for local dev servers).
    pub allow_classes: Vec<AddressClass>,
    /// Lowercase schemes that may be requested. Default `["http", "https"]`.
    pub allowed_schemes: Vec<String>,
    /// Called for every checked request; see [`PolicyDecision`].
    pub allow_request: Option<AllowCallback>,
    /// Link-local and metadata destinations cannot be allowed (default `true`).
    pub hard_block_link_local: bool,
    /// Disable every check (trusted callers only).
    pub unrestricted: bool,
    /// Replaces the system resolver (offline tests, pinned DNS, split-horizon setups).
    pub resolver: Option<Resolver>,
    resolved: ResolvedCache,
    /// Non-public addresses an explicit allow let through, per host.
    approved: Arc<Mutex<HashMap<String, Vec<IpAddr>>>>,
}

impl Default for NetworkPolicy {
    fn default() -> Self {
        Self {
            allow_hosts: Vec::new(),
            allow_classes: Vec::new(),
            allowed_schemes: vec!["http".into(), "https".into()],
            allow_request: None,
            hard_block_link_local: true,
            unrestricted: false,
            resolver: None,
            resolved: ResolvedCache::default(),
            approved: Arc::default(),
        }
    }
}

impl fmt::Debug for NetworkPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NetworkPolicy")
            .field("allow_hosts", &self.allow_hosts)
            .field("allow_classes", &self.allow_classes)
            .field("allowed_schemes", &self.allowed_schemes)
            .field("allow_request", &self.allow_request.is_some())
            .field("hard_block_link_local", &self.hard_block_link_local)
            .field("unrestricted", &self.unrestricted)
            .finish()
    }
}

/// Copy of the old string-only blocked record, kept per page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockedRequest {
    pub url: String,
    pub reason: String,
}

impl NetworkPolicy {
    /// Policy with every check off.
    pub fn unrestricted() -> Self {
        Self {
            unrestricted: true,
            ..Self::default()
        }
    }

    pub fn allow_host(mut self, host: impl Into<String>) -> Self {
        self.allow_hosts.push(normalize_host(&host.into()));
        self
    }

    pub fn allow_class(mut self, class: AddressClass) -> Self {
        self.allow_classes.push(class);
        self
    }

    pub fn allow_scheme(mut self, scheme: impl Into<String>) -> Self {
        self.allowed_schemes
            .push(scheme.into().to_ascii_lowercase());
        self
    }

    pub fn hard_block_link_local(mut self, on: bool) -> Self {
        self.hard_block_link_local = on;
        self
    }

    pub fn with_resolver(
        mut self,
        f: impl Fn(&str) -> Vec<IpAddr> + Send + Sync + 'static,
    ) -> Self {
        self.resolver = Some(Arc::new(f));
        self
    }

    /// Per-request callback. Returning `bool` works too: `true` allows,
    /// `false` defers to the defaults.
    pub fn allow_with<D: Into<PolicyDecision>>(
        mut self,
        f: impl Fn(&BrowserRequest) -> D + Send + Sync + 'static,
    ) -> Self {
        self.allow_request = Some(Arc::new(move |r| f(r).into()));
        self
    }

    fn scheme_allowed(&self, scheme: &str) -> bool {
        self.allowed_schemes.iter().any(|s| s == scheme)
    }

    fn address_allowed(&self, host: &str, class: AddressClass) -> bool {
        class == AddressClass::Public
            || self.allow_classes.contains(&class)
            || self.allow_hosts.iter().any(|h| h == host)
    }

    /// `Ok(())` when the request may proceed, otherwise why it is blocked.
    pub async fn check(&self, url: &str, resource_type: &str) -> Result<(), BlockReason> {
        if self.unrestricted {
            return Ok(());
        }
        let parsed = url::Url::parse(url).map_err(|e| BlockReason::InvalidUrl(e.to_string()))?;
        let scheme = parsed.scheme().to_ascii_lowercase();
        let is_document = matches!(resource_type, "Navigation" | "Document");
        if scheme == "about" && parsed.path() == "blank" {
            return Ok(());
        }
        if matches!(scheme.as_str(), "data" | "blob") && !is_document {
            return Ok(()); // inert subresource: never touches the network
        }
        let networked = matches!(scheme.as_str(), "http" | "https" | "ws" | "wss");
        let mut default_block =
            (!self.scheme_allowed(&scheme)).then(|| BlockReason::Scheme(scheme.clone()));
        let mut host = String::new();
        let mut class = None;
        let mut addresses = Vec::new();
        if networked {
            host = normalize_host(parsed.host_str().unwrap_or(""));
            if host.is_empty() {
                return Err(BlockReason::NoHost);
            }
            if is_metadata_host(&host) {
                let r = BlockReason::MetadataHost { host: host.clone() };
                if r.is_hard(self.hard_block_link_local) {
                    return Err(r);
                }
                default_block.get_or_insert(r);
            }
            let (c, worst, ips) = self.host_class(&host).await;
            class = Some(c);
            addresses = ips;
            if addresses.is_empty() {
                return Err(BlockReason::Unresolved { host });
            }
            if !self.address_allowed(&host, c)
                || (c == AddressClass::LinkLocal && self.hard_block_link_local)
            {
                let r = BlockReason::Address {
                    host: host.clone(),
                    ip: worst,
                    class: c,
                };
                if r.is_hard(self.hard_block_link_local) {
                    return Err(r);
                }
                default_block.get_or_insert(r);
            }
        }
        let decision = match &self.allow_request {
            Some(cb) => cb(&BrowserRequest {
                url: url.to_string(),
                scheme: scheme.clone(),
                host: host.clone(),
                port: parsed.port_or_known_default(),
                class,
                addresses: addresses.clone(),
                resource_type: resource_type.to_string(),
                default_block: default_block.clone(),
            }),
            None => PolicyDecision::Default,
        };
        let allowed = match decision {
            PolicyDecision::Allow => true,
            PolicyDecision::Deny(r) => return Err(BlockReason::Callback(r)),
            PolicyDecision::Default => match default_block {
                Some(r) => return Err(r),
                None => true,
            },
        };
        if allowed && networked && class != Some(AddressClass::Public) {
            self.approved
                .lock()
                .unwrap()
                .entry(host)
                .or_default()
                .extend(addresses);
        }
        Ok(())
    }

    /// Addresses the proxy may connect to for `host`: the same cached
    /// resolution the request check used. A non-public address passes only if
    /// its class is allowed, the host is listed, or the request check
    /// explicitly approved that exact address, so anything that bypassed
    /// interception (workers, rebinding) cannot reach a blocked range.
    pub(crate) async fn pin(&self, host: &str) -> Result<Vec<IpAddr>, BlockReason> {
        let host = normalize_host(host);
        if self.hard_block_link_local && is_metadata_host(&host) {
            return Err(BlockReason::MetadataHost { host });
        }
        let (_, _, ips) = self.host_class(&host).await;
        if ips.is_empty() {
            return Err(BlockReason::Unresolved { host });
        }
        let approved = self
            .approved
            .lock()
            .unwrap()
            .get(&host)
            .cloned()
            .unwrap_or_default();
        for ip in &ips {
            let class = classify_ip(*ip);
            let hard = self.hard_block_link_local && class == AddressClass::LinkLocal;
            let ok = !hard && (self.address_allowed(&host, class) || approved.contains(ip));
            if !ok {
                return Err(BlockReason::Address {
                    host,
                    ip: *ip,
                    class,
                });
            }
        }
        Ok(ips)
    }

    /// Worst class across the host's addresses (any blocked address blocks the
    /// host), the address that produced it, and every address.
    async fn host_class(&self, host: &str) -> (AddressClass, IpAddr, Vec<IpAddr>) {
        let unspecified = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
        if let Ok(ip) = host.parse::<IpAddr>() {
            return (classify_ip(ip), ip, vec![ip]);
        }
        if host == "localhost" || host.ends_with(".localhost") {
            let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
            return (AddressClass::Loopback, ip, vec![ip]);
        }
        let addrs = self.resolve(host).await;
        let worst = addrs
            .iter()
            .copied()
            .find(|ip| classify_ip(*ip) != AddressClass::Public);
        match worst {
            Some(ip) => (classify_ip(ip), ip, addrs),
            None => (
                AddressClass::Public,
                addrs.first().copied().unwrap_or(unspecified),
                addrs,
            ),
        }
    }

    async fn resolve(&self, host: &str) -> Vec<IpAddr> {
        const TTL: Duration = Duration::from_secs(30);
        if let Some((at, v)) = self.resolved.lock().unwrap().get(host) {
            if at.elapsed() < TTL {
                return v.clone();
            }
        }
        let found: Vec<IpAddr> = if let Some(r) = &self.resolver {
            r(host)
        } else {
            match tokio::time::timeout(Duration::from_secs(3), tokio::net::lookup_host((host, 0)))
                .await
            {
                Ok(Ok(it)) => it.map(|a| a.ip()).collect(),
                _ => Vec::new(), // fail closed: an unresolved host is refused
            }
        };
        self.resolved
            .lock()
            .unwrap()
            .insert(host.to_string(), (Instant::now(), found.clone()));
        found
    }
}

// ---------------------------------------------------------------------------
// Lifecycle events
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum LifecycleKind {
    Start,
    Stop,
    /// Chrome or a renderer died without being asked to.
    Crash,
    /// Admission or network policy refused something before any side effect.
    Denied,
}

impl LifecycleKind {
    pub fn label(self) -> &'static str {
        match self {
            LifecycleKind::Start => "start",
            LifecycleKind::Stop => "stop",
            LifecycleKind::Crash => "crash",
            LifecycleKind::Denied => "denied",
        }
    }
}

/// One structured, content-free lifecycle event for the caller's journal.
/// Never carries typed text, page content, or full URLs.
#[derive(Clone, Debug)]
pub struct BrowserEvent {
    pub kind: LifecycleKind,
    pub session_id: String,
    pub reason: String,
    pub at: SystemTime,
    pub page_id: Option<String>,
    /// Chrome's OS process id (start/stop/crash).
    pub pid: Option<u32>,
    /// What was denied: an [`ActionKind`] name, a resource type, or `Connect`.
    pub what: Option<String>,
    /// Typed network-policy reason for `Denied` events from the network policy.
    pub blocked: Option<BlockReason>,
}

impl BrowserEvent {
    pub(crate) fn new(kind: LifecycleKind, session_id: &str, reason: impl Into<String>) -> Self {
        Self {
            kind,
            session_id: session_id.to_string(),
            reason: reason.into(),
            at: SystemTime::now(),
            page_id: None,
            pid: None,
            what: None,
            blocked: None,
        }
    }

    pub fn timestamp_ms(&self) -> u64 {
        self.at
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "event": self.kind.label(),
            "session_id": self.session_id,
            "reason": self.reason,
            "ts_ms": self.timestamp_ms(),
            "page_id": self.page_id,
            "pid": self.pid,
            "what": self.what,
        })
    }
}

pub type EventSink = Arc<dyn Fn(&BrowserEvent) + Send + Sync>;

/// Everything pages share about effect control.
pub(crate) struct Guard {
    pub session_id: String,
    pub admission: Option<AdmissionHook>,
    pub network: NetworkPolicy,
    pub events: Option<EventSink>,
    /// This session's `(host, port)` pairs a page request check allowed; the proxy does not
    /// decide those again (#925).
    pub page_allowed: Mutex<HashSet<(String, u16)>>,
}

/// The `(host, port)` an allowed networked request leaves for, keyed as the proxy sees it.
fn networked_pair(url: &str) -> Option<(String, u16)> {
    let parsed = url::Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https" | "ws" | "wss") {
        return None;
    }
    let host = normalize_host(parsed.host_str()?);
    Some((host, parsed.port_or_known_default()?))
}

/// The URL a CONNECT `host:port` is checked as; CONNECT has no path.
fn connect_url(host: &str, port: u16) -> String {
    let host = normalize_host(host);
    if host.contains(':') {
        format!("https://[{host}]:{port}/")
    } else {
        format!("https://{host}:{port}/")
    }
}

impl Guard {
    pub fn emit(&self, e: BrowserEvent) {
        if let Some(s) = &self.events {
            s(&e);
        }
    }

    pub fn emit_kind(
        &self,
        kind: LifecycleKind,
        reason: impl Into<String>,
        f: impl FnOnce(&mut BrowserEvent),
    ) {
        let mut e = BrowserEvent::new(kind, &self.session_id, reason);
        f(&mut e);
        self.emit(e);
    }

    pub fn denied_network(&self, page_id: Option<&str>, what: &str, reason: &BlockReason) {
        self.emit_kind(LifecycleKind::Denied, reason.to_string(), |e| {
            e.page_id = page_id.map(str::to_string);
            e.what = Some(what.to_string());
            e.blocked = Some(reason.clone());
        });
    }

    pub async fn admit(&self, req: AdmissionRequest) -> crate::error::Result<()> {
        let what = format!("{:?}", req.action);
        let page_id = req.page_id.clone();
        if let Some(hook) = &self.admission {
            if let Decision::Deny(reason) = hook(req).await {
                self.emit_kind(LifecycleKind::Denied, reason.clone(), |e| {
                    e.page_id = Some(page_id);
                    e.what = Some(what);
                });
                return Err(crate::error::BrowserError::Denied(reason));
            }
        }
        Ok(())
    }

    /// [`NetworkPolicy::check`] for a request page interception saw. An allowed networked
    /// request records its `(host, port)` for this session.
    pub async fn check_request(&self, url: &str, kind: &str) -> Result<(), BlockReason> {
        self.network.check(url, kind).await?;
        if let Some(pair) = networked_pair(url) {
            self.page_allowed.lock().unwrap().insert(pair);
        }
        Ok(())
    }

    /// The proxy's decision for one connection: CONNECT `host:port`, or a plain-HTTP `url`.
    /// With a per-request callback set, a pair no page check of this session allowed gets the
    /// same check page requests get, as resource type `Connect`; then the address is pinned.
    /// Without a callback the decision is the pin alone, as before.
    pub(crate) async fn admit_proxied(
        &self,
        host: &str,
        port: u16,
        url: Option<&str>,
    ) -> Result<Vec<IpAddr>, BlockReason> {
        let decide = self.network.allow_request.is_some()
            && !self
                .page_allowed
                .lock()
                .unwrap()
                .contains(&(normalize_host(host), port));
        if decide {
            let connect;
            let url = match url {
                Some(url) => url,
                None => {
                    connect = connect_url(host, port);
                    &connect
                }
            };
            self.network.check(url, "Connect").await?;
        }
        self.network.pin(host).await
    }

    pub async fn check_url(
        &self,
        page_id: Option<&str>,
        url: &str,
        kind: &str,
    ) -> crate::error::Result<()> {
        match self.check_request(url, kind).await {
            Ok(()) => Ok(()),
            Err(reason) => {
                self.denied_network(page_id, kind, &reason);
                Err(crate::error::BrowserError::Blocked(PolicyDenial {
                    url: url.to_string(),
                    resource_type: kind.to_string(),
                    reason,
                }))
            }
        }
    }
}
