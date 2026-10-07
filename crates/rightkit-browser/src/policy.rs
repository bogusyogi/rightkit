//! Effect admission and network (SSRF) policy.
//!
//! Every navigation, input, and script action passes a caller-supplied
//! [`AdmissionHook`] as a typed [`AdmissionRequest`] before any side effect
//! (validate, approve, execute, settle). Every network request the page makes,
//! including redirect hops and subresources, is checked against a
//! [`NetworkPolicy`] inside the browser, before it leaves the machine.
//!
//! Default policy blocks loopback, link-local, private, unspecified, multicast
//! addresses, `localhost`, and every scheme that reaches local state
//! (`file:`, `chrome:`, `ftp:` ...). A caller opens specific targets with
//! `allow_hosts` or a per-request allow callback.
//!
//! Known limit: hostnames are vetted by a resolver lookup at request time and
//! Chrome may resolve again when it connects (DNS rebinding window); dedicated
//! workers are not intercepted. Pop-up windows are blocked outright so no
//! unmanaged page can bypass interception.

use futures::future::BoxFuture;
use std::collections::HashMap;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// Why an address class is blocked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddressClass {
    Public,
    Loopback,
    LinkLocal,
    Private,
    Unspecified,
    Multicast,
}

/// One network request about to leave the browser.
#[derive(Clone, Debug)]
pub struct NetworkRequest {
    pub url: String,
    pub scheme: String,
    pub host: String,
    pub port: Option<u16>,
    pub class: AddressClass,
    /// `Document`, `Image`, `Script`, `Fetch`, ... or `Navigation`.
    pub resource_type: String,
}

pub type AllowCallback = Arc<dyn Fn(&NetworkRequest) -> bool + Send + Sync>;

/// Per-host resolution shared by the request check and the pinning proxy.
type ResolvedCache = Arc<Mutex<HashMap<String, (Instant, Vec<IpAddr>)>>>;

#[derive(Clone, Default)]
pub struct NetworkPolicy {
    /// Hosts (exact, case-insensitive) allowed even when their address class is blocked.
    pub allow_hosts: Vec<String>,
    /// Called only for requests the defaults would block; `true` allows.
    pub allow_request: Option<AllowCallback>,
    /// Disable every check (trusted callers only).
    pub unrestricted: bool,
    /// Replaces the system resolver (offline tests, pinned DNS, split-horizon setups).
    pub resolver: Option<Resolver>,
    /// One resolution per host is shared by the request check and the pinning
    /// proxy, so the address that was vetted is the address that is connected.
    resolved: ResolvedCache,
    /// Non-public addresses an explicit allow let through, per host.
    approved: Arc<Mutex<HashMap<String, Vec<IpAddr>>>>,
}

pub type Resolver = Arc<dyn Fn(&str) -> Vec<IpAddr> + Send + Sync>;

impl fmt::Debug for NetworkPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NetworkPolicy")
            .field("allow_hosts", &self.allow_hosts)
            .field("allow_request", &self.allow_request.is_some())
            .field("unrestricted", &self.unrestricted)
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockedRequest {
    pub url: String,
    pub reason: String,
}

pub fn classify_ip(ip: IpAddr) -> AddressClass {
    match ip {
        IpAddr::V4(v4) => classify_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return classify_v4(v4);
            }
            let seg = v6.segments();
            if v6.is_loopback() {
                AddressClass::Loopback
            } else if v6.is_unspecified() {
                AddressClass::Unspecified
            } else if v6.is_multicast() {
                AddressClass::Multicast
            } else if seg[0] & 0xffc0 == 0xfe80 {
                AddressClass::LinkLocal
            } else if seg[0] & 0xfe00 == 0xfc00 || is_nat64_private(v6) {
                AddressClass::Private
            } else {
                AddressClass::Public
            }
        }
    }
}

/// 64:ff9b::/96 embeds an IPv4 address; classify the embedded one.
fn is_nat64_private(v6: Ipv6Addr) -> bool {
    let s = v6.segments();
    if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        let v4 = Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
        return classify_v4(v4) != AddressClass::Public;
    }
    false
}

fn classify_v4(ip: Ipv4Addr) -> AddressClass {
    let o = ip.octets();
    if ip.is_loopback() {
        AddressClass::Loopback
    } else if ip.is_unspecified() || o[0] == 0 {
        AddressClass::Unspecified
    } else if ip.is_link_local() {
        AddressClass::LinkLocal
    } else if ip.is_private() || (o[0] == 100 && (64..128).contains(&o[1])) {
        AddressClass::Private
    } else if ip.is_multicast() || ip.is_broadcast() || o[0] >= 240 {
        AddressClass::Multicast
    } else {
        AddressClass::Public
    }
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
        self.allow_hosts.push(host.into().to_ascii_lowercase());
        self
    }

    pub fn with_resolver(
        mut self,
        f: impl Fn(&str) -> Vec<IpAddr> + Send + Sync + 'static,
    ) -> Self {
        self.resolver = Some(Arc::new(f));
        self
    }

    pub fn allow_with(
        mut self,
        f: impl Fn(&NetworkRequest) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.allow_request = Some(Arc::new(f));
        self
    }

    /// `Ok(())` when the request may proceed, otherwise the reason it is blocked.
    pub async fn check(&self, url: &str, resource_type: &str) -> std::result::Result<(), String> {
        if self.unrestricted {
            return Ok(());
        }
        let parsed = url::Url::parse(url).map_err(|e| format!("unparseable url: {e}"))?;
        let scheme = parsed.scheme().to_ascii_lowercase();
        // Schemes that never touch the network or other local state.
        if matches!(scheme.as_str(), "about" | "data" | "blob") {
            if scheme == "about" && parsed.path() != "blank" {
                return self.decide(
                    url,
                    &scheme,
                    "",
                    None,
                    AddressClass::Loopback,
                    resource_type,
                    "about: page",
                );
            }
            return Ok(());
        }
        if !matches!(scheme.as_str(), "http" | "https" | "ws" | "wss") {
            let reason = format!("scheme '{scheme}' is blocked");
            return self.decide(
                url,
                &scheme,
                "",
                None,
                AddressClass::Loopback,
                resource_type,
                &reason,
            );
        }
        let host = parsed
            .host_str()
            .unwrap_or("")
            .trim_matches(['[', ']'])
            .to_ascii_lowercase();
        if host.is_empty() {
            return Err("request has no host".into());
        }
        let (class, ips) = self.host_class(&host).await;
        if class == AddressClass::Public {
            return Ok(());
        }
        let reason = format!("host '{host}' is {class:?}");
        self.decide(
            url,
            &scheme,
            &host,
            parsed.port_or_known_default(),
            class,
            resource_type,
            &reason,
        )?;
        self.approved.lock().unwrap().insert(host, ips);
        Ok(())
    }

    /// Addresses the proxy may connect to for `host`: the same cached
    /// resolution the request check used. A non-public address passes only if
    /// the request check explicitly allowed it, so anything that bypassed
    /// interception (workers, rebinding) cannot reach private ranges.
    pub(crate) async fn pin(&self, host: &str) -> std::result::Result<Vec<IpAddr>, String> {
        let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
        let (_, ips) = self.host_class(&host).await;
        if ips.is_empty() {
            return Err(format!("host '{host}' did not resolve"));
        }
        let approved = self
            .approved
            .lock()
            .unwrap()
            .get(&host)
            .cloned()
            .unwrap_or_default();
        let listed = self.allow_hosts.contains(&host);
        let ok: Vec<IpAddr> = ips
            .iter()
            .copied()
            .filter(|ip| {
                classify_ip(*ip) == AddressClass::Public || listed || approved.contains(ip)
            })
            .collect();
        if ok.len() == ips.len() {
            Ok(ok)
        } else {
            Err(format!("host '{host}' resolves to a blocked address"))
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn decide(
        &self,
        url: &str,
        scheme: &str,
        host: &str,
        port: Option<u16>,
        class: AddressClass,
        resource_type: &str,
        reason: &str,
    ) -> std::result::Result<(), String> {
        if !host.is_empty() && self.allow_hosts.iter().any(|h| h == host) {
            return Ok(());
        }
        if let Some(cb) = &self.allow_request {
            let req = NetworkRequest {
                url: url.to_string(),
                scheme: scheme.to_string(),
                host: host.to_string(),
                port,
                class,
                resource_type: resource_type.to_string(),
            };
            if cb(&req) {
                return Ok(());
            }
        }
        Err(reason.to_string())
    }

    /// Worst class across the host's addresses (any blocked address blocks the host).
    async fn host_class(&self, host: &str) -> (AddressClass, Vec<IpAddr>) {
        if let Ok(ip) = host.parse::<IpAddr>() {
            return (classify_ip(ip), vec![ip]);
        }
        if host == "localhost" || host.ends_with(".localhost") {
            return (
                AddressClass::Loopback,
                vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            );
        }
        let addrs = self.resolve(host).await;
        let class = addrs
            .iter()
            .copied()
            .map(classify_ip)
            .find(|c| *c != AddressClass::Public)
            .unwrap_or(AddressClass::Public);
        (class, addrs)
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
                _ => Vec::new(), // unresolved names cannot connect; Chrome reports the failure.
            }
        };
        self.resolved
            .lock()
            .unwrap()
            .insert(host.to_string(), (Instant::now(), found.clone()));
        found
    }
}

/// Lifecycle notifications for the caller's journal. Content-free.
#[derive(Clone, Debug)]
pub enum BrowserEvent {
    Started {
        session_id: String,
        pid: Option<u32>,
    },
    Stopped {
        session_id: String,
    },
    /// Admission or network policy refused something before any side effect.
    Denied {
        session_id: String,
        what: String,
        reason: String,
    },
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

    pub async fn admit(&self, req: AdmissionRequest) -> crate::error::Result<()> {
        let what = format!("{:?}", req.action);
        if let Some(hook) = &self.admission {
            if let Decision::Deny(reason) = hook(req).await {
                self.emit(BrowserEvent::Denied {
                    session_id: self.session_id.clone(),
                    what,
                    reason: reason.clone(),
                });
                return Err(crate::error::BrowserError::Denied(reason));
            }
        }
        Ok(())
    }

    pub async fn check_url(&self, url: &str, kind: &str) -> crate::error::Result<()> {
        match self.network.check(url, kind).await {
            Ok(()) => Ok(()),
            Err(reason) => {
                self.emit(BrowserEvent::Denied {
                    session_id: self.session_id.clone(),
                    what: kind.into(),
                    reason: reason.clone(),
                });
                Err(crate::error::BrowserError::Denied(format!(
                    "{url}: {reason}"
                )))
            }
        }
    }
}
