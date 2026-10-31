use crate::contract::{new_context, Caller};
pub use crate::contract::{CallContext, Emit, Handler};
use crate::discovery::{record_process_matches, try_lock_exclusive, LockedFile};
use crate::error::{ServiceError, ERROR_RESPONSE_TOO_LARGE};
use crate::record::*;
use crate::suite::{make_private_dir, valid_service_name, Suite, SuiteError};
use crate::takeover_watch::{self, Bell, Wake, TAKEOVER_CHECK_INTERVAL};
use crate::transport::{
    self, pid_alive, probe_endpoint, try_endpoint_for, Accepted, Conn, Listener,
    DEFAULT_MAX_PIPE_INSTANCES,
};
use rightkit_sidecar::{read_json_line, write_json_line, FrameError};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufReader, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

pub const DEFAULT_FRAME_LIMIT: usize = 16 * 1024 * 1024;

/// Default for [`HostOptions::write_timeout`]: how long one frame write plus flush to a client may
/// take before the host closes that connection. Matches the client's default call deadline.
pub const DEFAULT_WRITE_TIMEOUT: Duration = Duration::from_secs(120);

/// Default for [`HostOptions::handshake_timeout`]: how long a new connection may take to complete
/// `hello` before the host closes it. A healthy client sends `hello` at once, so this only
/// ever cuts off a connection that connected and went quiet.
pub const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// Default for [`HostOptions::reserved_slots`].
pub const DEFAULT_RESERVED_SLOTS: usize = 2;

/// Content-free lifecycle events for the host's journal.
#[derive(Debug, Clone, PartialEq)]
pub enum ServiceEvent {
    Started {
        service: String,
        endpoint: String,
        rank: u8,
    },
    ConnectionOpened {
        conn: u64,
        app: String,
    },
    ConnectionRejected {
        app: String,
        reason: String,
    },
    /// A connection was closed unread because the OS reported its peer is not the current
    /// user (or could not be identified). `reason` is the typed peer error: a uid or pid,
    /// never request content.
    PeerRejected {
        reason: String,
    },
    /// The Unix connection table ([`HostOptions::max_connections`]) is full and a connection was
    /// refused. Emitted once per fill: on the first refusal after the table became full, and again
    /// only after the live count has dropped below the limit and the table has refilled. Counts and
    /// a pid only, never request content, so a journal can say what was holding the table without
    /// reading anything from it.
    ///
    /// * `limit` is [`HostOptions::max_connections`] and `reserved` is
    ///   [`HostOptions::reserved_slots`].
    /// * `pre_handshake` is the admitted connections (reserved slots included) that have not
    ///   completed `hello`: a table full of these is stalled clients, not load.
    /// * `attached` is the live connections that completed `hello`.
    /// * `peer_pid` is the process id the kernel reports for the refused peer, when it does.
    TableFull {
        limit: usize,
        reserved: usize,
        pre_handshake: usize,
        attached: usize,
        peer_pid: Option<u32>,
    },
    Denied {
        conn: u64,
        app: String,
        method: String,
    },
    HandoffRequested {
        by_role: String,
        by_rank: u8,
    },
    /// A handler's result (`kind` "response") or an emitted event (`kind` "event") was larger
    /// than the frame limit and was not sent. For a response the client received the typed
    /// [`ERROR_RESPONSE_TOO_LARGE`](crate::ERROR_RESPONSE_TOO_LARGE) error instead.
    FrameTooLarge {
        conn: u64,
        method: String,
        kind: &'static str,
        limit: usize,
    },
    Released,
}

/// Callback for [`ServiceEvent`]s.
pub type EventFn = Arc<dyn Fn(&ServiceEvent) + Send + Sync>;

pub struct ServiceConfig {
    pub service: String,
    pub role: String,
    pub rank: u8,
    pub version: String,
    pub allowlist: Allowlist,
    pub frame_limit: usize,
    /// Deadline for election, endpoint publication, or a higher-ranked takeover.
    pub takeover_timeout: Duration,
    /// How long the incumbent lets in-flight requests reach a boundary.
    pub handoff_grace: Duration,
    pub on_event: Option<EventFn>,
    /// Role -> rank for peers that publish a `role` but no `rank` (HeardRight's
    /// `service.json` and `takeover.json`). A peer whose rank cannot be
    /// resolved is never taken over from and never obliges a handoff.
    pub known_roles: std::collections::BTreeMap<String, u8>,
    /// Explicit endpoint (a compatibility convention such as HeardRight's
    /// `<service_dir>/speech.sock`); default is [`crate::endpoint_for`].
    pub endpoint: Option<String>,
    /// A per-run secret every frame must carry; see [`ServiceConfig::private_secret`].
    pub private_secret: Option<crate::private::PrivateSecret>,
}

impl ServiceConfig {
    pub fn new(service: &str, role: &str, rank: u8, version: &str, allowlist: Allowlist) -> Self {
        Self {
            service: service.into(),
            role: role.into(),
            rank,
            version: version.into(),
            allowlist,
            frame_limit: DEFAULT_FRAME_LIMIT,
            takeover_timeout: Duration::from_secs(60),
            handoff_grace: Duration::from_secs(30),
            on_event: None,
            known_roles: Default::default(),
            endpoint: None,
            private_secret: None,
        }
    }

    /// Require `secret` on the `hello` frame and on every `request` frame (a top-level `secret`
    /// field, checked in constant time before any handler runs). A frame without it, or with a
    /// wrong one, is answered `unauthenticated` and the connection is closed. This is the host
    /// half of the private per-run child service pattern; see the `private` module.
    pub fn private_secret(mut self, secret: crate::private::PrivateSecret) -> Self {
        self.private_secret = Some(secret);
        self
    }

    /// Take the allowlist from a [`MethodTable`](crate::contract::MethodTable), the single source
    /// of method policy.
    pub fn with_methods(mut self, table: &crate::contract::MethodTable) -> Self {
        self.allowlist = table.allowlist();
        self
    }
}

/// Host-side transport options, passed to [`host_with_options`]. `HostOptions::default()`
/// is what [`host`] uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostOptions {
    max_pipe_instances: u32,
    write_timeout: Option<Duration>,
    max_message: usize,
    max_connections: usize,
    reserved_slots: usize,
    supervisor: SupervisorPid,
    handshake_timeout: Option<Duration>,
}

/// Default for [`HostOptions::max_connections`].
pub const DEFAULT_MAX_CONNECTIONS: usize = 64;

/// Which peer may use the reserved connection slots ([`HostOptions::supervisor_pid`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SupervisorPid {
    /// The host process's parent, read when the host starts (the default).
    Parent,
    Pid(u32),
    Nobody,
}

impl Default for HostOptions {
    fn default() -> Self {
        Self {
            max_pipe_instances: DEFAULT_MAX_PIPE_INSTANCES,
            write_timeout: Some(DEFAULT_WRITE_TIMEOUT),
            max_message: crate::chunk::DEFAULT_MAX_MESSAGE,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            reserved_slots: DEFAULT_RESERVED_SLOTS,
            supervisor: SupervisorPid::Parent,
            handshake_timeout: Some(DEFAULT_HANDSHAKE_TIMEOUT),
        }
    }
}

impl HostOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Cap on simultaneous instances of the Windows named pipe: live connections plus the
    /// one instance waiting for the next client. A client that arrives when the cap is
    /// reached gets "pipe busy" and retries, so a flood of connections cannot grow the
    /// host without bound. Default [`DEFAULT_MAX_PIPE_INSTANCES`] (16); clamped to
    /// `1..=254` (255 is "unlimited" to Win32 and is never used). Ignored on Unix, where
    /// there is no pipe.
    #[must_use]
    pub fn max_pipe_instances(mut self, n: u32) -> Self {
        self.max_pipe_instances = n.clamp(1, 254);
        self
    }

    /// The effective (clamped) cap.
    pub fn get_max_pipe_instances(&self) -> u32 {
        self.max_pipe_instances
    }

    /// Longest one frame (hello, event, or response) may take to write and flush to a client.
    /// A client that stops reading fills the transport buffer and would otherwise block the
    /// writing thread, and the connection with it, forever; when the bound passes the host closes
    /// that connection (the client sees `connection_closed`, never a partial frame it could
    /// mistake for a reply), and the connection's `connection_closed` handler hook runs. The
    /// bound is per frame, so a slow but progressing reader of a stream of events is not cut off
    /// unless a single frame stalls. Default [`DEFAULT_WRITE_TIMEOUT`] (120 s); `None` restores
    /// the earlier unbounded write. A zero duration is raised to 1 ms.
    #[must_use]
    pub fn write_timeout(mut self, bound: Option<Duration>) -> Self {
        self.write_timeout = bound.map(|d| d.max(Duration::from_millis(1)));
        self
    }

    /// The effective write bound (`None` = unbounded).
    pub fn get_write_timeout(&self) -> Option<Duration> {
        self.write_timeout
    }

    /// Largest message the host assembles from chunks, or hands out in chunks (see the `chunk`
    /// module). The frame limit ([`ServiceConfig::frame_limit`]) stays the cap on any one frame;
    /// this is the second, separate cap on a whole chunked request or result, held in memory per
    /// connection. Default `0`: chunking is **off**, the host refuses `rk.upload` and never
    /// announces a chunked result (it was 256 MiB earlier in the unpublished 0.2.7; see issue #323).
    /// A service that takes large calls opts in with a size (see
    /// [`SUGGESTED_MAX_MESSAGE`](crate::chunk::SUGGESTED_MAX_MESSAGE)). A connection holds at most one
    /// assembly of at most this many bytes; the first piece is authorised before it is buffered,
    /// and the assembled text is bounded in nesting and value count before it is parsed.
    #[must_use]
    pub fn max_message(mut self, bytes: usize) -> Self {
        self.max_message = bytes;
        self
    }

    /// The effective message bound.
    pub fn get_max_message(&self) -> usize {
        self.max_message
    }

    /// Cap on simultaneous connections on Unix (the counterpart of the Windows pipe-instance
    /// cap, which this does not replace there): a connection that arrives over the cap is closed
    /// at once, before any frame is read, so a flood cannot grow the host's threads and buffers
    /// without bound. Default [`DEFAULT_MAX_CONNECTIONS`] (64); at least 1. Unlike Windows "pipe
    /// busy", the refused client sees the connection close rather than a retryable busy status.
    /// Ignored on Windows.
    #[must_use]
    pub fn max_connections(mut self, n: usize) -> Self {
        self.max_connections = n.max(1);
        self
    }

    /// The effective connection cap.
    pub fn get_max_connections(&self) -> usize {
        self.max_connections
    }

    /// Connections the host admits beyond [`max_connections`](Self::max_connections) for the
    /// supervising owner only (see [`supervisor_pid`](Self::supervisor_pid)). A host whose table
    /// is full of stalled or busy clients would otherwise close the supervisor's own health probe
    /// and be judged dead while it is alive; these slots keep the probe able to connect. A
    /// connection from any other peer, or from the supervisor once every reserved slot is in use,
    /// is refused as before. Default [`DEFAULT_RESERVED_SLOTS`] (2); `0` reserves nothing. The
    /// peer's pid only decides who may use the spare capacity: `hello`, the allowlist and the
    /// private secret still gate every connection. Ignored on Windows, where the pipe-instance cap
    /// bounds the table.
    #[must_use]
    pub fn reserved_slots(mut self, n: usize) -> Self {
        self.reserved_slots = n;
        self
    }

    /// The effective number of reserved slots.
    pub fn get_reserved_slots(&self) -> usize {
        self.reserved_slots
    }

    /// The process that may use the reserved slots: the kernel-reported pid of a connecting peer
    /// must equal it. `Some(pid)` names it, `None` lets nobody use them. When this is never
    /// called the supervisor is the host process's parent, read when the host starts, which is
    /// the supervisor of a child it spawned (see the `supervisor` module). Ignored on Windows.
    #[must_use]
    pub fn supervisor_pid(mut self, pid: Option<u32>) -> Self {
        self.supervisor = match pid {
            Some(pid) => SupervisorPid::Pid(pid),
            None => SupervisorPid::Nobody,
        };
        self
    }

    /// The pid that may use the reserved slots, as the host would resolve it now. `None` when
    /// nobody may (or, off Unix, when there is no parent to name).
    pub fn get_supervisor_pid(&self) -> Option<u32> {
        match self.supervisor {
            SupervisorPid::Pid(pid) => Some(pid),
            SupervisorPid::Nobody => None,
            #[cfg(unix)]
            SupervisorPid::Parent => Some(std::os::unix::process::parent_id()),
            #[cfg(not(unix))]
            SupervisorPid::Parent => None,
        }
    }

    /// Longest a new connection may take to complete `hello`. A connection that connects and then
    /// sends nothing (or too little) holds a table slot and a thread; when the bound passes the
    /// host closes it (the client sees the connection close) and records a
    /// [`ServiceEvent::ConnectionRejected`] with an empty app and the reason
    /// `handshake deadline <n> ms passed`. The bound ends when `hello` is accepted, so it never
    /// limits an established connection, whose frames are bounded by
    /// [`write_timeout`](Self::write_timeout) and the client's own deadline. Default
    /// [`DEFAULT_HANDSHAKE_TIMEOUT`] (5 s); `None` restores the earlier unbounded wait. A zero
    /// duration is raised to 1 ms. Applies on Unix and Windows.
    #[must_use]
    pub fn handshake_timeout(mut self, bound: Option<Duration>) -> Self {
        self.handshake_timeout = bound.map(|d| d.max(Duration::from_millis(1)));
        self
    }

    /// The effective handshake bound (`None` = unbounded).
    pub fn get_handshake_timeout(&self) -> Option<Duration> {
        self.handshake_timeout
    }
}

#[derive(Debug)]
pub enum HostError {
    /// A live host of equal or better rank already serves this service.
    Held(ServiceRecord),
    /// The incumbent did not release within `takeover_timeout`.
    TakeoverTimeout,
    InvalidName(String),
    Suite(SuiteError),
    Io(std::io::Error),
    /// No socket path within the OS limit can name this service (typed; carries path, length, limit).
    Endpoint(crate::transport::EndpointError),
}
impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostError::Held(r) => write!(
                f,
                "{} is already hosted by {} (rank {}, pid {})",
                r.service, r.role, r.rank, r.pid
            ),
            HostError::TakeoverTimeout => write!(f, "incumbent host did not release in time"),
            HostError::InvalidName(s) => write!(f, "invalid service name {s:?}"),
            HostError::Suite(e) => write!(f, "{e}"),
            HostError::Io(e) => write!(f, "{e}"),
            HostError::Endpoint(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for HostError {}
impl From<std::io::Error> for HostError {
    fn from(e: std::io::Error) -> Self {
        HostError::Io(e)
    }
}
impl From<SuiteError> for HostError {
    fn from(e: SuiteError) -> Self {
        HostError::Suite(e)
    }
}

fn resolve_rank(cfg: &ServiceConfig, role: &str, rank: u8) -> u8 {
    crate::discovery::resolve_rank(role, rank, &cfg.known_roles).unwrap_or(RANK_UNKNOWN)
}

struct Shared {
    suite: Suite,
    cfg: ServiceConfig,
    handler: Arc<dyn Handler>,
    listener: Listener,
    // Held until endpoint cleanup completes; kernel releases it on process death.
    election: Mutex<Option<LockedFile>>,
    record: ServiceRecord,
    stop: AtomicBool,
    handoff: Arc<AtomicBool>,
    released: Mutex<bool>,
    released_cv: Condvar,
    inflight: AtomicUsize,
    next_conn: AtomicU64,
    conns: Mutex<HashMap<u64, Arc<dyn Fn() + Send + Sync>>>,
    /// What the takeover monitor blocks on; `release` stops it.
    bell: Arc<Bell>,
    watch: WatchTuning,
    write_timeout: Option<Duration>,
    max_message: usize,
    max_connections: usize,
    /// Connections being served (Unix cap). Reserved-slot connections are counted apart.
    live_conns: Arc<AtomicUsize>,
    /// Slots kept for the supervisor beyond `max_connections`, and the pid that may use them.
    reserved_slots: usize,
    supervisor: Option<u32>,
    /// Connections being served in a reserved slot.
    reserved_conns: Arc<AtomicUsize>,
    /// Admitted connections that have not completed `hello`.
    pre_handshake: Arc<AtomicUsize>,
    handshake_timeout: Option<Duration>,
    /// Failed-authentication limiter (issue #504), shared by every connection.
    fails: Mutex<crate::fail_limit::FailLimiter>,
}

/// Forgets a connection's failure-limiter entry when its thread ends, however it ends.
struct ForgetFails {
    s: Arc<Shared>,
    conn: u64,
}

impl Drop for ForgetFails {
    fn drop(&mut self) {
        self.s
            .fails
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .forget(self.conn, Instant::now());
    }
}

/// Decrements the live-connection count when a connection thread ends, however it ends.
struct ConnSlot(Arc<AtomicUsize>);

impl Drop for ConnSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Counts a connection as not yet through `hello` from admission until it completes one or ends.
/// The count goes down exactly once, whichever comes first, so it never drifts.
struct PreHandshake {
    count: Arc<AtomicUsize>,
    pending: bool,
}

impl PreHandshake {
    fn enter(count: &Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self {
            count: count.clone(),
            pending: true,
        }
    }

    /// `hello` succeeded. A second call, or the later drop, does nothing.
    fn settle(&mut self) {
        if std::mem::take(&mut self.pending) {
            self.count.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl Drop for PreHandshake {
    fn drop(&mut self) {
        self.settle();
    }
}

/// A deadline on one step of a connection, kept by a watcher thread. The step arms it with
/// [`set`](Self::set) and disarms it when done; if the armed deadline passes, the watcher runs
/// `expired` once. It bounds the handshake (armed before the `hello` read, disarmed when `hello`
/// succeeds) and each frame write (armed before a write+flush, disarmed after). Both expire by
/// closing the connection through the transport `shutdown`, which wakes a blocked Unix read or
/// write and cancels pending Windows pipe I/O. Only the oldest unfinished write is timed, so a
/// stream of writes that each finish in time is never cut.
struct DeadlineWatch {
    state: Mutex<WatchState>,
    cv: Condvar,
}

struct WatchState {
    /// Deadline of the operation in progress.
    until: Option<Instant>,
    done: bool,
}

struct FinishOnDrop(Arc<DeadlineWatch>);
impl Drop for FinishOnDrop {
    fn drop(&mut self) {
        self.0.finish();
    }
}

impl DeadlineWatch {
    fn start(expired: Arc<dyn Fn() + Send + Sync>) -> Arc<DeadlineWatch> {
        let w = Arc::new(DeadlineWatch {
            state: Mutex::new(WatchState {
                until: None,
                done: false,
            }),
            cv: Condvar::new(),
        });
        let watcher = w.clone();
        std::thread::spawn(move || {
            let mut g = watcher.state.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if g.done {
                    return;
                }
                match g.until {
                    None => g = watcher.cv.wait(g).unwrap_or_else(|e| e.into_inner()),
                    Some(t) => {
                        let left = t.saturating_duration_since(Instant::now());
                        if left.is_zero() {
                            g.until = None;
                            drop(g);
                            expired();
                            return;
                        }
                        g = watcher
                            .cv
                            .wait_timeout(g, left)
                            .unwrap_or_else(|e| e.into_inner())
                            .0;
                    }
                }
            }
        });
        w
    }
    fn set(&self, until: Option<Instant>) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).until = until;
        self.cv.notify_all();
    }
    fn finish(&self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).done = true;
        self.cv.notify_all();
    }
}

/// How the takeover monitor waits and how it tells that a requester is alive. Production uses
/// [`WatchTuning::PRODUCTION`]; the tests lengthen or shorten the interval and fake the probe.
#[derive(Clone, Copy)]
struct WatchTuning {
    /// How often a request written by another process is looked for.
    interval: Duration,
    /// Is the process that wrote a takeover request still running?
    alive: fn(u32) -> bool,
}

impl WatchTuning {
    const PRODUCTION: WatchTuning = WatchTuning {
        interval: TAKEOVER_CHECK_INTERVAL,
        alive: pid_alive,
    };
}

impl Shared {
    fn event(&self, e: ServiceEvent) {
        if let Some(f) = &self.cfg.on_event {
            f(&e);
        }
    }

    fn release(&self) {
        {
            let mut r = self.released.lock().unwrap_or_else(|e| e.into_inner());
            if *r {
                return;
            }
            *r = true;
        }
        self.stop.store(true, Ordering::SeqCst);
        // The takeover monitor is blocked on its bell, not sleeping: wake it so it leaves now.
        self.bell.stop();
        // Remove the record first so clients stop discovering this host, but only if it is ours.
        if read_record(&self.suite, &self.cfg.service).is_some_and(|r| r.pid == self.record.pid) {
            let _ = std::fs::remove_file(record_path(&self.suite, &self.cfg.service));
        }
        self.listener.wake();
        self.listener.cleanup();
        for (_, close) in self.conns.lock().unwrap_or_else(|e| e.into_inner()).drain() {
            close();
        }
        self.election
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        self.event(ServiceEvent::Released);
        self.released_cv.notify_all();
    }
}

/// Runtime figures of a running host, computed on call by [`Hosted::status`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ServiceStatus {
    /// Connections open now.
    pub connections: usize,
    /// Requests being served now (the queue the handoff waits on).
    pub inflight: usize,
    /// A takeover was accepted and the host is draining.
    pub handoff_requested: bool,
    /// The host has released the service.
    pub released: bool,
    /// Resident memory, CPU time and I/O counters of the whole process (the host runs in it), or
    /// `None` when the host cannot report them. See `rightkit_child_process::ProcessUsage`.
    pub process: Option<rightkit_child_process::ProcessUsage>,
}

/// A running host. Dropping it releases the service.
pub struct Hosted {
    shared: Arc<Shared>,
}

impl Hosted {
    pub fn record(&self) -> &ServiceRecord {
        &self.shared.record
    }
    pub fn handoff_requested(&self) -> bool {
        self.shared.handoff.load(Ordering::SeqCst)
    }
    pub fn inflight(&self) -> usize {
        self.shared.inflight.load(Ordering::SeqCst)
    }
    /// Connections, in-flight requests and process resource usage, read now. No polling thread
    /// runs for this.
    pub fn status(&self) -> ServiceStatus {
        ServiceStatus {
            connections: self
                .shared
                .conns
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .len(),
            inflight: self.shared.inflight.load(Ordering::SeqCst),
            handoff_requested: self.shared.handoff.load(Ordering::SeqCst),
            released: *self
                .shared
                .released
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
            process: rightkit_child_process::process_usage().ok(),
        }
    }
    /// Block until the host has released (clean shutdown or takeover).
    pub fn wait_released(&self, timeout: Duration) -> bool {
        let g = self
            .shared
            .released
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (g, _) = self
            .shared
            .released_cv
            .wait_timeout_while(g, timeout, |r| !*r)
            .unwrap_or_else(|e| e.into_inner());
        *g
    }
    pub fn shutdown(&self) {
        self.shared.release();
    }
}

impl Drop for Hosted {
    fn drop(&mut self) {
        self.shared.release();
    }
}

/// Host `cfg.service` on `suite`: discover, rank, take over if outranking,
/// bind the current-user-only endpoint, publish `service.json`. Chunked calls (`rk.upload`) are
/// off unless the options set [`HostOptions::max_message`].
// `HostError::Held` carries the incumbent's record by value so callers can match on it directly;
// this runs once per host start, so the larger Err is not a hot-path cost.
#[allow(clippy::result_large_err)]
pub fn host(
    suite: &Suite,
    cfg: ServiceConfig,
    handler: Arc<dyn Handler>,
) -> Result<Hosted, HostError> {
    host_with_options(suite, cfg, handler, HostOptions::default())
}

/// [`host`] with explicit [`HostOptions`].
#[allow(clippy::result_large_err)]
pub fn host_with_options(
    suite: &Suite,
    cfg: ServiceConfig,
    handler: Arc<dyn Handler>,
    options: HostOptions,
) -> Result<Hosted, HostError> {
    start_host(suite, cfg, handler, options, WatchTuning::PRODUCTION)
}

#[allow(clippy::result_large_err)]
fn start_host(
    suite: &Suite,
    cfg: ServiceConfig,
    handler: Arc<dyn Handler>,
    options: HostOptions,
    watch: WatchTuning,
) -> Result<Hosted, HostError> {
    if !valid_service_name(&cfg.service) {
        return Err(HostError::InvalidName(cfg.service.clone()));
    }
    let dir = suite.service_dir(&cfg.service);
    make_private_dir(&dir)?;
    let me = std::process::id();
    let deadline = Instant::now() + cfg.takeover_timeout;
    let mut requested = false;
    let lock_path = dir.join("election.lock");
    let endpoint = match cfg.endpoint.clone() {
        Some(e) => e,
        None => try_endpoint_for(suite, &cfg.service).map_err(HostError::Endpoint)?,
    };
    let mut backoff = Duration::from_millis(5);
    let (listener, election) = loop {
        // Never unlink the lock file: every contender must lock the same inode.
        // A loser can observe the incumbent, but only the owner can discard stale
        // discovery or bind. Retain ownership through publication and shutdown.
        let election = try_lock_exclusive(&lock_path)?;
        // `Err` is "unknown", not "no record": it falls to the arm that waits, so a record that
        // cannot be read for an instant is never removed and no listener is bound beside it.
        let current = try_read_record(suite, &cfg.service);
        let unreadable = current
            .as_ref()
            .err()
            .map(|e| std::io::Error::new(e.kind(), e.to_string()));
        match current {
            Ok(Some(rec)) if record_process_matches(&rec) && probe_endpoint(&rec.endpoint) => {
                let their = resolve_rank(&cfg, &rec.role, rec.rank);
                if their == RANK_UNKNOWN || cfg.rank >= their {
                    return Err(HostError::Held(rec));
                }
                if !requested {
                    let t = TakeoverRequest {
                        role: cfg.role.clone(),
                        rank: cfg.rank,
                        pid: me,
                        requested_at_ms: now_ms(),
                    };
                    let request_path = takeover_path(suite, &cfg.service);
                    write_atomic(&request_path, &serde_json::to_vec(&t).unwrap_or_default())?;
                    // A monitor in this process (another host of this service) looks now.
                    takeover_watch::poke(&request_path);
                    requested = true;
                }
            }
            Ok(_) if election.is_some() => {
                // PID generation and endpoint checks rejected the old record.
                // Only the elected process may remove it or a dead Unix socket.
                let _ = std::fs::remove_file(record_path(suite, &cfg.service));
                match Listener::bind(&endpoint, options.max_pipe_instances) {
                    Ok(listener) => break (listener, election.unwrap()),
                    // A legacy host may have bound without publishing yet, or a
                    // Windows incumbent may still be closing its pipe handles.
                    Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {}
                    Err(e) => return Err(HostError::Io(e)),
                }
            }
            // Winner is between lock acquisition and record publication, or the record could
            // not be read (see above).
            _ => {}
        }
        drop(election);
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            clear_takeover_if_owner(suite, &cfg.service, me);
            // Still unable to read the record at the deadline: say so rather than report a
            // timeout that blames the incumbent.
            return Err(match unreadable {
                Some(e) => HostError::Io(e),
                None => HostError::TakeoverTimeout,
            });
        }
        std::thread::sleep(backoff.min(remaining));
        backoff = (backoff * 2).min(Duration::from_millis(50));
    };
    let record = ServiceRecord {
        protocol_major: PROTOCOL_MAJOR,
        protocol_minor: PROTOCOL_MINOR,
        service: cfg.service.clone(),
        role: cfg.role.clone(),
        rank: cfg.rank,
        pid: me,
        endpoint: endpoint.clone(),
        version: cfg.version.clone(),
        engine_version: cfg.version.clone(),
        started_ms: now_ms(),
        process_started_ms: crate::discovery::own_start_ms().unwrap_or(0),
    };
    if let Err(e) = write_atomic(
        &record_path(suite, &cfg.service),
        &serde_json::to_vec_pretty(&record).unwrap_or_default(),
    ) {
        listener.cleanup();
        return Err(HostError::Io(e));
    }
    if read_takeover(suite, &cfg.service).is_some_and(|t| t.pid == me) {
        let _ = std::fs::remove_file(takeover_path(suite, &cfg.service));
    }
    let shared = Arc::new(Shared {
        suite: suite.clone(),
        cfg,
        handler,
        listener,
        election: Mutex::new(Some(election)),
        record,
        stop: AtomicBool::new(false),
        handoff: Arc::new(AtomicBool::new(false)),
        released: Mutex::new(false),
        released_cv: Condvar::new(),
        inflight: AtomicUsize::new(0),
        next_conn: AtomicU64::new(1),
        conns: Mutex::new(HashMap::new()),
        bell: Arc::new(Bell::new()),
        watch,
        write_timeout: options.write_timeout,
        max_message: options.max_message,
        max_connections: options.max_connections,
        live_conns: Arc::new(AtomicUsize::new(0)),
        reserved_slots: options.reserved_slots,
        supervisor: options.get_supervisor_pid(),
        reserved_conns: Arc::new(AtomicUsize::new(0)),
        pre_handshake: Arc::new(AtomicUsize::new(0)),
        handshake_timeout: options.handshake_timeout,
        fails: Mutex::new(crate::fail_limit::FailLimiter::new()),
    });
    takeover_watch::register(
        takeover_path(&shared.suite, &shared.cfg.service),
        &shared.bell,
    );
    shared.event(ServiceEvent::Started {
        service: shared.cfg.service.clone(),
        endpoint,
        rank: shared.cfg.rank,
    });
    let s = shared.clone();
    if let Err(e) = std::thread::Builder::new()
        .name("rk-svc-accept".into())
        .spawn(move || accept_loop(s))
    {
        shared.release();
        return Err(HostError::Io(e));
    }
    let s = shared.clone();
    if let Err(e) = std::thread::Builder::new()
        .name("rk-svc-takeover".into())
        .spawn(move || takeover_monitor(s))
    {
        shared.release();
        return Err(HostError::Io(e));
    }
    Ok(Hosted { shared })
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A reserved slot for `peer`: only the supervising owner may take one, and only while one is
/// free. The pid is the kernel's answer for the connecting process, so this decides who gets spare
/// capacity, not who is authenticated (`hello` and the private secret still decide that).
fn reserved_slot(s: &Shared, peer: &crate::PeerIdentity) -> Option<ConnSlot> {
    if !matches!((peer.pid, s.supervisor), (Some(pid), Some(owner)) if pid == owner) {
        return None;
    }
    let slot = ConnSlot(s.reserved_conns.clone());
    let used = s.reserved_conns.fetch_add(1, Ordering::SeqCst) + 1;
    (used <= s.reserved_slots).then_some(slot)
}

fn accept_loop(s: Arc<Shared>) {
    // Whether `TableFull` was already reported for the table's current fill. Only this thread
    // admits connections, so only this thread reads or writes it.
    let mut full_reported = false;
    while !s.stop.load(Ordering::SeqCst) {
        match s.listener.accept() {
            Ok(Accepted::Conn(conn)) => {
                if s.stop.load(Ordering::SeqCst) {
                    break;
                }
                // Count before spawning so a burst cannot overshoot the cap. Windows is bounded
                // by the pipe-instance cap, so only Unix refuses here.
                let table = ConnSlot(s.live_conns.clone());
                let live = s.live_conns.fetch_add(1, Ordering::SeqCst) + 1;
                let slot = if !cfg!(unix) || live <= s.max_connections {
                    // Room again: the next time the table fills is a new episode.
                    full_reported = false;
                    table
                } else {
                    drop(table);
                    match reserved_slot(&s, &conn.peer) {
                        Some(slot) => slot,
                        None => {
                            (conn.shutdown)();
                            s.event(ServiceEvent::PeerRejected {
                                reason: format!(
                                    "connection limit of {} reached",
                                    s.max_connections
                                ),
                            });
                            if !std::mem::replace(&mut full_reported, true) {
                                s.event(ServiceEvent::TableFull {
                                    limit: s.max_connections,
                                    reserved: s.reserved_slots,
                                    pre_handshake: s.pre_handshake.load(Ordering::SeqCst),
                                    attached: s
                                        .conns
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .len(),
                                    peer_pid: conn.peer.pid,
                                });
                            }
                            continue;
                        }
                    }
                };
                let pre = PreHandshake::enter(&s.pre_handshake);
                let s2 = s.clone();
                let _ = std::thread::Builder::new()
                    .name("rk-svc-conn".into())
                    .spawn(move || {
                        let _slot = slot;
                        serve_conn(s2, conn, pre)
                    });
            }
            // Wrong user (or unidentifiable): already closed unread. Log it, except for the
            // shutdown wake-up, which is our own connection.
            Ok(Accepted::Rejected(reason)) => {
                if !s.stop.load(Ordering::SeqCst) {
                    s.event(ServiceEvent::PeerRejected { reason });
                }
            }
            Ok(Accepted::Vanished) => {}
            Err(_) => {
                if s.stop.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// What `takeover.json` looked like when it was last judged: modification time and length.
type FileStamp = (Option<SystemTime>, u64);

fn file_stamp(path: &std::path::Path) -> Option<FileStamp> {
    let md = std::fs::metadata(path).ok()?;
    Some((md.modified().ok(), md.len()))
}

/// The requester's rank when `t` obliges a host of `cfg.rank` (running as `own_pid`) to hand
/// off: a known rank that beats ours, from another live process, and fresh. `None` otherwise.
fn obliging_rank(
    cfg: &ServiceConfig,
    own_pid: u32,
    alive: fn(u32) -> bool,
    t: &TakeoverRequest,
    now: u64,
) -> Option<u8> {
    let max_age = cfg.takeover_timeout.as_millis() as u64;
    let fresh = now.saturating_sub(t.requested_at_ms) <= max_age + 5_000;
    let their = resolve_rank(cfg, &t.role, t.rank);
    let obliges =
        their != RANK_UNKNOWN && their < cfg.rank && t.pid != own_pid && alive(t.pid) && fresh;
    obliges.then_some(their)
}

#[cfg(test)]
struct ExitMark<'a>(&'a Bell);

#[cfg(test)]
impl Drop for ExitMark<'_> {
    fn drop(&mut self) {
        self.0.mark_exited();
    }
}

/// Watch for a takeover request from a higher-ranked live host.
///
/// The thread blocks on `s.bell`: it wakes when the host releases (leave), when this process wrote
/// a takeover request for the service (look now), and otherwise once per `s.watch.interval` to
/// stat `takeover.json`, which is how a request written by another process is found. The file is
/// read only when its modification time or length differs from the one already judged (or on an
/// in-process poke), and a request that was judged not to oblige is not judged again until the
/// file changes. That is sound because the verdict can only turn from "obliges" to "does not" with
/// time (a request goes stale, a requester exits), never the other way.
fn takeover_monitor(s: Arc<Shared>) {
    #[cfg(test)]
    let _exit = ExitMark(&s.bell);
    let path = takeover_path(&s.suite, &s.cfg.service);
    let mut judged: Option<FileStamp> = None;
    let mut look_now = false;
    loop {
        if s.stop.load(Ordering::SeqCst) {
            return;
        }
        let force = std::mem::take(&mut look_now);
        match file_stamp(&path) {
            None => judged = None,
            Some(stamp) if force || judged != Some(stamp) => {
                judged = Some(stamp);
                #[cfg(test)]
                s.bell.note_read();
                match try_read_takeover(&s.suite, &s.cfg.service) {
                    Ok(Some(t)) => {
                        let now = now_ms();
                        let obliging = obliging_rank(&s.cfg, s.record.pid, s.watch.alive, &t, now);
                        if let Some(their) = obliging {
                            hand_off(&s, t, their);
                            return;
                        }
                    }
                    Ok(None) => {}
                    // Could not be read: whether a request exists is unknown, not "none". Do not
                    // mark this stamp as judged, so the next pass (one interval on, no faster)
                    // reads the file again.
                    Err(_) => judged = None,
                }
            }
            Some(_) => {}
        }
        #[cfg(test)]
        s.bell.note_pass();
        match s.bell.wait(s.watch.interval) {
            Wake::Stop => return,
            Wake::Poke => look_now = true,
            Wake::Timeout => {}
        }
    }
}

/// Accept a takeover: stop admitting work, let in-flight requests reach a boundary (up to
/// `handoff_grace`), then release.
fn hand_off(s: &Shared, t: TakeoverRequest, their: u8) {
    s.handoff.store(true, Ordering::SeqCst);
    s.event(ServiceEvent::HandoffRequested {
        by_role: t.role,
        by_rank: their,
    });
    s.handler.on_handoff();
    let until = Instant::now() + s.cfg.handoff_grace;
    while s.inflight.load(Ordering::SeqCst) > 0
        && Instant::now() < until
        && !s.stop.load(Ordering::SeqCst)
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    s.release();
}

fn frame_err(e: &FrameError) -> ServiceError {
    ServiceError::new("bad_frame", e.to_string())
}

fn serve_conn(s: Arc<Shared>, conn: Conn, mut pre: PreHandshake) {
    let Conn {
        reader,
        writer,
        peer_closed,
        shutdown,
        peer,
    } = conn;
    let mut reader = BufReader::new(reader);
    let writer = Arc::new(Mutex::new(writer));
    let limit = s.cfg.frame_limit;
    let bound = s
        .write_timeout
        .map(|d| (d, DeadlineWatch::start(shutdown.clone())));
    // Ends the watcher on every way out of this function, early returns included.
    let _watcher_ends = bound.as_ref().map(|(_, b)| FinishOnDrop(b.clone()));
    // Write one frame. `false` only when the frame is larger than the limit: nothing was
    // written, so the stream is still on a frame boundary. An I/O failure means the peer is
    // gone, which the read loop and the peer watch already report.
    let write = {
        let w = writer.clone();
        let bound = bound.clone();
        move |v: &Value| -> bool {
            let mut w = w.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((d, b)) = &bound {
                b.set(Some(Instant::now() + *d));
            }
            let r = match write_json_line(&mut *w, v, limit) {
                Ok(()) => {
                    let _ = w.flush();
                    true
                }
                Err(FrameError::TooLarge { .. }) => false,
                Err(_) => true,
            };
            if let Some((_, b)) = &bound {
                b.set(None);
            }
            r
        }
    };
    let send = {
        let write = write.clone();
        move |v: &Value| {
            write(v);
        }
    };
    let mut buf = Vec::new();
    // The connection's key for the failure limiter and its events; the same number is its id.
    let conn_key = s.next_conn.fetch_add(1, Ordering::SeqCst);
    let _forget_fails = ForgetFails {
        s: s.clone(),
        conn: conn_key,
    };
    // The handshake deadline, armed before the hello read: a client that connects and then says
    // nothing is closed through `shutdown`, which wakes the read below. Ended when hello succeeds
    // and on every other way out of this function, so a probe or garbage leaks no watcher.
    let handshake = s.handshake_timeout.map(|d| {
        let (s, shutdown) = (s.clone(), shutdown.clone());
        let watch = DeadlineWatch::start(Arc::new(move || {
            s.event(ServiceEvent::ConnectionRejected {
                app: String::new(),
                reason: format!("handshake deadline {} ms passed", d.as_millis()),
            });
            shutdown();
        }));
        watch.set(Some(Instant::now() + d));
        watch
    });
    let _handshake_ends = handshake.as_ref().map(|w| FinishOnDrop(w.clone()));
    // ---- hello
    let hello: Value = match read_json_line(&mut reader, &mut buf, 64 * 1024) {
        Ok(Some(v)) => v,
        _ => return, // probe or garbage
    };
    let app = hello["app"].as_str().unwrap_or("").to_string();
    let reject = |reason: &str, code: &str, retryable: bool| {
        let e = ServiceError {
            code: code.into(),
            message: reason.into(),
            retryable,
            details: None,
        };
        send(&json!({"type": "hello", "ok": false, "error": e.to_value()}));
        s.event(ServiceEvent::ConnectionRejected {
            app: app.clone(),
            reason: reason.into(),
        });
        shutdown();
    };
    if hello["type"] != "hello" || app.is_empty() {
        return reject(
            "first frame must be hello with an app label",
            "bad_hello",
            false,
        );
    }
    if let Some(secret) = &s.cfg.private_secret {
        // The limiter sees only a secret that failed to verify: a correct hello is never refused
        // because other callers failed (issue #504).
        if !secret.verify(hello["secret"].as_str().unwrap_or("")) {
            let now = Instant::now();
            let limited = s
                .fails
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .refuses_unverified(conn_key, now);
            if limited {
                return reject(
                    "too many failed authentications; wait before reconnecting",
                    crate::error::ERROR_RATE_LIMITED,
                    false,
                );
            }
            s.fails
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .record_failure(conn_key, now);
            return reject(
                "missing or wrong secret",
                crate::error::ERROR_UNAUTHENTICATED,
                false,
            );
        }
    }
    if hello["protocol_major"].as_u64() != Some(u64::from(PROTOCOL_MAJOR)) {
        return reject("protocol major mismatch", "protocol_mismatch", false);
    }
    if s.handoff.load(Ordering::SeqCst) {
        return reject(
            "host is handing off",
            crate::error::ERROR_HOST_HANDOFF,
            true,
        );
    }
    if !s.cfg.allowlist.app_known(&app) {
        return reject(
            "app is not on the service allowlist",
            crate::error::ERROR_NOT_ALLOWED,
            false,
        );
    }
    let id = conn_key;
    s.conns
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id, shutdown.clone());
    // Hello succeeded: the connection is attached, so it leaves the pre-handshake count and the
    // deadline watcher ends now rather than idling for the life of the connection.
    pre.settle();
    if let Some(watch) = &handshake {
        watch.finish();
    }
    s.event(ServiceEvent::ConnectionOpened {
        conn: id,
        app: app.clone(),
    });
    send(&json!({
        "type": "hello", "ok": true, "service": s.cfg.service, "role": s.cfg.role, "rank": s.cfg.rank,
        "version": s.cfg.version, "protocol_major": PROTOCOL_MAJOR, "protocol_minor": PROTOCOL_MINOR,
        "methods": s.cfg.allowlist.methods_for(&app),
        "frame_limit": limit, "max_message": s.max_message,
        "chunk_bytes": crate::chunk::chunk_bytes_for(limit),
    }));

    let caller = Caller::local_app(app.clone(), peer.clone());
    let closed_notified = Arc::new(AtomicBool::new(false));
    let notify_closed = {
        let (s, flag) = (s.clone(), closed_notified.clone());
        move || {
            if !flag.swap(true, Ordering::SeqCst) {
                s.handler.connection_closed(id);
            }
        }
    };
    // Chunked transfers (see the `chunk` module): at most one of each per connection, dropped
    // with it.
    let chunk_bytes = crate::chunk::chunk_bytes_for(limit);
    let mut uploads = crate::chunk::Uploads::default();
    let mut download: Option<crate::chunk::Download> = None;
    let mut downloads_made: u64 = 0;
    // ---- requests, lock-step: one in flight per connection
    loop {
        let req: Value = match read_json_line(&mut reader, &mut buf, limit) {
            Ok(Some(v)) => v,
            Ok(None) | Err(FrameError::Io(_)) => break,
            Err(e) => {
                send(
                    &json!({"type": "response", "id": 0, "ok": false, "error": frame_err(&e).to_value()}),
                );
                continue;
            }
        };
        if let Some(secret) = &s.cfg.private_secret {
            // A verified request is never refused by the limiter, so a block caused by other
            // callers cannot end an authenticated connection (issue #504).
            if !secret.verify(req["secret"].as_str().unwrap_or("")) {
                let now = Instant::now();
                let limited = s
                    .fails
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .refuses_unverified(conn_key, now);
                if limited {
                    s.event(ServiceEvent::ConnectionRejected {
                        app: app.clone(),
                        reason: "too many failed authentications".into(),
                    });
                    send(
                        &json!({"type": "response", "id": req["id"].as_u64().unwrap_or(0), "ok": false,
                        "error": ServiceError::new(crate::error::ERROR_RATE_LIMITED, "too many failed authentications; wait before reconnecting").to_value()}),
                    );
                    break;
                }
                s.fails
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .record_failure(conn_key, now);
                s.event(ServiceEvent::ConnectionRejected {
                    app: app.clone(),
                    reason: "missing or wrong secret".into(),
                });
                send(
                    &json!({"type": "response", "id": req["id"].as_u64().unwrap_or(0), "ok": false,
                    "error": ServiceError::new(crate::error::ERROR_UNAUTHENTICATED, "missing or wrong secret").to_value()}),
                );
                break;
            }
        }
        let Some(rid) = req["id"].as_u64().filter(|_| req["type"] == "request") else {
            send(
                &json!({"type": "response", "id": 0, "ok": false, "error": ServiceError::new("bad_request", "expected {type:request,id,method}").to_value()}),
            );
            continue;
        };
        let respond_err = |e: ServiceError| {
            send(&json!({"type": "response", "id": rid, "ok": false, "error": e.to_value()}))
        };
        // Any frame is a chance to drop an assembly that went idle.
        uploads.expire();
        let mut method = req["method"].as_str().unwrap_or("").to_string();
        let chunked_ok = req["chunked_ok"] == true && s.max_message > 0;
        if s.handoff.load(Ordering::SeqCst) {
            respond_err(ServiceError::handoff());
            continue;
        }
        // The reserved chunk methods carry other calls: the carried call is authorised below on
        // its own method id, so these need no allowlist entry of their own.
        let mut assembled: Option<Value> = None;
        if method == crate::chunk::METHOD_FETCH {
            let answer = match &download {
                Some(d) => d.fetch(req.get("params").unwrap_or(&Value::Null)),
                None => Err(ServiceError::new(
                    crate::error::ERROR_BAD_CHUNK,
                    "no chunked result is held on this connection",
                )),
            };
            match answer {
                Ok(v) => send(&json!({"type": "response", "id": rid, "ok": true, "result": v})),
                Err(e) => respond_err(e),
            }
            continue;
        }
        if method == crate::chunk::METHOD_UPLOAD {
            if s.max_message == 0 {
                respond_err(ServiceError::new(
                    crate::error::ERROR_REQUEST_TOO_LARGE,
                    "this host does not accept chunked requests",
                ));
                continue;
            }
            // The caller's allowlist runs on the first piece, before a byte is buffered; the
            // final dispatch below checks again.
            let authorize = |m: &str| {
                crate::contract::authorize_allowlist(&s.cfg.allowlist, &caller, m).inspect_err(
                    |_| {
                        s.event(ServiceEvent::Denied {
                            conn: id,
                            app: app.clone(),
                            method: m.to_string(),
                        });
                    },
                )
            };
            match uploads.accept_with(
                req.get("params").unwrap_or(&Value::Null),
                s.max_message,
                &authorize,
            ) {
                Ok(crate::chunk::UploadStep::More { received }) => {
                    send(&json!({"type": "response", "id": rid, "ok": true,
                        "result": {"received": received}}));
                    continue;
                }
                Ok(crate::chunk::UploadStep::Done { method: m, params }) => {
                    method = m;
                    assembled = Some(params);
                }
                Err(e) => {
                    respond_err(e);
                    continue;
                }
            }
        }
        if let Err(e) = crate::contract::authorize_allowlist(&s.cfg.allowlist, &caller, &method) {
            s.event(ServiceEvent::Denied {
                conn: id,
                app: app.clone(),
                method: method.clone(),
            });
            respond_err(e);
            continue;
        }
        if method == "ping" {
            send(
                &json!({"type": "response", "id": rid, "ok": true, "result": {"pong": true, "rank": s.cfg.rank}}),
            );
            continue;
        }
        let params =
            assembled.unwrap_or_else(|| req.get("params").cloned().unwrap_or_else(|| json!({})));
        let cx = new_context(
            id,
            caller.clone(),
            Arc::new(AtomicBool::new(false)),
            s.handoff.clone(),
        );
        let emit: Emit = {
            let (write, s, method) = (write.clone(), s.clone(), method.clone());
            Arc::new(move |ev| {
                if !write(&json!({"type": "event", "id": rid, "event": ev})) {
                    s.event(ServiceEvent::FrameTooLarge {
                        conn: id,
                        method: method.clone(),
                        kind: "event",
                        limit,
                    });
                }
            })
        };
        s.inflight.fetch_add(1, Ordering::SeqCst);
        let done = AtomicBool::new(false);
        let result = std::thread::scope(|sc| {
            // Parked, not slept: the scope joins this thread, so a plain 40 ms sleep added up to
            // 40 ms to every request after its handler had already finished.
            let watcher = sc.spawn(|| {
                while !done.load(Ordering::SeqCst) {
                    if peer_closed() {
                        cx.closed.store(true, Ordering::SeqCst);
                        notify_closed();
                        return;
                    }
                    std::thread::park_timeout(Duration::from_millis(40));
                }
            });
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                s.handler.call(&cx, &method, &params, &emit)
            }))
            .unwrap_or_else(|_| Err(ServiceError::new("panic", "handler panicked")));
            done.store(true, Ordering::SeqCst);
            watcher.thread().unpark();
            r
        });
        s.inflight.fetch_sub(1, Ordering::SeqCst);
        if cx.peer_closed() {
            break;
        }
        match result {
            Ok(v) => {
                // A result that does not fit is answered with a typed error, never dropped:
                // a dropped response leaves the client waiting for its whole deadline.
                if !write(&json!({"type": "response", "id": rid, "ok": true, "result": v})) {
                    s.event(ServiceEvent::FrameTooLarge {
                        conn: id,
                        method: method.clone(),
                        kind: "response",
                        limit,
                    });
                    // A client that opted in gets the result in pieces, within the message
                    // bound; everyone else, and anything beyond the bound, the typed error.
                    let held = chunked_ok
                        .then(|| serde_json::to_string(&v).ok())
                        .flatten()
                        .filter(|text| text.len() <= s.max_message);
                    drop(v);
                    match held {
                        Some(text) => {
                            downloads_made += 1;
                            let held = crate::chunk::Download::new(
                                format!("d{downloads_made}"),
                                text,
                                chunk_bytes,
                            );
                            let descriptor = held.descriptor();
                            download = Some(held);
                            send(&json!({"type": "response", "id": rid, "ok": true,
                                "result": Value::Null, "chunked": descriptor}));
                        }
                        None => respond_err(ServiceError::new(
                            ERROR_RESPONSE_TOO_LARGE,
                            format!(
                                "the result of {method} is larger than the {limit}-byte frame limit"
                            ),
                        )),
                    }
                }
            }
            Err(e) => respond_err(e),
        }
    }
    notify_closed();
    s.conns
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&id);
    shutdown();
}

#[allow(dead_code)]
fn _assert_transport_used() {
    let _ = transport::current_uid;
}

#[cfg(test)]
mod options_tests {
    use super::*;

    #[test]
    fn pipe_instance_cap_is_small_finite_and_clamped() {
        let d = HostOptions::default();
        assert_eq!(d.get_max_pipe_instances(), DEFAULT_MAX_PIPE_INSTANCES);
        assert!(d.get_max_pipe_instances() >= 1 && d.get_max_pipe_instances() < 255);
        assert_eq!(
            HostOptions::new()
                .max_pipe_instances(4)
                .get_max_pipe_instances(),
            4
        );
        assert_eq!(
            HostOptions::new()
                .max_pipe_instances(0)
                .get_max_pipe_instances(),
            1
        );
        // 255 is PIPE_UNLIMITED_INSTANCES to Win32; never reachable.
        assert_eq!(
            HostOptions::new()
                .max_pipe_instances(u32::MAX)
                .get_max_pipe_instances(),
            254
        );
    }

    #[test]
    fn handshake_deadline_can_be_removed_and_never_reaches_zero() {
        assert_eq!(
            HostOptions::new()
                .handshake_timeout(None)
                .get_handshake_timeout(),
            None
        );
        assert_eq!(
            HostOptions::new()
                .handshake_timeout(Some(Duration::ZERO))
                .get_handshake_timeout(),
            Some(Duration::from_millis(1))
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_supervisor_is_the_parent_unless_named_or_removed() {
        assert_eq!(
            HostOptions::default().get_supervisor_pid(),
            Some(std::os::unix::process::parent_id())
        );
        assert_eq!(
            HostOptions::new()
                .supervisor_pid(Some(7))
                .get_supervisor_pid(),
            Some(7)
        );
        assert_eq!(
            HostOptions::new().supervisor_pid(None).get_supervisor_pid(),
            None
        );
    }

    #[test]
    fn a_connection_leaves_the_pre_handshake_count_exactly_once() {
        let count = Arc::new(AtomicUsize::new(0));
        let mut a = PreHandshake::enter(&count);
        let b = PreHandshake::enter(&count);
        assert_eq!(count.load(Ordering::SeqCst), 2);
        // Settling twice, then dropping, still removes `a` only once.
        a.settle();
        a.settle();
        assert_eq!(count.load(Ordering::SeqCst), 1);
        drop(a);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        // A connection that ends before hello leaves through its drop.
        drop(b);
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }
}

#[cfg(test)]
mod monitor_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// Longer than any test: a wake-up that arrives is a signal, never the interval running out.
    const LONG: Duration = Duration::from_secs(600);

    struct Quiet {
        handoffs: AtomicUsize,
    }

    impl Handler for Quiet {
        fn call(
            &self,
            _cx: &CallContext,
            _method: &str,
            _params: &Value,
            _emit: &Emit,
        ) -> Result<Value, ServiceError> {
            Ok(json!({}))
        }
        fn on_handoff(&self) {
            self.handoffs.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !ok() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// A host of rank 5 on a fresh suite, in this process.
    fn host_in(
        dir: &tempfile::TempDir,
        interval: Duration,
        alive: fn(u32) -> bool,
    ) -> (Suite, Hosted, Arc<Quiet>) {
        let suite = Suite::at(dir.path().join("suite")).unwrap();
        let mut cfg = ServiceConfig::new(
            "watch",
            "incumbent",
            5,
            "test",
            Allowlist::new().allow("*", ["echo"]),
        );
        cfg.takeover_timeout = Duration::from_secs(20);
        cfg.handoff_grace = Duration::from_secs(1);
        let handler = Arc::new(Quiet {
            handoffs: AtomicUsize::new(0),
        });
        let hosted = start_host(
            &suite,
            cfg,
            handler.clone(),
            HostOptions::default(),
            WatchTuning { interval, alive },
        )
        .expect("host starts");
        (suite, hosted, handler)
    }

    fn request(role: &str, rank: u8, pid: u32) -> TakeoverRequest {
        TakeoverRequest {
            role: role.into(),
            rank,
            pid,
            requested_at_ms: now_ms(),
        }
    }

    /// A pid that is not this process's: the monitor ignores requests from its own process.
    fn other_pid() -> u32 {
        std::process::id().wrapping_add(1)
    }

    #[test]
    fn a_request_from_another_process_is_found_by_the_file_check() {
        let dir = tempfile::tempdir().unwrap();
        let (suite, hosted, handler) = host_in(&dir, Duration::from_millis(50), |_| true);
        // Written the way another process writes it: nothing in this process is told.
        let body = serde_json::to_vec(&request("shell", 0, other_pid())).unwrap();
        write_atomic(&takeover_path(&suite, "watch"), &body).unwrap();
        assert!(
            hosted.wait_released(Duration::from_secs(20)),
            "the incumbent must hand off"
        );
        assert!(hosted.handoff_requested());
        assert_eq!(handler.handoffs.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_request_written_in_this_process_wakes_the_monitor_without_the_interval() {
        let dir = tempfile::tempdir().unwrap();
        let (suite, hosted, handler) = host_in(&dir, LONG, |_| true);
        let bell = hosted.shared.bell.clone();
        wait_for("the first pass", || bell.passes() >= 1);
        write_takeover(&suite, "watch", &request("shell", 0, other_pid())).unwrap();
        assert!(
            hosted.wait_released(Duration::from_secs(20)),
            "a 10-minute interval must not delay a request raised in this process"
        );
        assert_eq!(handler.handoffs.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn releasing_the_host_ends_the_monitor_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let (_suite, hosted, _handler) = host_in(&dir, LONG, pid_alive);
        let bell = hosted.shared.bell.clone();
        wait_for("the first pass", || bell.passes() >= 1);
        assert!(!bell.exited());
        hosted.shutdown();
        wait_for("the monitor thread to return", || bell.exited());
    }

    #[test]
    fn an_unchanged_request_that_does_not_oblige_is_stat_ed_not_read_again() {
        let dir = tempfile::tempdir().unwrap();
        let (suite, hosted, handler) = host_in(&dir, Duration::from_millis(20), |_| true);
        let bell = hosted.shared.bell.clone();
        // Equal rank: never obliges.
        let body = serde_json::to_vec(&request("peer", 5, other_pid())).unwrap();
        write_atomic(&takeover_path(&suite, "watch"), &body).unwrap();
        wait_for("the request to be judged", || bell.reads() >= 1);
        let before = bell.wakes();
        wait_for("five more checks", || bell.wakes() >= before + 5);
        assert_eq!(bell.reads(), 1, "an unchanged file must not be read again");
        assert!(!hosted.handoff_requested());

        // Replacing it is noticed, and a better-ranked request is then accepted.
        let body = serde_json::to_vec(&request("shell", 0, other_pid())).unwrap();
        write_atomic(&takeover_path(&suite, "watch"), &body).unwrap();
        assert!(hosted.wait_released(Duration::from_secs(20)));
        assert_eq!(handler.handoffs.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_quiet_host_wakes_its_monitor_about_once_a_second() {
        let dir = tempfile::tempdir().unwrap();
        let (_suite, hosted, _handler) = host_in(&dir, TAKEOVER_CHECK_INTERVAL, pid_alive);
        let bell = hosted.shared.bell.clone();
        let (start, woke_before) = (Instant::now(), bell.wakes());
        std::thread::sleep(Duration::from_secs(2));
        let (secs, woke) = (start.elapsed().as_secs_f64(), bell.wakes() - woke_before);
        // The window can stretch on a loaded machine; the allowance stretches with it. The
        // 25 ms poll this replaces woke about 80 times in two seconds.
        let allowed = 3u64.max(secs.ceil() as u64 + 1);
        assert!(
            woke <= allowed,
            "the monitor woke {woke} times in {secs:.1} s"
        );
        drop(hosted);
    }

    /// A directory at `takeover.json` makes the read fail with an error that is not `NotFound`
    /// (`IsADirectory` on Unix, `PermissionDenied` on Windows) on every platform.
    #[test]
    fn an_unreadable_request_is_looked_at_again_not_judged_absent() {
        let dir = tempfile::tempdir().unwrap();
        let (suite, hosted, handler) = host_in(&dir, Duration::from_millis(20), |_| true);
        let bell = hosted.shared.bell.clone();
        let path = takeover_path(&suite, "watch");
        std::fs::create_dir_all(&path).unwrap();
        // The file stat never changes, yet it is read on every pass: it was never judged.
        wait_for("three reads of the unreadable file", || bell.reads() >= 3);
        assert!(!hosted.handoff_requested());

        // Once it can be read, the request it holds is accepted.
        std::fs::remove_dir(&path).unwrap();
        let body = serde_json::to_vec(&request("shell", 0, other_pid())).unwrap();
        write_atomic(&path, &body).unwrap();
        assert!(hosted.wait_released(Duration::from_secs(20)));
        assert_eq!(handler.handoffs.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn an_unreadable_record_blocks_the_election_instead_of_being_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let suite = Suite::at(dir.path().join("suite")).unwrap();
        let record = record_path(&suite, "watch");
        std::fs::create_dir_all(&record).unwrap();
        let mut cfg = ServiceConfig::new("watch", "newcomer", 5, "test", Allowlist::new());
        cfg.takeover_timeout = Duration::from_millis(300);
        let err = start_host(
            &suite,
            cfg,
            Arc::new(Quiet {
                handoffs: AtomicUsize::new(0),
            }),
            HostOptions::default(),
            WatchTuning {
                interval: LONG,
                alive: |_| true,
            },
        )
        .err()
        .expect("a record that cannot be read must not be treated as no record");
        match err {
            HostError::Io(e) => assert_ne!(e.kind(), std::io::ErrorKind::NotFound),
            _ => panic!("expected the read error"),
        }
        assert!(record.is_dir(), "left as it was");
    }

    #[test]
    fn only_a_fresh_live_better_ranked_request_from_another_process_obliges() {
        let mut cfg = ServiceConfig::new("watch", "incumbent", 5, "test", Allowlist::new());
        cfg.takeover_timeout = Duration::from_secs(10);
        cfg.known_roles.insert("legacy".into(), 1);
        let now = 1_000_000_000_000u64;
        let live: fn(u32) -> bool = |_| true;
        let dead: fn(u32) -> bool = |_| false;
        let at = |role: &str, rank: u8, pid: u32, requested_at_ms: u64| TakeoverRequest {
            role: role.into(),
            rank,
            pid,
            requested_at_ms,
        };

        assert_eq!(
            obliging_rank(&cfg, 7, live, &at("shell", 0, 9, now), now),
            Some(0)
        );
        // Equal or worse rank never obliges.
        assert_eq!(obliging_rank(&cfg, 7, live, &at("x", 5, 9, now), now), None);
        assert_eq!(obliging_rank(&cfg, 7, live, &at("x", 6, 9, now), now), None);
        // Our own process never does.
        assert_eq!(
            obliging_rank(&cfg, 9, live, &at("shell", 0, 9, now), now),
            None
        );
        // A requester that has exited abandoned its request.
        assert_eq!(
            obliging_rank(&cfg, 7, dead, &at("shell", 0, 9, now), now),
            None
        );
        // Fresh means younger than the takeover timeout plus five seconds.
        assert_eq!(
            obliging_rank(&cfg, 7, live, &at("shell", 0, 9, now - 15_000), now),
            Some(0)
        );
        assert_eq!(
            obliging_rank(&cfg, 7, live, &at("shell", 0, 9, now - 15_001), now),
            None
        );
        // A peer that names only a role is ranked through `known_roles`; an unknown one never wins.
        assert_eq!(
            obliging_rank(&cfg, 7, live, &at("legacy", RANK_UNKNOWN, 9, now), now),
            Some(1)
        );
        assert_eq!(
            obliging_rank(&cfg, 7, live, &at("stranger", RANK_UNKNOWN, 9, now), now),
            None
        );
    }
}
