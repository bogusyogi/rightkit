//! The mesh endpoint: one iroh `Endpoint` built with Orthic policy, the trust gate, and the
//! protocol handlers.
//!
//! Policy (Adrian, 2026-10-16 and 2026-10-17): iroh with our OWN relay, no default public relay,
//! no n0 DNS or pkarr, no Mainline DHT. [`Preset::N0`](iroh::endpoint::presets) is never used:
//! the endpoint starts from `presets::Minimal` (crypto provider only), takes its relay from
//! `MeshSettings::relay_url` (none means no relay at all), and registers exactly one address
//! lookup: local-network mDNS (`iroh-mdns-address-lookup`, `MeshSettings::local_discovery`), so no
//! internet lookup service ever hears an endpoint id. Peers are dialled by id (plus our relay URL
//! when set); iroh then tries to hole-punch a direct path inside the same connection and migrates
//! to it ("direct first, relay as fallback"). With no relay, only mDNS or a known direct address
//! finds a peer.
//!
//! Every connection, incoming or outgoing, passes `TrustGate`: the remote key must be an active
//! paired device (or the configured mailbox server), else the connection is closed before any
//! handler runs. The registry is read on every handshake, so a revocation takes effect at the next
//! connection.

use std::collections::HashMap;
use std::fmt;
use std::io::{self, Read, Write};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use iroh::endpoint::{
    AfterHandshakeOutcome, BeforeConnectOutcome, Connection, EndpointHooks, IdleTimeout,
    LocalTransportAddr, QuicTransportConfig, WeakConnectionHandle, presets,
};
use iroh::protocol::{AcceptError, ProtocolHandler, Router, RouterBuilder};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, RelayUrl, TransportAddr};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::caps::{PathKind, RELAY_CIRCLE_MONTH, RELAY_TRANSFER_MAX, RelayBudget};
use crate::error::MeshError;
use crate::hello::{
    ALPN_HELLO, CAP_IDLE_CLOSE, HelloBook, HelloHandler, PeerHello, clamp_build, spawn_hello,
};
use crate::identity::{PeerTrust, secret_key_from_seed};
use crate::keys::KeyExchange;
use crate::mailbox::{Mailbox, MailboxClient};
use crate::seal::{KeyStore, PairKeys};
use crate::settings::MeshSettings;
use crate::sync::{PullResult, RecordStore, SyncHandler, pull_collection};
use crate::transfer::{BulkHandler, BulkReceiver};
use crate::util::now_ms;

pub const ALPN_KEYS: &[u8] = b"right/keys/1";

/// How a connection reaches the other device right now. A path that is not provably a direct IP
/// path counts as relayed, so every cap fails closed.
pub fn path_kind(connection: &Connection) -> PathKind {
    let paths = connection.paths();
    for path in paths.iter() {
        if path.is_selected() {
            return if path.is_ip() {
                PathKind::Direct
            } else {
                PathKind::Relayed
            };
        }
    }
    PathKind::Unknown
}

/// True when `remote` is on the same network as `local`, this device's own address of that same
/// path: a loopback, private or link-local pair in one /24 (IPv4) or one /64 (IPv6), or a global
/// IPv6 pair sharing the /64 interface prefix (a home LAN reached over its global prefix, Pulse
/// #469). No round-trip-time guess; a different prefix is a remote path.
fn same_network(remote: std::net::IpAddr, local: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match (remote.to_canonical(), local.to_canonical()) {
        (IpAddr::V4(r), IpAddr::V4(l)) => {
            (r.is_private() || r.is_link_local() || r.is_loopback())
                && r.octets()[..3] == l.octets()[..3]
        }
        (IpAddr::V6(r), IpAddr::V6(l)) => r.segments()[..4] == l.segments()[..4],
        _ => false,
    }
}

/// True when the selected path is a direct IP path whose remote address sits in the same network as
/// the local address of that same path (see `same_network`: private /24, or the same IPv6 /64
/// including a global prefix), so it is the same network and not a hole-punched or remote-direct path. Anything unknown
/// is false; this never guesses from Wi-Fi status. The path carries no discovery source, so the
/// subnet test is the evidence (mDNS-learned addresses are not told apart).
pub fn path_is_nearby(connection: &Connection) -> bool {
    let paths = connection.paths();
    for path in paths.iter() {
        if !path.is_selected() {
            continue;
        }
        let (TransportAddr::Ip(remote), LocalTransportAddr::Ip(Some(local))) =
            (path.remote_addr(), path.local_addr())
        else {
            return false;
        };
        return same_network(remote.ip(), *local);
    }
    false
}

/// Where a live connection runs, in the words the UI shows (Pulse #469): `Nearby` is a direct IP
/// path to a private, link-local or loopback address (the same network), `Direct` any other IP
/// path (hole-punched across networks), `Relay` our relay, `Unknown` before a path is selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathLabel {
    Nearby,
    Direct,
    Relay,
    Unknown,
}

impl PathLabel {
    pub fn as_str(self) -> &'static str {
        match self {
            PathLabel::Nearby => "nearby",
            PathLabel::Direct => "direct",
            PathLabel::Relay => "relay",
            PathLabel::Unknown => "unknown",
        }
    }
}

/// One observation of a connection's selected path, handed to the observer set with
/// [`set_path_observer`] when a connection opens and whenever its selected path changes.
#[derive(Clone, Debug)]
pub struct PathEvent {
    /// The remote endpoint id.
    pub peer: [u8; 32],
    pub label: PathLabel,
    /// The selected path's remote address (`ip:port`), `relay` or empty while unknown.
    pub remote: String,
    pub rtt_ms: u64,
    /// `dial` (we opened it), `accept` (the peer opened it).
    pub side: &'static str,
    /// `opened` (first observation), `migrated` (the selected path changed), `closed`.
    pub change: &'static str,
    /// Milliseconds from the first observation, so a relay-to-direct upgrade shows its delay.
    pub age_ms: u64,
}

type PathObserver = Box<dyn Fn(&PathEvent) + Send + Sync>;

static PATH_OBSERVER: OnceLock<PathObserver> = OnceLock::new();

/// Register the process-wide path observer (the engine logs `mesh.path`). Set once; a second call
/// is ignored.
pub fn set_path_observer(observer: impl Fn(&PathEvent) + Send + Sync + 'static) {
    let _ = PATH_OBSERVER.set(Box::new(observer));
}

/// The last known path to one peer.
#[derive(Clone, Debug)]
pub struct PeerPath {
    pub label: PathLabel,
    pub remote: String,
    pub rtt_ms: u64,
    /// True while a connection with this path is open.
    pub live: bool,
    pub since_ms: u64,
    /// When a connection to this peer was last observed by its watcher (every poll while it is
    /// open) or closed (ms).
    pub seen_ms: u64,
    /// Which watcher wrote it (a newer connection owns the entry).
    generation: u64,
    /// Connections to this peer open right now, counted by their watchers (Pulse #560): `live`
    /// is false only when the LAST one closed, so a dual-dial loser closing cannot mark the
    /// winner's path dead.
    open: u32,
    /// When this peer last ran on a nearby path (ms); lengthens the wait for the LAN path.
    nearby_at: Option<u64>,
    /// When waiting for a direct path last failed (ms); skips the wait for a while.
    settle_failed_at: Option<u64>,
}

type PathBook = Arc<Mutex<HashMap<[u8; 32], PeerPath>>>;

static WATCH_GENERATION: AtomicU64 = AtomicU64::new(1);

/// What each live watcher last saw, by peer and generation (M3): when the watcher that owns a
/// peer's entry closes, ownership passes to the highest generation still live, with its last
/// observation, instead of leaving the entry on the closed connection's address. Locked only
/// inside the book's lock (book first, then this).
///
/// Scoped per book, so per [`Mesh`] (P2): the key carries the book's identity, and a watcher's
/// views are removed when it exits for any reason ([`WatcherGuard`]), so a dead watcher is never an
/// heir. Each view carries the time it was observed; a handover restores it instead of stamping
/// the entry fresh.
type WatcherView = (PathLabel, String, u64, u64);
type WatcherViews = HashMap<(usize, [u8; 32]), HashMap<u64, WatcherView>>;
static WATCHERS: Mutex<Option<WatcherViews>> = Mutex::new(None);

/// Removes a watcher's view when its task ends, however it ends (closed, aborted, runtime shut
/// down). Takes only the `WATCHERS` lock, never the book's.
struct WatcherGuard {
    key: (usize, [u8; 32]),
    generation: u64,
}

impl Drop for WatcherGuard {
    fn drop(&mut self) {
        let mut views = WATCHERS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(map) = views.as_mut()
            && let Some(mine) = map.get_mut(&self.key)
        {
            mine.remove(&self.generation);
            if mine.is_empty() {
                map.remove(&self.key);
            }
        }
    }
}

fn is_nearby_ip(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local() || v4.is_loopback(),
        IpAddr::V6(v6) => {
            let seg = v6.segments();
            v6.is_loopback() || seg[0] & 0xfe00 == 0xfc00 || seg[0] & 0xffc0 == 0xfe80
        }
    }
}

/// The selected path of `connection`: label, remote text and round-trip time. `None` while iroh
/// has no path selected (a transient gap while it switches paths): that is not an observation.
fn describe_path(connection: &Connection) -> Option<(PathLabel, String, u64)> {
    let paths = connection.paths();
    for path in paths.iter() {
        if !path.is_selected() {
            continue;
        }
        let rtt = u64::try_from(path.rtt().as_millis()).unwrap_or(u64::MAX);
        return Some(match path.remote_addr() {
            TransportAddr::Ip(addr) => {
                let own_network = matches!(
                    path.local_addr(),
                    LocalTransportAddr::Ip(Some(local)) if same_network(addr.ip(), *local)
                );
                let label = if is_nearby_ip(addr.ip().to_canonical()) || own_network {
                    PathLabel::Nearby
                } else {
                    PathLabel::Direct
                };
                (label, addr.to_string(), rtt)
            }
            _ if path.is_relay() => (PathLabel::Relay, "relay".to_string(), rtt),
            _ => (PathLabel::Unknown, String::new(), rtt),
        });
    }
    None
}

/// Watch one connection's selected path on the mesh runtime: record it in `book`, tell the
/// observer on the first observation, on every change and at close. Polls (250 ms for the first 20
/// seconds, when a relay-to-direct upgrade happens, then every 5 s) and stops when the connection
/// closes.
///
/// The watcher holds only a weak handle (Pulse #560): a strong `Connection` here kept every
/// replaced connection alive for as long as the QUIC keep-alive ran (hours; a Mac log showed dial
/// closes at a median of 1.5 to 4.5 h), because a QUIC connection closes on its own (code 0) only
/// when its last handle drops. The poll upgrades the handle for the length of one look and drops
/// it before sleeping; a failed upgrade means every holder let go, which is a close.
fn watch_path(book: PathBook, connection: &Connection, side: &'static str) {
    let peer = *connection.remote_id().as_bytes();
    let weak = connection.weak_handle();
    let generation = WATCH_GENERATION.fetch_add(1, Ordering::Relaxed);
    let book_id = Arc::as_ptr(&book) as usize;
    tokio::spawn(async move {
        let _guard = WatcherGuard {
            key: (book_id, peer),
            generation,
        };
        let started = Instant::now();
        let mut last: Option<PathLabel> = None;
        // This watcher added one to the peer's open count (and owes the matching subtraction).
        let mut counted = false;
        loop {
            let (closed, described) = {
                let strong = weak.upgrade();
                match &strong {
                    Some(connection) => (
                        connection.close_reason().is_some(),
                        describe_path(connection),
                    ),
                    None => (true, None),
                }
            };
            // No selected path on a live connection keeps the previous label: no event, and the
            // first observation waits for a real path (or the close).
            if !closed && described.is_none() {
                let step = if started.elapsed() < Duration::from_secs(20) {
                    Duration::from_millis(250)
                } else {
                    Duration::from_secs(5)
                };
                tokio::time::sleep(step).await;
                continue;
            }
            // A close with no selected path reports the last known label.
            let (label, remote, rtt_ms) =
                described.unwrap_or((last.unwrap_or(PathLabel::Unknown), String::new(), 0));
            let age_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            // The book is refreshed on EVERY observation from this connection (N2): its remote
            // address and `seen_ms` follow a Nearby to Nearby move of the peer (a new IP), so the
            // address the twin binding proves is the live one, never one from before the move.
            // Only a label change or the close is reported to the observer.
            let reported = closed || last != Some(label);
            {
                let change = if closed {
                    "closed"
                } else if last.is_none() {
                    "opened"
                } else {
                    "migrated"
                };
                if let Ok(mut map) = book.lock() {
                    let entry = map.entry(peer).or_insert(PeerPath {
                        label,
                        remote: remote.clone(),
                        rtt_ms,
                        live: false,
                        since_ms: now_ms(),
                        seen_ms: now_ms(),
                        generation,
                        open: 0,
                        nearby_at: None,
                        settle_failed_at: None,
                    });
                    let mut views = WATCHERS.lock().unwrap_or_else(|e| e.into_inner());
                    let views = views.get_or_insert_with(HashMap::new);
                    if closed {
                        if counted {
                            entry.open = entry.open.saturating_sub(1);
                            counted = false;
                        }
                        let key = (book_id, peer);
                        let mine = views.get_mut(&key);
                        if let Some(mine) = mine {
                            mine.remove(&generation);
                        }
                        let heir = views
                            .get(&key)
                            .and_then(|m| m.iter().max_by_key(|(g, _)| **g))
                            .map(|(g, v)| (*g, v.clone()));
                        if views.get(&key).is_some_and(HashMap::is_empty) {
                            views.remove(&key);
                        }
                        // Only the last open connection's close ends the path.
                        if entry.open == 0 {
                            entry.live = false;
                            entry.seen_ms = now_ms();
                        } else if entry.generation <= generation
                            && let Some((heir_generation, (label, remote, rtt_ms, observed_ms))) =
                                heir
                        {
                            // The owning (newest) watcher closed while an older connection lives
                            // (M3): that one owns the entry now, with what it last saw and WHEN
                            // it saw it (P2: a stale view stays stale, so the twin binding's
                            // freshness gate still refuses it); its next poll refreshes it.
                            entry.label = label;
                            entry.remote = remote;
                            entry.rtt_ms = rtt_ms;
                            entry.live = true;
                            entry.seen_ms = observed_ms;
                            entry.generation = heir_generation;
                        }
                    } else {
                        views
                            .entry((book_id, peer))
                            .or_default()
                            .insert(generation, (label, remote.clone(), rtt_ms, now_ms()));
                        if !counted {
                            entry.open += 1;
                            counted = true;
                        }
                        if label == PathLabel::Nearby {
                            entry.nearby_at = Some(now_ms());
                        }
                        // The newest connection owns the entry; an older one never overwrites it.
                        if entry.generation <= generation {
                            entry.label = label;
                            entry.remote = remote.clone();
                            entry.rtt_ms = rtt_ms;
                            entry.live = true;
                            entry.seen_ms = now_ms();
                            entry.generation = generation;
                            if last != Some(label) {
                                entry.since_ms = now_ms();
                            }
                        }
                    }
                }
                if reported {
                    if let Some(observer) = PATH_OBSERVER.get() {
                        observer(&PathEvent {
                            peer,
                            label,
                            remote,
                            rtt_ms,
                            side,
                            change,
                            age_ms,
                        });
                    }
                    last = Some(label);
                }
            }
            if closed {
                break;
            }
            let step = if started.elapsed() < Duration::from_secs(20) {
                Duration::from_millis(250)
            } else {
                Duration::from_secs(5)
            };
            tokio::time::sleep(step).await;
        }
    });
}

/// How long a LAN-only dial runs alone before the relay dial starts beside it (Pulse #469): a
/// paired device on this network answers well inside it, so the relay is never used for it.
const LOCAL_HEAD_START: Duration = Duration::from_millis(300);
/// How long a connection that came up on the relay waits for iroh to move it to a direct path
/// when this peer was nearby in the last ten minutes, and when nothing is known about it.
const SETTLE_NEARBY: Duration = Duration::from_millis(2000);
const SETTLE_UNKNOWN: Duration = Duration::from_millis(500);
/// After a wait for a direct path failed, skip the wait for this long (a peer on another network
/// would otherwise pay it on every connection).
const SETTLE_SKIP_MS: u64 = 60_000;
const NEARBY_FRESH_MS: u64 = 600_000;

/// QUIC keep-alive and idle timeout on every connection (Pulse #500/#501): a peer that vanished
/// without a close is noticed within `QUIC_MAX_IDLE`, so a dead connection does not linger in the
/// reuse cache or the iroh remote map. The keep-alive is well inside the idle timeout.
const QUIC_KEEP_ALIVE: Duration = Duration::from_secs(5);
const QUIC_MAX_IDLE: Duration = Duration::from_secs(20);

/// Closes any connection whose remote key is not an active paired device.
#[derive(Clone)]
struct TrustGate {
    trust: Arc<dyn PeerTrust>,
    mailbox: Option<[u8; 32]>,
    /// Protocols that run before a device is paired (the host's own pairing protocol). Each
    /// handler of such a protocol must authenticate the person itself; the gate only lets the
    /// connection reach it.
    open: Vec<&'static [u8]>,
    /// The only protocols a guest (`PeerInfo::guest`, Pulse #492) may open.
    guest: Vec<&'static [u8]>,
    /// Every accepted connection that passed the gate and is still open, so [`Mesh::close_peer`]
    /// can end a revoked device's live connections at once (R09). Weak handles (Pulse #560): a
    /// strong list would be one more holder keeping a connection nobody serves alive.
    live: Arc<Mutex<Vec<WeakConnectionHandle>>>,
}

impl fmt::Debug for TrustGate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TrustGate")
    }
}

impl TrustGate {
    fn allows(&self, endpoint: &[u8; 32], alpn: &[u8]) -> bool {
        if self.open.contains(&alpn) || self.mailbox.as_ref() == Some(endpoint) {
            return true;
        }
        match self.trust.peer(endpoint) {
            Some(peer) => !peer.guest || self.guest.contains(&alpn),
            None => false,
        }
    }
}

impl EndpointHooks for TrustGate {
    async fn before_connect<'a>(
        &'a self,
        remote_addr: &'a EndpointAddr,
        alpn: &'a [u8],
    ) -> BeforeConnectOutcome {
        if self.allows(remote_addr.id.as_bytes(), alpn) {
            BeforeConnectOutcome::Accept
        } else {
            BeforeConnectOutcome::Reject
        }
    }

    async fn after_handshake<'a>(&'a self, conn: &'a Connection) -> AfterHandshakeOutcome {
        if self.allows(conn.remote_id().as_bytes(), conn.alpn()) {
            if let Ok(mut live) = self.live.lock() {
                live.retain(|w| w.upgrade().is_some_and(|c| c.close_reason().is_none()));
                live.push(conn.weak_handle());
            }
            AfterHandshakeOutcome::Accept
        } else {
            AfterHandshakeOutcome::Reject {
                error_code: 403u32.into(),
                reason: b"not a paired device".to_vec(),
            }
        }
    }
}

/// Everything [`Mesh::start`] needs from the host application.
pub struct MeshInit {
    /// The device-link identity seed (`KeyVault::load_or_create_seed`). It becomes the iroh secret
    /// key, so the endpoint id equals the device-link public key.
    pub seed: [u8; 32],
    pub settings: MeshSettings,
    pub trust: Arc<dyn PeerTrust>,
    pub keys: Arc<dyn KeyStore>,
    /// The relayed-bytes ledger file, beside the device registry.
    pub budget_path: std::path::PathBuf,
    /// The application's synced records; `None` serves and pulls no sync.
    pub records: Option<Arc<dyn RecordStore>>,
    /// The application's receiver for bulk transfers; `None` refuses to receive any.
    pub bulk: Option<Arc<dyn BulkReceiver>>,
    /// The host's own blocking stream protocols (ALPN plus service), registered only while
    /// `accept_connections` is on. For a host with no async runtime of its own (see [`SyncMesh`]).
    pub services: Vec<(&'static [u8], Arc<dyn StreamService>)>,
    /// ALPNs a not-yet-paired device may open (the host's pairing protocol). Empty for none.
    pub open_alpns: Vec<&'static [u8]>,
    /// The only ALPNs a guest (`PeerInfo::guest`, Pulse #492) may open or be dialled on; every
    /// other protocol refuses it both ways. Empty: a guest is reached on nothing.
    pub guest_alpns: Vec<&'static [u8]>,
}

/// A running mesh endpoint.
pub struct Mesh {
    router: Router,
    endpoint: Endpoint,
    relay: Option<RelayUrl>,
    settings: MeshSettings,
    trust: Arc<dyn PeerTrust>,
    budget: Arc<Mutex<RelayBudget>>,
    exchange: KeyExchange,
    records: Option<Arc<dyn RecordStore>>,
    mailbox: Option<Mailbox>,
    local: [u8; 32],
    open_alpns: Vec<&'static [u8]>,
    guest_alpns: Vec<&'static [u8]>,
    /// The ALPNs `StreamHandler` serves (the host's blocking stream services): the ones whose
    /// acceptor closes idle connections, so the only ones the long reuse window applies to.
    stream_alpns: Vec<&'static [u8]>,
    /// The mesh runtime, for background work started from a blocking caller.
    rt: tokio::runtime::Handle,
    /// Direct addresses learned out of band for a peer (a pairing QR), dialled alongside any
    /// relay: iOS cannot use mDNS without Apple's multicast entitlement, so a phone needs them.
    known: Mutex<std::collections::HashMap<[u8; 32], Vec<std::net::SocketAddr>>>,
    /// Keeps the mDNS announcement alive.
    _local_lookup: Option<iroh_mdns_address_lookup::MdnsAddressLookup>,
    /// The selected path of each peer's latest connection (Pulse #469).
    paths: PathBook,
    /// One live connection per (peer, alpn) for [`Mesh::connect_shared`] (Pulse #480).
    shared: Mutex<HashMap<SharedKey, Arc<SharedSlot>>>,
    /// The accepted connections of every peer, shared with the trust gate ([`Mesh::close_peer`]).
    live: Arc<Mutex<Vec<WeakConnectionHandle>>>,
    /// What each paired device told us about itself (Pulse #560, `hello.rs`).
    hello: Arc<HelloBook>,
}

/// See [`Mesh::memory_counts`].
#[derive(Clone, Debug, Default)]
pub struct MeshCounts {
    /// Peers in the path book (every peer that ever connected; one entry each).
    pub path_peers: usize,
    /// Connections open right now per peer with any open, largest first (no peer ids).
    pub open_per_peer: Vec<u32>,
    /// Cached dialled (peer, protocol) slots.
    pub shared_slots: usize,
    /// Accepted-connection handles held for the trust gate.
    pub accepted: usize,
    /// Peers with direct addresses learned out of band.
    pub known_peers: usize,
    /// Live path watchers across every book.
    pub watcher_views: usize,
}

type SharedKey = ([u8; 32], Vec<u8>);

/// A cached dialled connection and when a caller last took it (monotonic and wall clock: on macOS
/// `Instant` does not advance while the computer sleeps, so the wall clock is the second witness).
struct SharedConn {
    connection: Connection,
    used: Instant,
    used_wall: SystemTime,
}

/// One (peer, alpn) cache slot. `generation` moves whenever the slot is forgotten (network change,
/// revoke, a failed chat): a dial that finishes under another generation belongs to a slot nobody
/// looks up any more and is never cached or handed out (Pulse #560).
#[derive(Default)]
struct SharedSlot {
    conn: tokio::sync::Mutex<Option<SharedConn>>,
    generation: AtomicU64,
}

/// A cached connection idle longer than this is dropped and redialled: the accepting side lets an
/// idle connection go after [`STREAM_IDLE`], and a stream opened just as it does would be lost.
/// 20 s, below the 30 s after which an e501a3cd acceptor stops reading an idle connection WITHOUT
/// closing it (its keep-alive then holds it open, so `close_reason()` stays `None` and a request
/// written there is never read). This is the window for a peer that has not said it closes idle
/// connections (an older Pulse, or a hello not fetched yet).
const SHARED_REUSE_IDLE: Duration = Duration::from_secs(20);

/// The window for a peer whose hello lists `idle-close` (Pulse #560), on a protocol served by
/// [`StreamHandler`] ONLY (the host's blocking stream services, `Mesh::stream_alpns`): that
/// handler closes an idle connection after [`STREAM_IDLE`] (120 s), so a connection used in the
/// last 90 s is still open on its side. Sync, bulk and key connections have no such close and keep
/// [`SHARED_REUSE_IDLE`]. A one-minute caller (the chat peers poll) then reuses one connection
/// instead of dialling the other device every minute.
const SHARED_REUSE_IDLE_CAPABLE: Duration = Duration::from_secs(90);

impl fmt::Debug for Mesh {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Mesh")
    }
}

fn endpoint_id(hex_id: &str) -> Result<EndpointId, MeshError> {
    let bytes = crate::util::unhex::<32>(hex_id)?;
    EndpointId::from_bytes(&bytes).map_err(|e| MeshError::Address(e.to_string()))
}

impl Mesh {
    /// Bind the endpoint and start serving. Refused while `settings.enabled` is false.
    ///
    /// `extra` registers the host's own protocols (for example `pulse/link/1`) on the router
    /// beside the mesh's; it is called only when connections are accepted.
    pub async fn start(
        init: MeshInit,
        extra: impl FnOnce(RouterBuilder) -> RouterBuilder,
    ) -> Result<Self, MeshError> {
        let settings = init.settings;
        if !settings.enabled {
            return Err(MeshError::Disabled);
        }
        let relay = if settings.use_relay {
            let text = settings.relay_url.as_deref().ok_or(MeshError::NoRelay)?;
            Some(RelayUrl::from_str(text).map_err(|e| MeshError::Address(e.to_string()))?)
        } else {
            None
        };
        let relay_mode = match &relay {
            Some(url) => RelayMode::custom([url.clone()]),
            None => RelayMode::Disabled,
        };
        let secret = secret_key_from_seed(&init.seed);
        let local = *secret.public().as_bytes();
        let live: Arc<Mutex<Vec<WeakConnectionHandle>>> = Arc::new(Mutex::new(Vec::new()));
        let mailbox_id = match &settings.mailbox {
            Some(target) => Some(*endpoint_id(&target.endpoint_id)?.as_bytes()),
            None => None,
        };
        let idle = IdleTimeout::try_from(QUIC_MAX_IDLE)
            .map_err(|e| MeshError::Endpoint(format!("idle timeout: {e}")))?;
        let transport = QuicTransportConfig::builder()
            .keep_alive_interval(QUIC_KEEP_ALIVE)
            .max_idle_timeout(Some(idle))
            .build();
        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(secret)
            .relay_mode(relay_mode)
            .transport_config(transport)
            .hooks(TrustGate {
                trust: init.trust.clone(),
                mailbox: mailbox_id,
                open: init.open_alpns.clone(),
                guest: init.guest_alpns.clone(),
                live: live.clone(),
            })
            .bind()
            .await
            .map_err(|e| MeshError::Endpoint(e.to_string()))?;

        // Local discovery: two devices on one network find each other's direct addresses by mDNS,
        // so the link works with no relay and no internet. Best effort: a network that blocks
        // multicast leaves the relay (when set) or a known direct address.
        let mut local_lookup = None;
        if settings.local_discovery {
            local_lookup = iroh_mdns_address_lookup::MdnsAddressLookup::builder()
                .build(endpoint.id())
                .ok()
                .and_then(|mdns| {
                    endpoint.address_lookup().ok().map(|lookup| {
                        lookup.add(mdns.clone());
                        mdns
                    })
                });
        }

        let budget = Arc::new(Mutex::new(RelayBudget::open(
            init.budget_path,
            RELAY_CIRCLE_MONTH,
            now_ms(),
        )?));
        let exchange = KeyExchange::new(local, init.keys.clone(), init.trust.clone());

        let paths: PathBook = Arc::new(Mutex::new(HashMap::new()));
        let stream_alpns: Vec<&'static [u8]> = init.services.iter().map(|(a, _)| *a).collect();
        let mut builder = Router::builder(endpoint.clone());
        if settings.accept_connections {
            builder = builder.accept(ALPN_KEYS, exchange.clone());
            // The capability hello (Pulse #560): what this build can do, for the other side's
            // Send to read before it relies on a newer message.
            let mut caps = vec![CAP_IDLE_CLOSE.to_string()];
            for cap in &settings.hello_caps {
                if !caps.contains(cap) {
                    caps.push(cap.clone());
                }
            }
            builder = builder.accept(
                ALPN_HELLO,
                HelloHandler::new(&clamp_build(&settings.hello_build), &caps),
            );
            if let Some(records) = &init.records {
                builder = builder.accept(
                    crate::sync::ALPN_SYNC,
                    SyncHandler::new(init.trust.clone(), records.clone()),
                );
            }
            if let Some(receiver) = &init.bulk {
                builder = builder.accept(
                    crate::transfer::ALPN_BULK,
                    BulkHandler {
                        trust: init.trust.clone(),
                        receiver: receiver.clone(),
                        budget: budget.clone(),
                        settings: settings.clone(),
                    },
                );
            }
            for (alpn, service) in init.services {
                builder = builder.accept(
                    alpn,
                    StreamHandler {
                        service,
                        rt: tokio::runtime::Handle::current(),
                        paths: paths.clone(),
                        budget: budget.clone(),
                    },
                );
            }
            builder = extra(builder);
        }
        let router = builder.spawn();

        let mailbox = match (&settings.mailbox, &relay) {
            (Some(target), Some(url)) => {
                let addr = EndpointAddr::new(endpoint_id(&target.endpoint_id)?)
                    .with_relay_url(url.clone());
                Some(Mailbox::new(
                    MailboxClient::new(endpoint.clone(), addr),
                    local,
                    init.trust.clone(),
                    init.keys.clone(),
                ))
            }
            _ => None,
        };

        Ok(Self {
            router,
            endpoint,
            relay,
            settings,
            trust: init.trust,
            budget,
            exchange,
            records: init.records,
            mailbox,
            local,
            open_alpns: init.open_alpns,
            guest_alpns: init.guest_alpns,
            stream_alpns,
            rt: tokio::runtime::Handle::current(),
            known: Mutex::new(std::collections::HashMap::new()),
            _local_lookup: local_lookup,
            paths,
            shared: Mutex::new(HashMap::new()),
            live,
            hello: Arc::new(HelloBook::default()),
        })
    }

    /// This device's endpoint id: its device-link public key.
    pub fn id(&self) -> [u8; 32] {
        self.local
    }

    pub fn settings(&self) -> &MeshSettings {
        &self.settings
    }

    pub(crate) fn budget(&self) -> &Arc<Mutex<RelayBudget>> {
        &self.budget
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// The mailbox, when a mailbox server is configured.
    pub fn mailbox(&self) -> Option<&Mailbox> {
        self.mailbox.as_ref()
    }

    /// Relayed bytes used this month and the circle limit, for the hub's "Relayed this month" meter.
    pub fn relayed_this_month(&self) -> (u64, u64) {
        let mut budget = self.budget.lock().unwrap_or_else(|e| e.into_inner());
        (budget.used(now_ms()), budget.limit())
    }

    /// The address to dial a paired device: its id plus our relay (when relaying is on).
    pub fn peer_addr(&self, peer: &[u8; 32]) -> Result<EndpointAddr, MeshError> {
        self.addr_for(peer, true)
    }

    /// The dial address with or without our relay: the id plus every direct address known for the
    /// peer (a pairing QR, or the address its last nearby connection ran on).
    fn addr_for(&self, peer: &[u8; 32], with_relay: bool) -> Result<EndpointAddr, MeshError> {
        let id = EndpointId::from_bytes(peer).map_err(|e| MeshError::Address(e.to_string()))?;
        let mut addr = EndpointAddr::new(id);
        if with_relay && let Some(relay) = &self.relay {
            addr = addr.with_relay_url(relay.clone());
        }
        if let Some(known) = self
            .known
            .lock()
            .ok()
            .and_then(|map| map.get(peer).cloned())
        {
            for ip in known {
                addr = addr.with_ip_addr(ip);
            }
        }
        // The address its last nearby connection ran on (fresh for ten minutes): the dial needs no
        // mDNS answer to start on the local network.
        if let Some(seen) = self.peer_path(peer)
            && seen.label == PathLabel::Nearby
            && let Ok(ip) = seen.remote.parse::<std::net::SocketAddr>()
            && self
                .paths
                .lock()
                .ok()
                .and_then(|m| m.get(peer).and_then(|p| p.nearby_at))
                .is_some_and(|t| now_ms().saturating_sub(t) < NEARBY_FRESH_MS)
        {
            addr = addr.with_ip_addr(ip);
        }
        Ok(addr)
    }

    /// The selected path of the latest connection to `peer`, for the device list and the Send
    /// card. `None`: no connection was seen since this mesh started.
    pub fn peer_path(&self, peer: &[u8; 32]) -> Option<PeerPath> {
        self.paths.lock().ok()?.get(peer).cloned()
    }

    /// Remember direct addresses for `peer` (from a pairing QR or a saved record); replaces any.
    pub fn note_addrs(&self, peer: &[u8; 32], addrs: Vec<std::net::SocketAddr>) {
        if let Ok(mut map) = self.known.lock() {
            map.insert(*peer, addrs);
        }
    }

    /// This device's own direct addresses on private networks (RFC 1918, ULA, link-local is
    /// left out), for a pairing QR: what a phone on the same network dials.
    pub fn lan_addrs(&self) -> Vec<std::net::SocketAddr> {
        use std::net::IpAddr;
        self.endpoint
            .addr()
            .ip_addrs()
            .filter(|a| match a.ip() {
                IpAddr::V4(v4) => v4.is_private(),
                IpAddr::V6(v6) => (v6.segments()[0] & 0xfe00) == 0xfc00,
            })
            .copied()
            .collect()
    }

    /// Dial the active paired device `peer` on `alpn`: the local network first, our relay only when
    /// the device does not answer there (Pulse #469). With local discovery on and a relay set, a
    /// LAN-only dial (the id plus known direct addresses, resolved by mDNS) runs alone for
    /// [`LOCAL_HEAD_START`]; the relay dial then starts beside it and the first connection wins, the
    /// other is dropped, so a nearby device never rides the relay. A connection that still came up
    /// on the relay waits briefly for iroh to move it to the direct path ([`Mesh::settle`]); iroh
    /// keeps migrating it for as long as it lives.
    /// An active paired device, and for a guest only on `guest_alpns` (Pulse #492).
    fn dialable(&self, peer: &[u8; 32], alpn: &[u8]) -> bool {
        self.trust
            .peer(peer)
            .is_some_and(|info| !info.guest || self.guest_alpns.contains(&alpn))
    }

    pub async fn connect(&self, peer: &[u8; 32], alpn: &[u8]) -> Result<Connection, MeshError> {
        // A protocol a not-yet-paired device may open (pairing) can be dialled before the pairing.
        let open = self.open_alpns.contains(&alpn);
        if !open && !self.dialable(peer, alpn) {
            return Err(MeshError::NotPaired);
        }
        self.dial(peer, alpn).await
    }

    /// Like [`Mesh::connect`], but callers of one (peer, alpn) share one live connection: the
    /// cached one is returned while it is open and was used in the last `SHARED_REUSE_IDLE`;
    /// otherwise one is dialled exactly as `connect` does, cached, and watched. Concurrent callers
    /// for the same key wait on a per-key lock, so only one dials. Trust is checked on every call
    /// before any cache hit. The caller must never close the returned connection (it is shared):
    /// let it drop, or finish its own streams. Never used for the pairing protocols (`open_alpns`,
    /// one session per connection) or [`ALPN_KEYS`] (its handler serves one stream, then waits for
    /// the dialler to close).
    ///
    /// `fresh` skips the cache hit (after a stream on a cached connection failed) and replaces it.
    pub async fn connect_shared(
        &self,
        peer: &[u8; 32],
        alpn: &[u8],
        fresh: bool,
    ) -> Result<Connection, MeshError> {
        if alpn == ALPN_KEYS || self.open_alpns.contains(&alpn) {
            return self.connect(peer, alpn).await;
        }
        let key: SharedKey = (*peer, alpn.to_vec());
        if !self.dialable(peer, alpn) {
            // Revoked or never paired: nothing cached for it may be handed out.
            if let Ok(mut map) = self.shared.lock()
                && let Some(slot) = map.remove(&key)
            {
                slot.generation.fetch_add(1, Ordering::SeqCst);
            }
            return Err(MeshError::NotPaired);
        }
        // The long reuse window only where the acceptor closes idle connections on its own: the
        // host's stream protocols, from a peer whose hello says so (Pulse #560).
        let long =
            self.stream_alpns.contains(&alpn) && self.hello.cached(peer).has_cap(CAP_IDLE_CLOSE);
        let reuse = if long {
            SHARED_REUSE_IDLE_CAPABLE
        } else {
            SHARED_REUSE_IDLE
        };
        // Two tries: a dial that ends under a forgotten slot (wake, network change) is dropped and
        // dialled once more under the current one.
        let mut attempt = 0u8;
        // Re-picks of the slot because it was forgotten while this caller waited on its lock.
        let mut repicks = 0u8;
        while attempt < 2 {
            let slot = {
                let mut map = self.shared.lock().unwrap_or_else(|e| e.into_inner());
                map.entry(key.clone()).or_default().clone()
            };
            // Held across the dial: a second caller for this key waits here, then finds the
            // result. Only a connection THIS side dialled is ever cached: a connection the peer
            // dialled is served by the accept handler, and a request written on it would never be
            // read.
            let mut guard = slot.conn.lock().await;
            // Still the map's slot? A caller that queued behind a slot which was forgotten (a
            // revoke, a network change) must not dial and cache into the orphan: that would be a
            // second connection no later call could find (R2-9). Take the current slot instead.
            let current = self
                .shared
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&key)
                .is_some_and(|in_map| Arc::ptr_eq(in_map, &slot));
            if !current {
                // Never dial or cache into an orphan (N6): take the current slot, and give up
                // with an error, not a dial, when the map keeps changing under this caller.
                repicks += 1;
                if repicks >= 8 {
                    return Err(MeshError::Connect("the network changed".to_string()));
                }
                continue;
            }
            let generation = slot.generation.load(Ordering::SeqCst);
            if !fresh
                && attempt == 0
                && let Some(cached) = guard.as_mut()
                && cached.connection.close_reason().is_none()
                && cached.used.elapsed() < reuse
                // The wall clock keeps running while a sleeping computer's `Instant` stands still.
                && cached
                    .used_wall
                    .elapsed()
                    .is_ok_and(|gap| gap < reuse)
            {
                cached.used = Instant::now();
                cached.used_wall = SystemTime::now();
                return Ok(cached.connection.clone());
            }
            *guard = None;
            // Trust again: it may have been revoked while this caller waited on the lock.
            if !self.dialable(peer, alpn) {
                return Err(MeshError::NotPaired);
            }
            let connection = self.dial(peer, alpn).await?;
            // Revoked while dialling: refused (dropping the connection closes it).
            if !self.dialable(peer, alpn) {
                return Err(MeshError::NotPaired);
            }
            // The slot must still be the map's, with the generation it was read at, and the
            // connection is cached under the SAME hold of the map's lock (M6): a forget
            // (`forget_shared_key`, `forget_shared`, a revoke) takes that lock before it bumps the
            // generation, so it either comes first (this caller sees the orphan and drops the
            // connection, then dials again under the new slot) or after (the slot is then
            // forgotten with its connection, as a cached one is). No window leaves a connection
            // cached in an orphan. The block has no await, so the std guard never crosses one.
            let cached = {
                let map = self.shared.lock().unwrap_or_else(|e| e.into_inner());
                let current = map
                    .get(&key)
                    .is_some_and(|in_map| Arc::ptr_eq(in_map, &slot))
                    && slot.generation.load(Ordering::SeqCst) == generation;
                if current {
                    // The replaced entry (if any) was dropped above: with no watcher or list
                    // holding a strong handle, its streams ending closes it on its own (Pulse
                    // #560).
                    *guard = Some(SharedConn {
                        connection: connection.clone(),
                        used: Instant::now(),
                        used_wall: SystemTime::now(),
                    });
                }
                current
            };
            if !cached {
                // Forgotten while dialling: dropped, then dialled again under the current slot
                // (a connection started before a network change is not trusted).
                attempt += 1;
                continue;
            }
            // After a fresh dial, learn the peer's capabilities in the background when not known
            // (or older than ten minutes); this dial never waits for it.
            self.kick_hello(peer);
            return Ok(connection);
        }
        Err(MeshError::Connect("the network changed".to_string()))
    }

    /// Start the background hello for `peer` (see `hello.rs`); a guest is never asked (the hello
    /// is not one of its protocols), so a guest's `peer_hello` stays `Unknown`.
    fn kick_hello(&self, peer: &[u8; 32]) {
        if !self.dialable(peer, ALPN_HELLO) || !self.hello.needs_refresh(peer) {
            return;
        }
        if let Ok(addr) = self.peer_addr(peer) {
            spawn_hello(&self.rt, &self.hello, &self.endpoint, *peer, addr);
        }
    }

    /// The cached hello of `peer`, with no network and no side effect (Pulse #560).
    pub fn peer_hello_cached(&self, peer: &[u8; 32]) -> PeerHello {
        self.hello.cached(peer)
    }

    /// What `peer` told us about its build and capabilities (Pulse #560), from the cache ONLY: it
    /// never waits on the network. With nothing cached (or an entry older than ten minutes) it
    /// starts one background fetch (single-flight per peer) and answers what it has, `Unknown`
    /// when nothing: the caller proceeds as it would with an unknown peer. `Legacy` is an older
    /// Pulse whose handshake refused the protocol, seen twice; any other failure is `Unknown`.
    pub fn peer_hello(&self, peer: &[u8; 32]) -> PeerHello {
        self.kick_hello(peer);
        self.hello.cached(peer)
    }

    /// Tell iroh the network changed (wake from sleep, Wi-Fi switch) and forget the shared dial
    /// cache (Pulse #560): a cached connection from before the change is likely dead and would cost
    /// a failed first stream. Nothing is closed: an entry only leaves the cache, and a stream in
    /// flight keeps its own handle. A cached `Legacy` hello is forgotten too (the peer may have
    /// restarted on a current build).
    pub async fn network_change(&self) {
        self.forget_shared();
        self.hello.clear_legacy();
        self.endpoint.network_change().await;
    }

    /// Drop every cached shared connection (see [`Mesh::network_change`]). Every slot's generation
    /// moves, so a dial in flight does not repopulate the cache with its stale result.
    pub fn forget_shared(&self) {
        let mut map = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        for slot in map.values() {
            slot.generation.fetch_add(1, Ordering::SeqCst);
        }
        map.clear();
    }

    /// Drop the cached shared connection of ONE (peer, alpn) (Pulse #560): a failed request on that
    /// protocol must not end connections of other protocols to the same peer (`close_peer` does).
    /// Nothing is closed; a stream in flight keeps its own handle.
    pub fn forget_shared_key(&self, peer: &[u8; 32], alpn: &[u8]) {
        let key: SharedKey = (*peer, alpn.to_vec());
        let mut map = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = map.remove(&key) {
            slot.generation.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Sizes of the mesh's own per-peer state, counts only (Pulse #501: the engine logs them every
    /// minute so a growing map or a peer holding many connections is visible). Each lock is
    /// taken alone, never inside another.
    pub fn memory_counts(&self) -> MeshCounts {
        let mut open_per_peer: Vec<u32> = {
            let book = self.paths.lock().unwrap_or_else(|e| e.into_inner());
            book.values().map(|p| p.open).filter(|n| *n > 0).collect()
        };
        open_per_peer.sort_unstable_by(|a, b| b.cmp(a));
        let path_peers = self.paths.lock().unwrap_or_else(|e| e.into_inner()).len();
        let shared_slots = self.shared.lock().unwrap_or_else(|e| e.into_inner()).len();
        let accepted = self.live.lock().unwrap_or_else(|e| e.into_inner()).len();
        let known_peers = self.known.lock().unwrap_or_else(|e| e.into_inner()).len();
        let watcher_views = WATCHERS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map_or(0, |views| views.values().map(|m| m.len()).sum());
        MeshCounts {
            path_peers,
            open_per_peer,
            shared_slots,
            accepted,
            known_peers,
            watcher_views,
        }
    }

    /// Close every connection to and from `peer` now, on every protocol (R09): the cached ones
    /// this side dialled and the ones the peer dialled. Call it after the local deny is persisted;
    /// streams in flight end, and nothing the peer sent after this is read. A connection the
    /// peer dials again meets the trust gate, which refuses a revoked key.
    pub fn close_peer(&self, peer: &[u8; 32]) {
        let dialled: Vec<Arc<SharedSlot>> = {
            let mut map = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            let keys: Vec<SharedKey> = map.keys().filter(|(p, _)| p == peer).cloned().collect();
            keys.iter().filter_map(|key| map.remove(key)).collect()
        };
        for slot in dialled {
            slot.generation.fetch_add(1, Ordering::SeqCst);
            // A caller holding the slot is mid-dial; its result is refused by the trust check and
            // the generation check it makes after the dial, and a finished connection is closed
            // here.
            if let Ok(mut guard) = slot.conn.try_lock()
                && let Some(shared) = guard.take()
            {
                shared.connection.close(2u32.into(), b"device removed");
            }
        }
        let accepted: Vec<Connection> = {
            let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
            let mut gone = Vec::new();
            let mut kept = Vec::new();
            for weak in live.drain(..) {
                // A failed upgrade is a connection nobody holds any more: already closed.
                let Some(connection) = weak.upgrade() else {
                    continue;
                };
                if connection.close_reason().is_some() {
                    continue;
                }
                if connection.remote_id().as_bytes() == peer {
                    gone.push(connection);
                } else {
                    kept.push(weak);
                }
            }
            *live = kept;
            gone
        };
        for connection in accepted {
            connection.close(2u32.into(), b"device removed");
        }
    }

    /// Dial a new connection (local network first, relay beside it), watch its path and settle.
    async fn dial(&self, peer: &[u8; 32], alpn: &[u8]) -> Result<Connection, MeshError> {
        let lan_first = self.settings.local_discovery && self.relay.is_some();
        let connection = if lan_first {
            self.dial_local_first(peer, alpn).await?
        } else {
            self.endpoint
                .connect(self.peer_addr(peer)?, alpn)
                .await
                .map_err(|e| MeshError::Connect(e.to_string()))?
        };
        watch_path(self.paths.clone(), &connection, "dial");
        if lan_first {
            self.settle(peer, &connection).await;
        }
        Ok(connection)
    }

    async fn dial_local_first(
        &self,
        peer: &[u8; 32],
        alpn: &[u8],
    ) -> Result<Connection, MeshError> {
        fn fail<E: fmt::Display>(e: E) -> MeshError {
            MeshError::Connect(e.to_string())
        }
        let lan = self.endpoint.connect(self.addr_for(peer, false)?, alpn);
        tokio::pin!(lan);
        match tokio::time::timeout(LOCAL_HEAD_START, &mut lan).await {
            Ok(Ok(connection)) => return Ok(connection),
            Ok(Err(_)) => {
                // The LAN dial failed outright (nothing known for this id): the relay is the way.
                return self
                    .endpoint
                    .connect(self.peer_addr(peer)?, alpn)
                    .await
                    .map_err(fail);
            }
            Err(_) => {}
        }
        let relay = self.endpoint.connect(self.peer_addr(peer)?, alpn);
        tokio::pin!(relay);
        tokio::select! {
            result = &mut lan => match result {
                Ok(connection) => Ok(connection),
                Err(_) => relay.await.map_err(fail),
            },
            result = &mut relay => match result {
                Ok(connection) => Ok(connection),
                Err(_) => lan.await.map_err(fail),
            },
        }
    }

    /// A connection that opened on the relay while local discovery is on: wait for the selected
    /// path to become an IP path (iroh opens it by NAT traversal over the relay) for up to
    /// [`SETTLE_NEARBY`] when the peer ran nearby in the last ten minutes, [`SETTLE_UNKNOWN`] when
    /// nothing is known, and not at all for a minute after a wait failed (a peer on another network).
    async fn settle(&self, peer: &[u8; 32], connection: &Connection) {
        if path_kind(connection) == PathKind::Direct {
            return;
        }
        let now = now_ms();
        let window = {
            let map = self.paths.lock().unwrap_or_else(|e| e.into_inner());
            match map.get(peer) {
                Some(p)
                    if p.nearby_at
                        .is_some_and(|t| now.saturating_sub(t) < NEARBY_FRESH_MS) =>
                {
                    SETTLE_NEARBY
                }
                Some(p)
                    if p.settle_failed_at
                        .is_some_and(|t| now.saturating_sub(t) < SETTLE_SKIP_MS) =>
                {
                    return;
                }
                _ => SETTLE_UNKNOWN,
            }
        };
        let started = Instant::now();
        while started.elapsed() < window {
            tokio::time::sleep(Duration::from_millis(40)).await;
            if path_kind(connection) == PathKind::Direct {
                return;
            }
        }
        let mut map = self.paths.lock().unwrap_or_else(|e| e.into_inner());
        let failed = Some(now_ms());
        match map.get_mut(peer) {
            Some(entry) => entry.settle_failed_at = failed,
            None => {
                map.insert(
                    *peer,
                    PeerPath {
                        label: PathLabel::Relay,
                        remote: "relay".to_string(),
                        rtt_ms: 0,
                        live: true,
                        since_ms: now_ms(),
                        seen_ms: now_ms(),
                        generation: 0,
                        open: 0,
                        nearby_at: None,
                        settle_failed_at: failed,
                    },
                );
            }
        }
    }

    /// Exchange mailbox keys with a paired device over a live connection (once per pairing, and to
    /// rotate). The engine runs it right after pairing and on the first connection with a new member.
    pub async fn exchange_keys(&self, peer: &[u8; 32]) -> Result<PairKeys, MeshError> {
        let connection = self.connect(peer, ALPN_KEYS).await?;
        let keys = self.exchange.exchange(&connection).await;
        connection.close(0u32.into(), b"done");
        keys
    }

    /// On connect: pull each `(collection, cursor)` from `peer` until it has nothing more, and
    /// return `(collection, result)` for the caller to store the cursors. At most 20 pages per
    /// collection per call; a result with `more` still true needs another call.
    pub async fn reconcile(
        &self,
        peer: &[u8; 32],
        collections: &[(String, u64)],
    ) -> Result<Vec<(String, PullResult)>, MeshError> {
        let records = self.records.as_ref().ok_or(MeshError::Disabled)?;
        let info = self.trust.peer(peer).ok_or(MeshError::NotPaired)?;
        let connection = self
            .connect_shared(peer, crate::sync::ALPN_SYNC, false)
            .await?;
        let mut out = Vec::new();
        for (collection, cursor) in collections {
            let mut since = *cursor;
            let mut total = PullResult::default();
            for _ in 0..20 {
                let page = pull_collection(&connection, records.as_ref(), &info, collection, since)
                    .await?;
                since = page.next_cursor;
                total.applied += page.applied;
                total.needs_direct.extend(page.needs_direct);
                total.next_cursor = page.next_cursor;
                total.more = page.more;
                // A skipped record needs a direct path; pulling again here would only skip it again.
                if !page.more || !total.needs_direct.is_empty() {
                    break;
                }
            }
            out.push((collection.clone(), total));
        }
        // The connection is shared (`connect_shared`): it is left open, not closed here.
        Ok(out)
    }

    /// Close the endpoint and every handler cleanly.
    pub async fn shutdown(&self) {
        let _ = self.router.shutdown().await;
    }
}

// ---- blocking streams: a host with no async runtime of its own -------------------------------

/// How long an idle connection waits for its next stream before the handler closes it. Longer than
/// [`SHARED_REUSE_IDLE_CAPABLE`] (90 s) with a 30 s margin, and so than [`SHARED_REUSE_IDLE`], so a
/// reused connection is never let go under a stream; an idle connection costs only the QUIC
/// keep-alive. The `idle-close` capability of the hello (Pulse #560) promises exactly this close;
/// 120 s is the value that promise is made at, so do not lower it below 90 s plus a margin.
const STREAM_IDLE: Duration = Duration::from_secs(120);
/// The last look for a stream after [`STREAM_IDLE`] ran out, before the connection is closed.
const IDLE_GRACE: Duration = Duration::from_secs(2);
/// How long the acceptor waits for the opener's marker byte once a stream exists.
const STREAM_OPEN_WAIT: Duration = Duration::from_secs(10);

/// A host's blocking protocol on the mesh (for example the engine's pairing and link requests).
/// `serve` runs on a blocking thread for each stream a peer opens on the registered ALPN, and the
/// stream is finished when it returns. `peer` is the remote endpoint id, authenticated by the
/// iroh handshake and checked by the trust gate (unless the ALPN is in `open_alpns`).
pub trait StreamService: Send + Sync + 'static {
    fn serve(&self, peer: [u8; 32], stream: BlockingStream);
}

/// The wait applied to each read and write of a [`BlockingStream`], settable from a clone after
/// the stream moved into a reader.
#[derive(Clone, Debug)]
pub struct StreamControl(Arc<AtomicU64>);

impl StreamControl {
    pub fn set_timeout(&self, wait: Duration) {
        let ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX).max(1);
        self.0.store(ms, Ordering::SeqCst);
    }
}

/// One iroh bidirectional stream as `std::io::Read` and `Write`. A read or write that waits longer
/// than the control's timeout fails with `ErrorKind::TimedOut`. It blocks through the mesh's
/// runtime, so use it from a thread that is not itself running inside that runtime. Dropping it
/// finishes the sending side (and releases its hold on the connection).
pub struct BlockingStream {
    rt: tokio::runtime::Handle,
    send: SendHalf,
    recv: iroh::endpoint::RecvStream,
    wait: Arc<AtomicU64>,
    // Keeps the connection open while the stream is in use, and tells a relayed path from a direct
    // one to [`BlockingStream::split`].
    connection: Connection,
    budget: Arc<Mutex<RelayBudget>>,
}

/// The sending side of a stream: finished when dropped.
struct SendHalf(iroh::endpoint::SendStream);

impl Drop for SendHalf {
    fn drop(&mut self) {
        let _ = self.0.finish();
    }
}

impl BlockingStream {
    fn new(
        rt: tokio::runtime::Handle,
        send: iroh::endpoint::SendStream,
        recv: iroh::endpoint::RecvStream,
        connection: Connection,
        budget: Arc<Mutex<RelayBudget>>,
    ) -> Self {
        Self {
            rt,
            send: SendHalf(send),
            recv,
            wait: Arc::new(AtomicU64::new(30_000)),
            connection,
            budget,
        }
    }

    /// Split into a reader and a writer that two threads can drive at once (a byte pipe such as
    /// the ssh tunnel, Pulse #626). `read_wait` bounds one read (an idle ssh session waits long),
    /// `write_wait` one write. While the path is not a direct IP path the halves meter their
    /// bytes: at most [`RELAY_TRANSFER_MAX`] for the whole stream, counted against the circle's
    /// monthly relay budget, fail closed (see `Tunnel`). Dropping the writer finishes the
    /// sending side; the stream's hold on the connection ends with the last half.
    pub fn split(self, read_wait: Duration, write_wait: Duration) -> (TunnelReader, TunnelWriter) {
        let tunnel = Arc::new(Tunnel {
            connection: self.connection,
            budget: self.budget,
            state: Mutex::new(TunnelState::default()),
        });
        let ms = |wait: Duration| u64::try_from(wait.as_millis()).unwrap_or(u64::MAX).max(1);
        (
            TunnelReader {
                rt: self.rt.clone(),
                recv: self.recv,
                wait: ms(read_wait),
                tunnel: tunnel.clone(),
            },
            TunnelWriter {
                rt: self.rt,
                send: self.send,
                wait: ms(write_wait),
                tunnel,
            },
        )
    }

    pub fn control(&self) -> StreamControl {
        StreamControl(self.wait.clone())
    }

    fn wait(&self) -> Duration {
        Duration::from_millis(self.wait.load(Ordering::SeqCst))
    }
}

impl Read for BlockingStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let wait = self.wait();
        let recv = &mut self.recv;
        self.rt.block_on(async {
            match tokio::time::timeout(wait, AsyncReadExt::read(recv, buf)).await {
                Ok(result) => result,
                Err(_) => Err(io::ErrorKind::TimedOut.into()),
            }
        })
    }
}

impl Write for BlockingStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let wait = self.wait();
        let send = &mut self.send.0;
        self.rt.block_on(async {
            match tokio::time::timeout(wait, AsyncWriteExt::write(send, buf)).await {
                Ok(result) => result,
                Err(_) => Err(io::ErrorKind::TimedOut.into()),
            }
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        let wait = self.wait();
        let send = &mut self.send.0;
        self.rt.block_on(async {
            match tokio::time::timeout(wait, send.flush()).await {
                Ok(result) => result,
                Err(_) => Err(io::ErrorKind::TimedOut.into()),
            }
        })
    }
}

// ---- a byte pipe over one stream (the ssh tunnel) --------------------------------

/// Relay accounting shared by the two halves of a split stream.
struct Tunnel {
    connection: Connection,
    budget: Arc<Mutex<RelayBudget>>,
    state: Mutex<TunnelState>,
}

#[derive(Default)]
struct TunnelState {
    /// Bytes that moved while the path was not direct, both directions.
    relayed: u64,
    /// Bytes reserved in the monthly ledger so far (taken in steps, the unused part refunded).
    reserved: u64,
}

/// The ledger is written once per this many relayed bytes, not once per read.
const TUNNEL_RESERVE_STEP: u64 = 64 * 1024;

impl Tunnel {
    /// Count `bytes` that just moved. A direct path is never capped and never counted. A relayed
    /// (or not provably direct) path allows [`RELAY_TRANSFER_MAX`] per stream and what is left of
    /// the circle's month; past either, the error ends the stream (D7: the caps fail closed). The
    /// message never carries a byte of the stream.
    fn charge(&self, bytes: usize) -> io::Result<()> {
        if path_kind(&self.connection) == PathKind::Direct {
            return Ok(());
        }
        let bytes = bytes as u64;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.relayed = state.relayed.saturating_add(bytes);
        if state.relayed > RELAY_TRANSFER_MAX {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "relay_limit: a relayed session carries at most 25 MB; it needs a direct connection",
            ));
        }
        if state.relayed > state.reserved {
            let mut budget = self.budget.lock().unwrap_or_else(|e| e.into_inner());
            let step = TUNNEL_RESERVE_STEP.max(state.relayed - state.reserved);
            budget.reserve(now_ms(), step).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "relay_budget_spent: the circle has used its relayed bytes for this month",
                )
            })?;
            state.reserved += step;
        }
        Ok(())
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap_or_else(|e| e.into_inner());
        let unused = state.reserved.saturating_sub(state.relayed);
        if unused > 0 {
            let mut budget = self.budget.lock().unwrap_or_else(|e| e.into_inner());
            let _ = budget.refund(unused);
        }
    }
}

/// The receiving half of a split [`BlockingStream`]: `Ok(0)` when the peer finished its side.
pub struct TunnelReader {
    rt: tokio::runtime::Handle,
    recv: iroh::endpoint::RecvStream,
    wait: u64,
    tunnel: Arc<Tunnel>,
}

impl Read for TunnelReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let wait = Duration::from_millis(self.wait);
        let recv = &mut self.recv;
        let n = self.rt.block_on(async {
            match tokio::time::timeout(wait, AsyncReadExt::read(recv, buf)).await {
                Ok(result) => result,
                Err(_) => Err(io::ErrorKind::TimedOut.into()),
            }
        })?;
        self.tunnel.charge(n)?;
        Ok(n)
    }
}

/// The sending half of a split [`BlockingStream`]; dropping it finishes the sending side.
pub struct TunnelWriter {
    rt: tokio::runtime::Handle,
    send: SendHalf,
    wait: u64,
    tunnel: Arc<Tunnel>,
}

impl Write for TunnelWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.tunnel.charge(buf.len())?;
        let wait = Duration::from_millis(self.wait);
        let send = &mut self.send.0;
        self.rt.block_on(async {
            match tokio::time::timeout(wait, AsyncWriteExt::write(send, buf)).await {
                Ok(result) => result,
                Err(_) => Err(io::ErrorKind::TimedOut.into()),
            }
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        let wait = Duration::from_millis(self.wait);
        let send = &mut self.send.0;
        self.rt.block_on(async {
            match tokio::time::timeout(wait, send.flush()).await {
                Ok(result) => result,
                Err(_) => Err(io::ErrorKind::TimedOut.into()),
            }
        })
    }
}

/// Registers a [`StreamService`] as an iroh protocol handler.
struct StreamHandler {
    service: Arc<dyn StreamService>,
    rt: tokio::runtime::Handle,
    paths: PathBook,
    budget: Arc<Mutex<RelayBudget>>,
}

impl fmt::Debug for StreamHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StreamHandler")
    }
}

impl ProtocolHandler for StreamHandler {
    /// Serves every stream the peer opens on this connection concurrently (a reused connection
    /// carries many); the handler closes the connection and returns only when no stream is being
    /// served and nothing new arrived for [`STREAM_IDLE`], or the connection closed.
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let peer = *connection.remote_id().as_bytes();
        watch_path(self.paths.clone(), &connection, "accept");
        let mut active: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
        loop {
            let next = if active.is_empty() {
                match tokio::time::timeout(STREAM_IDLE, connection.accept_bi()).await {
                    Ok(next) => Some(next),
                    // The idle deadline passed: one last short look before closing, so a stream
                    // the dialler opened in that very moment is served, not dropped (Pulse #560).
                    Err(_) => tokio::time::timeout(IDLE_GRACE, connection.accept_bi())
                        .await
                        .ok(),
                }
            } else {
                tokio::select! {
                    next = connection.accept_bi() => Some(next),
                    // A served stream finished: re-evaluate (the loop may now be idle).
                    _ = active.join_next() => continue,
                }
            };
            let Some(Ok((send, mut recv))) = next else {
                break;
            };
            let rt = self.rt.clone();
            let service = self.service.clone();
            let budget = self.budget.clone();
            let connection = connection.clone();
            // The marker read and the serve run in their own task, so a slow opener or a long
            // serve never holds up the next accept_bi.
            active.spawn(async move {
                // QUIC shows a stream to the acceptor only once the opener sent data, so every
                // opener starts with one marker byte (`SyncMesh::open_stream`); a Host that must
                // speak first (the pairing offer) would otherwise wait forever.
                let mut marker = [0u8; 1];
                let read =
                    tokio::time::timeout(STREAM_OPEN_WAIT, recv.read_exact(&mut marker)).await;
                if !matches!(read, Ok(Ok(_))) {
                    return;
                }
                let stream = BlockingStream::new(rt, send, recv, connection, budget);
                let _ = tokio::task::spawn_blocking(move || service.serve(peer, stream)).await;
            });
        }
        // Let in-flight streams finish, then close the connection: returning alone does not end
        // it (the router's handle drops, but the dialler's keep-alive and any stream it still
        // holds keep the QUIC connection up, Pulse #560), and an open
        // connection nobody reads would take the opener's next request and never answer it.
        while active.join_next().await.is_some() {}
        connection.close(0u32.into(), b"idle");
        Ok(())
    }
}

/// A [`Mesh`] with its own runtime, for a host whose code is blocking threads (the engine). Every
/// method blocks; call them from threads outside the mesh's runtime.
pub struct SyncMesh {
    rt: tokio::runtime::Runtime,
    mesh: Mesh,
}

impl SyncMesh {
    pub fn start(init: MeshInit) -> Result<Self, MeshError> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("pulse-mesh")
            .enable_all()
            .build()
            .map_err(|e| MeshError::Endpoint(e.to_string()))?;
        let mesh = rt.block_on(Mesh::start(init, |router| router))?;
        Ok(Self { rt, mesh })
    }

    /// This device's endpoint id: its device-link public key.
    pub fn id(&self) -> [u8; 32] {
        self.mesh.id()
    }

    /// See [`Mesh::lan_addrs`].
    pub fn lan_addrs(&self) -> Vec<std::net::SocketAddr> {
        self.mesh.lan_addrs()
    }

    /// See [`Mesh::note_addrs`].
    pub fn note_addrs(&self, peer: &[u8; 32], addrs: Vec<std::net::SocketAddr>) {
        self.mesh.note_addrs(peer, addrs);
    }

    /// See [`Mesh::peer_path`].
    pub fn peer_path(&self, peer: &[u8; 32]) -> Option<PeerPath> {
        self.mesh.peer_path(peer)
    }

    /// See [`Mesh::close_peer`].
    pub fn close_peer(&self, peer: &[u8; 32]) {
        self.mesh.close_peer(peer);
    }

    /// Open (or reuse) the shared connection to `peer` on `alpn` within `wait`, sending nothing
    /// (Pulse #560): the cache keeps it, so the next stream on it opens at once.
    pub fn warm(&self, peer: &[u8; 32], alpn: &[u8], wait: Duration) -> Result<(), MeshError> {
        self.rt.block_on(async {
            tokio::time::timeout(wait, self.mesh.connect_shared(peer, alpn, false))
                .await
                .map_err(|_| MeshError::Connect("timed out".to_string()))?
                .map(|_| ())
        })
    }

    /// See [`Mesh::peer_hello`]: cached only, with a background fetch; never blocks on the network.
    pub fn peer_hello(&self, peer: &[u8; 32]) -> PeerHello {
        self.mesh.peer_hello(peer)
    }

    /// See [`Mesh::forget_shared_key`].
    pub fn forget_shared_key(&self, peer: &[u8; 32], alpn: &[u8]) {
        self.mesh.forget_shared_key(peer, alpn);
    }

    /// See [`Mesh::peer_hello_cached`]; no network.
    pub fn peer_hello_cached(&self, peer: &[u8; 32]) -> PeerHello {
        self.mesh.peer_hello_cached(peer)
    }

    /// See [`Mesh::network_change`] (Pulse #560).
    pub fn network_change(&self) {
        // Bounded: a wake handler must never hang on iroh's actor.
        let _ = self.rt.block_on(tokio::time::timeout(
            Duration::from_secs(5),
            self.mesh.network_change(),
        ));
    }

    /// Dial `peer` on `alpn` and open one stream (a paired device, or any device for an ALPN in
    /// `open_alpns`), waiting up to `wait` for the connection. Direct first, our relay as fallback.
    pub fn open_stream(
        &self,
        peer: &[u8; 32],
        alpn: &[u8],
        wait: Duration,
    ) -> Result<BlockingStream, MeshError> {
        let mesh = &self.mesh;
        let (connection, send, recv) = self.rt.block_on(async {
            // One shared connection per (peer, alpn). If a stream cannot be opened on a reused
            // one (the other side let it go just now), dial a fresh one once.
            let mut fresh = false;
            loop {
                let connection = tokio::time::timeout(wait, mesh.connect_shared(peer, alpn, fresh))
                    .await
                    .map_err(|_| MeshError::Connect("timed out".to_string()))??;
                let opened = async {
                    let (mut send, recv) = connection
                        .open_bi()
                        .await
                        .map_err(|e| MeshError::Stream(e.to_string()))?;
                    send.write_all(&[0u8])
                        .await
                        .map_err(|e| MeshError::Stream(e.to_string()))?;
                    Ok::<_, MeshError>((send, recv))
                }
                .await;
                match opened {
                    Ok((send, recv)) => break Ok::<_, MeshError>((connection, send, recv)),
                    Err(_) if !fresh => fresh = true,
                    Err(e) => break Err(e),
                }
            }
        })?;
        let stream = BlockingStream::new(
            self.rt.handle().clone(),
            send,
            recv,
            connection,
            mesh.budget().clone(),
        );
        stream.control().set_timeout(wait);
        Ok(stream)
    }

    /// The async mesh, for the calls this wrapper does not mirror (`send_bulk`, `reconcile`,
    /// `exchange_keys`, the mailbox). Run their futures with [`SyncMesh::block_on`].
    pub fn mesh(&self) -> &Mesh {
        &self.mesh
    }

    /// Run a mesh future to completion from a blocking thread.
    pub fn block_on<F: std::future::Future>(&self, future: F) -> F::Output {
        self.rt.block_on(future)
    }

    /// Close the endpoint and its handlers. The runtime stops when the last holder drops this.
    pub fn shutdown(&self) {
        self.rt.block_on(self.mesh.shutdown());
    }
}
