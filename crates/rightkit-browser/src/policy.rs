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
//! Known gaps (documented, not silently covered):
//! * WebSocket handshakes and requests from dedicated/shared/service workers
//!   are not visible to page-level interception. They still pass the pinning
//!   proxy, so the address policy holds, but the scheme check and the
//!   per-request callback are not consulted for them.
//! * The callback can only *deny* public destinations for requests the page
//!   interception sees (same limit as above).
//! * WebRTC UDP is restricted to proxied routes by Chrome flag; this is not
//!   independently verified by a test.

use futures::future::BoxFuture;
use std::collections::HashMap;
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
    Unresolved { host: String },
    /// The host is, or resolves to, a blocked address.
    Address {
        host: String,
        ip: IpAddr,
        class: AddressClass,
    },
    /// A cloud metadata hostname.
    MetadataHost { host: String },
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
        self.allowed_schemes.push(scheme.into().to_ascii_lowercase());
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
        let mut default_block = (!self.scheme_allowed(&scheme)).then(|| BlockReason::Scheme(scheme.clone()));
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
            if !self.address_allowed(&host, c) || (c == AddressClass::LinkLocal && self.hard_block_link_local) {
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
}

impl Guard {
    pub fn emit(&self, e: BrowserEvent) {
        if let Some(s) = &self.events {
            s(&e);
        }
    }

    pub fn emit_kind(&self, kind: LifecycleKind, reason: impl Into<String>, f: impl FnOnce(&mut BrowserEvent)) {
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

    pub async fn check_url(&self, page_id: Option<&str>, url: &str, kind: &str) -> crate::error::Result<()> {
        match self.network.check(url, kind).await {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    /// Offline policy: `public.example` is public, `rebind.example` resolves to
    /// one public and one private address, `meta.example` to the metadata IP.
    fn offline() -> NetworkPolicy {
        NetworkPolicy::default().with_resolver(|h| match h {
            "public.example" => vec![ip("93.184.216.34")],
            "rebind.example" => vec![ip("93.184.216.34"), ip("10.0.0.7")],
            "meta.example" => vec![ip("169.254.169.254")],
            "mapped.example" => vec![ip("::ffff:192.168.1.1")],
            "dev.example" => vec![ip("127.0.0.1")],
            _ => vec![],
        })
    }

    fn check(p: &NetworkPolicy, url: &str) -> Result<(), BlockReason> {
        block_on(p.check(url, "Navigation"))
    }

    #[test]
    fn classifies_every_blocked_v4_range() {
        let cases = [
            ("127.0.0.1", AddressClass::Loopback),
            ("127.255.255.254", AddressClass::Loopback),
            ("169.254.169.254", AddressClass::LinkLocal),
            ("169.254.0.1", AddressClass::LinkLocal),
            ("10.0.0.1", AddressClass::Private),
            ("10.255.255.255", AddressClass::Private),
            ("172.16.0.1", AddressClass::Private),
            ("172.31.255.255", AddressClass::Private),
            ("192.168.0.1", AddressClass::Private),
            ("100.64.0.1", AddressClass::Private),
            ("100.127.255.255", AddressClass::Private),
            ("0.0.0.0", AddressClass::Unspecified),
            ("0.1.2.3", AddressClass::Unspecified),
            ("224.0.0.1", AddressClass::Multicast),
            ("255.255.255.255", AddressClass::Reserved),
            ("240.0.0.1", AddressClass::Reserved),
            ("192.0.2.1", AddressClass::Reserved),
            ("198.18.0.1", AddressClass::Reserved),
            ("198.51.100.1", AddressClass::Reserved),
            ("203.0.113.1", AddressClass::Reserved),
        ];
        for (s, want) in cases {
            assert_eq!(classify_ip(ip(s)), want, "{s}");
        }
        for s in ["8.8.8.8", "93.184.216.34", "172.32.0.1", "172.15.255.255", "100.63.255.255", "100.128.0.1", "169.253.1.1"] {
            assert_eq!(classify_ip(ip(s)), AddressClass::Public, "{s}");
        }
    }

    #[test]
    fn classifies_every_blocked_v6_range_and_embedded_v4() {
        let cases = [
            ("::1", AddressClass::Loopback),
            ("::", AddressClass::Unspecified),
            ("fe80::1", AddressClass::LinkLocal),
            ("febf::1", AddressClass::LinkLocal),
            ("fc00::1", AddressClass::Private),
            ("fd00:ec2::254", AddressClass::Private),
            ("fec0::1", AddressClass::Private),
            ("ff02::1", AddressClass::Multicast),
            ("2001:db8::1", AddressClass::Reserved),
            // IPv4-mapped.
            ("::ffff:127.0.0.1", AddressClass::Loopback),
            ("::ffff:169.254.169.254", AddressClass::LinkLocal),
            ("::ffff:10.1.2.3", AddressClass::Private),
            ("::ffff:192.168.1.1", AddressClass::Private),
            ("::ffff:0.0.0.0", AddressClass::Unspecified),
            // IPv4-compatible, NAT64, 6to4.
            ("::127.0.0.1", AddressClass::Loopback),
            ("64:ff9b::a9fe:a9fe", AddressClass::LinkLocal),
            ("64:ff9b::7f00:1", AddressClass::Loopback),
            ("2002:c0a8:0101::1", AddressClass::Private),
            ("2002:7f00:0001::1", AddressClass::Loopback),
        ];
        for (s, want) in cases {
            assert_eq!(classify_ip(ip(s)), want, "{s}");
        }
        for s in ["2606:4700:4700::1111", "::ffff:8.8.8.8", "64:ff9b::808:808", "2002:0808:0808::1"] {
            assert_eq!(classify_ip(ip(s)), AddressClass::Public, "{s}");
        }
    }

    #[test]
    fn default_policy_blocks_literals_localhost_and_metadata() {
        let p = offline();
        for url in [
            "http://127.0.0.1/",
            "http://127.1.2.3:8080/x",
            "http://localhost/",
            "http://LOCALHOST./",
            "http://app.localhost/",
            "http://[::1]/",
            "http://169.254.169.254/latest/meta-data/",
            "http://[fe80::1]/",
            "http://10.0.0.5/",
            "http://172.20.1.1/",
            "http://192.168.1.1/",
            "http://100.64.1.1/",
            "http://[fc00::1]/",
            "http://0.0.0.0/",
            "http://[::]/",
            "http://[::ffff:127.0.0.1]/",
            "http://[::ffff:169.254.169.254]/",
            "http://[::ffff:10.0.0.1]/",
            "http://2130706433/",   // decimal 127.0.0.1
            "http://0x7f.0.0.1/",   // hex octet
            "http://metadata.google.internal/computeMetadata/v1/",
        ] {
            let r = check(&p, url);
            assert!(r.is_err(), "{url} was allowed");
        }
        assert_eq!(check(&p, "https://public.example/"), Ok(()));
        assert_eq!(check(&p, "http://8.8.8.8/"), Ok(()));
    }

    #[test]
    fn every_resolved_address_is_checked_and_mapped_v6_resolution_blocks() {
        let p = offline();
        match check(&p, "https://rebind.example/") {
            Err(BlockReason::Address { ip: a, class, .. }) => {
                assert_eq!(a, ip("10.0.0.7"));
                assert_eq!(class, AddressClass::Private);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            check(&p, "https://mapped.example/"),
            Err(BlockReason::Address { class: AddressClass::Private, .. })
        ));
        assert!(matches!(
            check(&p, "https://meta.example/"),
            Err(BlockReason::Address { class: AddressClass::LinkLocal, .. })
        ));
        // No answer fails closed.
        assert_eq!(
            check(&p, "https://nowhere.example/"),
            Err(BlockReason::Unresolved { host: "nowhere.example".into() })
        );
    }

    #[test]
    fn schemes_other_than_http_are_blocked_unless_allowed() {
        let p = offline();
        for url in [
            "file:///etc/passwd",
            "chrome://settings",
            "ftp://public.example/",
            "ws://public.example/",
            "wss://public.example/",
            "javascript:alert(1)",
            "about:config",
        ] {
            assert!(
                matches!(check(&p, url), Err(BlockReason::Scheme(_))),
                "{url}: {:?}",
                check(&p, url)
            );
        }
        // Inert: about:blank, data:/blob: subresources. data: documents are blocked.
        assert_eq!(check(&p, "about:blank"), Ok(()));
        assert_eq!(block_on(p.check("data:image/png;base64,AA==", "Image")), Ok(()));
        assert!(matches!(
            block_on(p.check("data:text/html,<h1>x</h1>", "Document")),
            Err(BlockReason::Scheme(_))
        ));
        let p = offline().allow_scheme("file").allow_scheme("wss");
        assert_eq!(check(&p, "file:///tmp/x.html"), Ok(()));
        assert_eq!(check(&p, "wss://public.example/socket"), Ok(()));
        // Allowing the scheme does not relax the address check.
        assert!(check(&p, "wss://127.0.0.1/").is_err());
    }

    #[test]
    fn allow_callback_overrides_both_ways_but_not_hard_blocks() {
        let seen: Arc<Mutex<Vec<BrowserRequest>>> = Arc::default();
        let s2 = seen.clone();
        let p = offline().allow_with(move |r: &BrowserRequest| {
            s2.lock().unwrap().push(r.clone());
            match r.host.as_str() {
                "127.0.0.1" if r.port == Some(3000) => PolicyDecision::Allow,
                "public.example" => PolicyDecision::Deny("not on the list".into()),
                "169.254.169.254" | "meta.example" => PolicyDecision::Allow,
                _ => PolicyDecision::Default,
            }
        });
        assert_eq!(check(&p, "http://127.0.0.1:3000/"), Ok(()));
        assert!(check(&p, "http://127.0.0.1:3001/").is_err());
        assert_eq!(
            check(&p, "https://public.example/"),
            Err(BlockReason::Callback("not on the list".into()))
        );
        // Hard blocks win over the callback, which is not even consulted.
        let before = seen.lock().unwrap().len();
        assert!(check(&p, "http://169.254.169.254/").is_err());
        assert!(check(&p, "https://meta.example/").is_err());
        assert_eq!(seen.lock().unwrap().len(), before);
        // The callback sees what the defaults would do.
        let s = seen.lock().unwrap();
        let r = s.iter().find(|r| r.port == Some(3000)).unwrap();
        assert_eq!(r.class, Some(AddressClass::Loopback));
        assert!(matches!(r.default_block, Some(BlockReason::Address { .. })));
        let r = s.iter().find(|r| r.host == "public.example").unwrap();
        assert_eq!(r.default_block, None);
        drop(s);

        // bool callbacks keep working: false defers to the defaults.
        let p = offline().allow_with(|r: &BrowserRequest| r.host == "10.0.0.5");
        assert_eq!(check(&p, "http://10.0.0.5/"), Ok(()));
        assert!(check(&p, "http://10.0.0.6/").is_err());
        assert_eq!(check(&p, "https://public.example/"), Ok(()));

        // Opting out of the hard block makes link-local overridable.
        let p = offline().hard_block_link_local(false).allow_host("169.254.169.254");
        assert_eq!(check(&p, "http://169.254.169.254/"), Ok(()));
    }

    #[test]
    fn class_and_host_allowances() {
        let p = offline().allow_class(AddressClass::Loopback);
        assert_eq!(check(&p, "http://localhost:5173/"), Ok(()));
        assert_eq!(check(&p, "https://dev.example/"), Ok(()));
        assert!(check(&p, "http://10.0.0.1/").is_err());
        assert!(check(&p, "http://169.254.169.254/").is_err());
        let p = offline().allow_host("Rebind.Example.");
        assert_eq!(check(&p, "https://rebind.example/"), Ok(()));
        // Even listed, a link-local destination stays blocked.
        let p = offline().allow_host("meta.example").allow_class(AddressClass::LinkLocal);
        assert!(check(&p, "https://meta.example/").is_err());
    }

    #[test]
    fn pin_only_connects_to_vetted_addresses() {
        let p = offline();
        // Public host pins.
        assert_eq!(block_on(p.pin("public.example")).unwrap(), vec![ip("93.184.216.34")]);
        // Private resolution that the request check never approved is refused at connect time.
        assert!(block_on(p.pin("rebind.example")).is_err());
        assert!(block_on(p.pin("dev.example")).is_err());
        assert!(block_on(p.pin("metadata.google.internal")).is_err());
        assert!(block_on(p.pin("nowhere.example")).is_err());
        // After an explicit per-request allow, exactly those addresses pin.
        let p = offline().allow_with(|r: &BrowserRequest| r.host == "dev.example");
        assert_eq!(check(&p, "https://dev.example/"), Ok(()));
        assert_eq!(block_on(p.pin("dev.example")).unwrap(), vec![ip("127.0.0.1")]);
    }

    #[test]
    fn rebinding_after_check_is_refused_at_connect() {
        // First answer public, every later answer private: the check passes,
        // the cache is bypassed by expiry, and pin must still refuse.
        let n = Arc::new(Mutex::new(0u32));
        let n2 = n.clone();
        let p = NetworkPolicy::default().with_resolver(move |_| {
            let mut g = n2.lock().unwrap();
            *g += 1;
            if *g == 1 {
                vec![ip("93.184.216.34")]
            } else {
                vec![ip("10.9.9.9")]
            }
        });
        assert_eq!(check(&p, "https://flip.example/"), Ok(()));
        p.resolved.lock().unwrap().clear(); // simulate TTL expiry
        assert!(matches!(
            block_on(p.pin("flip.example")),
            Err(BlockReason::Address { class: AddressClass::Private, .. })
        ));
    }

    #[test]
    fn unrestricted_allows_everything() {
        let p = NetworkPolicy::unrestricted();
        assert_eq!(check(&p, "file:///etc/hosts"), Ok(()));
        assert_eq!(check(&p, "http://169.254.169.254/"), Ok(()));
    }

    #[test]
    fn denials_emit_typed_events_with_session_reason_and_timestamp() {
        let events: Arc<Mutex<Vec<BrowserEvent>>> = Arc::default();
        let e2 = events.clone();
        let g = Guard {
            session_id: "bs-test".into(),
            admission: Some(Arc::new(|_req| Box::pin(async { Decision::Deny("needs approval".into()) }))),
            network: offline(),
            events: Some(Arc::new(move |e: &BrowserEvent| e2.lock().unwrap().push(e.clone()))),
        };
        let before = SystemTime::now();
        let err = block_on(g.check_url(Some("p1"), "http://10.0.0.1/", "Navigation")).unwrap_err();
        match err {
            crate::BrowserError::Blocked(d) => {
                assert_eq!(d.resource_type, "Navigation");
                assert!(matches!(d.reason, BlockReason::Address { class: AddressClass::Private, .. }));
            }
            other => panic!("{other:?}"),
        }
        let err = block_on(g.admit(AdmissionRequest {
            session_id: "bs-test".into(),
            page_id: "p1".into(),
            action: ActionKind::Click,
            url: None,
            target: None,
            detail: None,
        }))
        .unwrap_err();
        assert!(matches!(err, crate::BrowserError::Denied(ref r) if r == "needs approval"));
        let ev = events.lock().unwrap();
        assert_eq!(ev.len(), 2);
        for e in ev.iter() {
            assert_eq!(e.kind, LifecycleKind::Denied);
            assert_eq!(e.session_id, "bs-test");
            assert_eq!(e.page_id.as_deref(), Some("p1"));
            assert!(!e.reason.is_empty());
            assert!(e.at >= before);
            assert!(e.timestamp_ms() > 0);
            let j = e.to_json();
            assert_eq!(j["event"], "denied");
            assert_eq!(j["session_id"], "bs-test");
        }
        assert!(ev[0].blocked.is_some());
        assert_eq!(ev[0].what.as_deref(), Some("Navigation"));
        assert_eq!(ev[1].what.as_deref(), Some("Click"));
        assert!(ev[1].blocked.is_none());
    }
}
