//! The capability hello (Pulse #560): which build a paired device runs and what it can do.
//!
//! Before this, a Send to a device on an older build failed with "the other device declined it"
//! and nothing said why: the manifest message of `6bc4e8dd` is unknown to an older receiver, which
//! drops it. The hello is a tiny internal protocol, `right/hello/1`, served by every endpoint that
//! accepts connections, behind the same trust gate as every other protocol (paired devices only).
//!
//! Wire: the dialler opens one bidirectional stream and writes one marker byte (QUIC shows a
//! stream to the acceptor only once data arrives, the convention of every blocking protocol here);
//! the acceptor answers `u32 big-endian length | JSON {"v":1,"caps":[..],"build":".."}` and
//! finishes. The dialler closes the connection (code 0).
//!
//! An older Pulse does not register the protocol, so its endpoint refuses the TLS handshake with
//! the "no application protocol" alert (QUIC crypto error 0x100 + 120). Exactly that error code,
//! matched on the typed iroh error and seen twice in a row, reads as [`PeerHello::Legacy`] (an
//! older Pulse). Any other failure (timeout, no route, the peer asleep) is [`PeerHello::Unknown`]
//! and is never cached, so a sleeping device is not mistaken for an old one. A CURRENT build that
//! does not accept connections (`deviceLinkListen` off, or mid-restart before its protocols
//! register) raises the same alert and also reads as Legacy: either way it cannot take a Send.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use iroh::endpoint::{ConnectError, ConnectingError, Connection, ConnectionError};
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointAddr};
use serde::{Deserialize, Serialize};

pub const ALPN_HELLO: &[u8] = b"right/hello/1";

/// The capability this crate itself adds: on the host's own blocking stream protocols (the
/// `StreamHandler` ALPNs, `MeshInit::services`) the acceptor closes a connection that served no
/// stream for `STREAM_IDLE`, so a dialler may reuse one of THOSE for longer (`endpoint.rs`, Pulse
/// #560). Sync, bulk and key connections do not carry it and keep the short window.
pub const CAP_IDLE_CLOSE: &str = "idle-close";

/// The capability words a dialler accepts from a peer (anything else is dropped): the mesh's own
/// and the engine's. A peer's claim is a hint that picks a longer reuse window, never a grant.
const KNOWN_CAPS: &[&str] = &[
    CAP_IDLE_CLOSE,
    "send-manifest",
    "small-unsupported",
    "agent-files",
];

/// A cached answer is refreshed in the background once it is older than this; a cached `Legacy`
/// older than this reads as `Unknown` until the refresh lands (an upgraded peer must not stay
/// "older" for the life of the mesh).
const HELLO_REFRESH: Duration = Duration::from_secs(600);
/// After a failed attempt a peer is not asked again for this long.
const FAILED_BACKOFF: Duration = Duration::from_secs(30);
/// How long a `Known` answer is trusted when every refresh fails (N10): a peer that went away, or
/// came back on another build, is `Unknown` after this, never "known" for the life of the mesh.
const HELLO_KNOWN_LIFE: Duration = Duration::from_secs(3600);
/// Longest build text kept, and the characters it may hold.
const BUILD_MAX: usize = 64;

/// A peer's (or our own) build text, clamped to 64 characters of `[A-Za-z0-9._+-]`: it reaches
/// the hub log, so nothing else may ride in it.
pub fn clamp_build(build: &str) -> String {
    build
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
        .take(BUILD_MAX)
        .collect()
}
/// The whole exchange (dial, stream, answer) is abandoned after this.
const HELLO_WAIT: Duration = Duration::from_secs(8);
/// The acceptor's wait for the dialler's marker, and for the dialler's close after the answer.
const HELLO_SERVE_WAIT: Duration = Duration::from_secs(5);
/// Longest answer the dialler reads.
const HELLO_MAX_BYTES: usize = 4096;

/// What a paired device told us about itself, as the dialler sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeerHello {
    /// The device answered: its build text and capability words.
    Known { build: String, caps: Vec<String> },
    /// The device refused the protocol outright: an older Pulse.
    Legacy,
    /// Nothing learned yet (not asked, or the attempt failed for another reason).
    Unknown,
}

impl PeerHello {
    /// True only for a device that answered and listed `cap`.
    pub fn has_cap(&self, cap: &str) -> bool {
        matches!(self, PeerHello::Known { caps, .. } if caps.iter().any(|c| c == cap))
    }
}

#[derive(Serialize, Deserialize)]
struct HelloWire {
    v: u32,
    caps: Vec<String>,
    build: String,
}

/// One `mesh.peer.hello` observation: written to the hub log by the host, once per change.
#[derive(Clone, Debug)]
pub struct HelloEvent {
    pub peer: [u8; 32],
    /// `known`, `legacy` or `unknown`.
    pub result: &'static str,
    pub build: String,
    pub caps: Vec<String>,
    /// For `unknown`: a fixed word (`connect`, `stream`, `decode`, `version`, `length`,
    /// `timeout`); never an address or a peer's text.
    pub cause: &'static str,
}

type HelloObserver = Box<dyn Fn(&HelloEvent) + Send + Sync>;

static HELLO_OBSERVER: OnceLock<HelloObserver> = OnceLock::new();

/// Register the process-wide hello observer (the engine logs `mesh.peer.hello`). Set once.
pub fn set_hello_observer(observer: impl Fn(&HelloEvent) + Send + Sync + 'static) {
    let _ = HELLO_OBSERVER.set(Box::new(observer));
}

/// The outcome of one attempt.
pub(crate) enum HelloFetch {
    Got { build: String, caps: Vec<String> },
    Legacy,
    Failed(&'static str),
}

/// Serves the hello: the answer is built once, the handler only writes it.
pub(crate) struct HelloHandler {
    answer: Vec<u8>,
}

impl fmt::Debug for HelloHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HelloHandler")
    }
}

impl HelloHandler {
    pub(crate) fn new(build: &str, caps: &[String]) -> Self {
        let json = serde_json::to_vec(&HelloWire {
            v: 1,
            caps: caps.to_vec(),
            build: clamp_build(build),
        })
        .unwrap_or_default();
        let mut answer = Vec::with_capacity(json.len() + 4);
        answer.extend_from_slice(&(json.len() as u32).to_be_bytes());
        answer.extend_from_slice(&json);
        Self { answer }
    }
}

impl ProtocolHandler for HelloHandler {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let served = async {
            let (mut send, mut recv) = connection.accept_bi().await.ok()?;
            let mut marker = [0u8; 1];
            recv.read_exact(&mut marker).await.ok()?;
            send.write_all(&self.answer).await.ok()?;
            send.finish().ok()?;
            Some(())
        };
        let _ = tokio::time::timeout(HELLO_SERVE_WAIT, served).await;
        // Let the dialler read the answer and close; then close ourselves (returning alone does
        // not end a connection: its path watcher and the keep-alive would keep it open).
        let _ = tokio::time::timeout(HELLO_SERVE_WAIT, connection.closed()).await;
        connection.close(0u32.into(), b"hello");
        Ok(())
    }
}

/// The QUIC code of the TLS "no application protocol" alert (120): 0x100 plus the alert.
const ALERT_NO_APPLICATION_PROTOCOL: u64 = 0x100 | 120;

/// True when a failed dial is the peer refusing the protocol: an endpoint that does not serve
/// `right/hello/1` closes the handshake with crypto code 120. Matched on the typed error (the
/// peer's close frame, or the same code raised locally), never on text; anything else is not
/// evidence of an old build.
fn is_alpn_refusal(error: &ConnectError) -> bool {
    let closed = match error {
        ConnectError::Connecting {
            source: ConnectingError::ConnectionError { source, .. },
            ..
        } => source,
        ConnectError::Connection { source, .. } => source,
        _ => return false,
    };
    match closed {
        ConnectionError::ConnectionClosed(close) => {
            u64::from(close.error_code) == ALERT_NO_APPLICATION_PROTOCOL
        }
        ConnectionError::TransportError(error) => {
            u64::from(error.code) == ALERT_NO_APPLICATION_PROTOCOL
        }
        _ => false,
    }
}

/// Ask `addr` for its hello: one dial, one stream, closed by us. `Legacy` needs the alert twice
/// in a row (a second dial half a second after the first), so one odd handshake never brands a
/// current build. Failure causes are fixed words (they reach the hub log): never an address.
pub(crate) async fn fetch_hello(endpoint: &Endpoint, addr: EndpointAddr) -> HelloFetch {
    let exchange = async {
        let mut alerts = 0u8;
        let connection = loop {
            match endpoint.connect(addr.clone(), ALPN_HELLO).await {
                Ok(connection) => break connection,
                Err(e) if is_alpn_refusal(&e) => {
                    alerts += 1;
                    if alerts >= 2 {
                        return HelloFetch::Legacy;
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                Err(_) => return HelloFetch::Failed("connect"),
            }
        };
        let read = async {
            let (mut send, mut recv) = connection.open_bi().await.map_err(|_| "stream")?;
            send.write_all(&[0u8]).await.map_err(|_| "stream")?;
            send.finish().map_err(|_| "stream")?;
            let mut len = [0u8; 4];
            recv.read_exact(&mut len).await.map_err(|_| "stream")?;
            let len = u32::from_be_bytes(len) as usize;
            if len == 0 || len > HELLO_MAX_BYTES {
                return Err("length");
            }
            let mut body = vec![0u8; len];
            recv.read_exact(&mut body).await.map_err(|_| "stream")?;
            let wire = serde_json::from_slice::<HelloWire>(&body).map_err(|_| "decode")?;
            // A different wire version is not understood: not a hello, so nothing is learned.
            if wire.v != 1 {
                return Err("version");
            }
            Ok(wire)
        }
        .await;
        connection.close(0u32.into(), b"done");
        match read {
            Ok(wire) => HelloFetch::Got {
                build: clamp_build(&wire.build),
                caps: wire
                    .caps
                    .into_iter()
                    .filter(|cap| KNOWN_CAPS.contains(&cap.as_str()))
                    .collect(),
            },
            Err(cause) => HelloFetch::Failed(cause),
        }
    };
    match tokio::time::timeout(HELLO_WAIT, exchange).await {
        Ok(result) => result,
        Err(_) => HelloFetch::Failed("timeout"),
    }
}

struct Entry {
    hello: PeerHello,
    at: Instant,
}

/// The per-peer answers for the life of the mesh, the attempts in flight, and the last result
/// logged per peer (a line is written only when it changes).
#[derive(Default)]
pub(crate) struct HelloBook {
    entries: Mutex<HashMap<[u8; 32], Entry>>,
    inflight: Mutex<HashSet<[u8; 32]>>,
    /// When the last attempt failed per peer: a sleeping device is not asked again for a while.
    failed: Mutex<HashMap<[u8; 32], Instant>>,
    logged: Mutex<HashMap<[u8; 32], String>>,
}

impl HelloBook {
    /// The cached answer; `Unknown` when none, and for a `Legacy` older than ten minutes (it is
    /// asked again; a peer that upgraded must not stay "older").
    pub(crate) fn cached(&self, peer: &[u8; 32]) -> PeerHello {
        self.entries
            .lock()
            .ok()
            .and_then(|map| {
                map.get(peer).map(|e| match &e.hello {
                    PeerHello::Legacy if e.at.elapsed() >= HELLO_REFRESH => PeerHello::Unknown,
                    PeerHello::Known { .. } if e.at.elapsed() >= HELLO_KNOWN_LIFE => {
                        PeerHello::Unknown
                    }
                    other => other.clone(),
                })
            })
            .unwrap_or(PeerHello::Unknown)
    }

    /// Forget every `Legacy` answer (the network changed, or the peer may have restarted on a
    /// current build): the next send asks again.
    pub(crate) fn clear_legacy(&self) {
        if let Ok(mut map) = self.entries.lock() {
            map.retain(|_, e| e.hello != PeerHello::Legacy);
        }
    }

    /// True when there is no answer yet or it is older than ten minutes (and the last attempt did
    /// not fail in the last 30 seconds).
    pub(crate) fn needs_refresh(&self, peer: &[u8; 32]) -> bool {
        if self
            .failed
            .lock()
            .ok()
            .and_then(|map| map.get(peer).map(|at| at.elapsed() < FAILED_BACKOFF))
            .unwrap_or(false)
        {
            return false;
        }
        self.entries
            .lock()
            .ok()
            .and_then(|map| map.get(peer).map(|e| e.at.elapsed() >= HELLO_REFRESH))
            .unwrap_or(true)
    }

    /// Claim the single in-flight attempt for `peer`; false when one is already running.
    pub(crate) fn begin(&self, peer: &[u8; 32]) -> bool {
        self.inflight
            .lock()
            .map(|mut set| set.insert(*peer))
            .unwrap_or(false)
    }

    pub(crate) fn end(&self, peer: &[u8; 32]) {
        if let Ok(mut set) = self.inflight.lock() {
            set.remove(peer);
        }
    }

    /// Store an attempt's outcome (`Got` and `Legacy` are cached, `Failed` is not: the old answer
    /// stays) and report a change to the observer. Returns what the caller should treat the peer as.
    pub(crate) fn record(&self, peer: &[u8; 32], fetched: HelloFetch) -> PeerHello {
        let (hello, cause) = match fetched {
            HelloFetch::Got { build, caps } => (PeerHello::Known { build, caps }, ""),
            HelloFetch::Legacy => (PeerHello::Legacy, ""),
            HelloFetch::Failed(cause) => (PeerHello::Unknown, cause),
        };
        if let Ok(mut failed) = self.failed.lock() {
            if hello == PeerHello::Unknown {
                failed.insert(*peer, Instant::now());
            } else {
                failed.remove(peer);
            }
        }
        let kept = if hello == PeerHello::Unknown {
            self.cached(peer)
        } else {
            if let Ok(mut map) = self.entries.lock() {
                map.insert(
                    *peer,
                    Entry {
                        hello: hello.clone(),
                        at: Instant::now(),
                    },
                );
            }
            hello.clone()
        };
        // A failed refresh keeps the cached answer and is not a change worth a line.
        if hello == PeerHello::Unknown && kept != PeerHello::Unknown {
            return kept;
        }
        let (result, build, caps) = match &hello {
            PeerHello::Known { build, caps } => ("known", build.clone(), caps.clone()),
            PeerHello::Legacy => ("legacy", String::new(), Vec::new()),
            PeerHello::Unknown => ("unknown", String::new(), Vec::new()),
        };
        let key = format!("{result}|{build}|{}", caps.join(","));
        let changed = self
            .logged
            .lock()
            .map(|mut map| map.insert(*peer, key.clone()).as_deref() != Some(key.as_str()))
            .unwrap_or(false);
        if changed && let Some(observer) = HELLO_OBSERVER.get() {
            observer(&HelloEvent {
                peer: *peer,
                result,
                build,
                caps,
                cause,
            });
        }
        kept
    }
}

/// Start a background hello for `peer` unless one is cached fresh or already running. Never
/// blocks the caller (Pulse #560: a dial must not wait for the hello).
pub(crate) fn spawn_hello(
    rt: &tokio::runtime::Handle,
    book: &Arc<HelloBook>,
    endpoint: &Endpoint,
    peer: [u8; 32],
    addr: EndpointAddr,
) {
    if !book.needs_refresh(&peer) || !book.begin(&peer) {
        return;
    }
    // The claim goes into its guard at once (M8): a panic in `spawn` itself, or a task dropped
    // before it ever ran, releases it too, never only the task's own exits. A leaked slot would
    // stop this peer from ever being asked again.
    let slot = InflightSlot {
        book: book.clone(),
        peer,
    };
    let book = book.clone();
    let endpoint = endpoint.clone();
    rt.spawn(async move {
        let _slot = slot;
        let fetched = fetch_hello(&endpoint, addr).await;
        book.record(&peer, fetched);
    });
}

/// Holds one peer's in-flight claim and gives it back when dropped, unwinding included.
struct InflightSlot {
    book: Arc<HelloBook>,
    peer: [u8; 32],
}

impl Drop for InflightSlot {
    fn drop(&mut self) {
        self.book.end(&self.peer);
    }
}
