//! Daemon client lifecycle with typed provenance (PK06-4, K06 "Host composition").
//!
//! Two supported lifecycles for a service host that outlives one window of one app:
//!
//! * **[`Provenance::Owned`]**: this process launched the host (through the [`Launcher`] the app
//!   supplied), recorded the launched process's [`AdoptedIdentity`] (pid plus start time), and
//!   waited for that same pid to publish itself. Only an owned host may be stopped by this
//!   lifecycle ([`DaemonLifecycle::shutdown_owned`]), and only while the recorded identity still
//!   names the launched process. A replacement the lifecycle launches itself is owned too.
//! * **[`Provenance::Attached`]**: some other process started the host (another shell of the same
//!   app, a CLI, an installer). The lifecycle verifies the endpoint's identity and generation and
//!   uses it; it never signals it, never stops it and never infers a right to. An arbitrary
//!   discovered pid is never owned. [`DaemonLifecycle::release_session`] on an attached client
//!   closes that client's connection and nothing else.
//!
//! The pieces, in the order the spec names them:
//!
//! * [`DaemonLifecycle::attach_or_launch`]: discover the live host. A live, reachable host whose
//!   process generation still matches is attached (after the identity check below). Otherwise,
//!   when a [`Launcher`] was given, take the cross-process spawn claim, launch, and wait for the
//!   launched pid to publish. Without a launcher, no host is [`LifecycleError::NoHost`].
//! * [`DaemonLifecycle::acquire_session`]: **revalidates** the host (record, process generation,
//!   and the connection's own identity check) before it hands out a [`Session`]. A host that
//!   exited is relaunched only when this lifecycle owns the launcher and the restart budget
//!   ([`RestartPolicy`], the same tracker the private-child supervisor uses) allows it; a spent
//!   budget is [`LifecycleError::RestartExhausted`] until
//!   [`reset_restart_budget`](DaemonLifecycle::reset_restart_budget).
//! * [`Session::call`]: revalidates **before the request is written and again before the reply is
//!   handed back**. A reply that arrives after the host changed generation is not delivered
//!   ([`LifecycleError::StaleGeneration`]): whether the request ran on the old host is unknown,
//!   so it is never replayed automatically. [`GenerationFence`] is the same rule for transports
//!   that multiplex requests.
//! * [`DaemonLifecycle::release_session`]: ends one session. Never signals the host.
//! * [`DaemonLifecycle::shutdown_owned`]: owned hosts only. The app's own graceful request first
//!   (when one was configured), the grace period, then the launched process tree is killed if it
//!   is still there. [`LifecycleError::NotOwned`] for an attached host, with nothing sent and
//!   nothing signalled.
//!
//! **Identity.** The local backend ([`LocalBackend`]) connects with
//! [`Client::connect_expecting`]: the OS names the process on the listening end of the socket and
//! its executable must be the [`HostExpectation`]'s, before any byte is written. The kernel pid
//! must also equal the pid `service.json` names, and the record read during the handshake must be
//! the generation that was discovered ([`HostGeneration`]: endpoint, pid, process start time and
//! publication time). A host that fails any of it is [`LifecycleError::HostIdentity`] or a
//! retryable change error, and is never attached.
//!
//! **Not replaced.** [`supervise_adopted`](crate::supervisor::supervise_adopted) stays the way to
//! run a private child (`PrivateChild`, secret on stdin). This module is for resident services
//! published through `service.json`.
//!
//! Dropping a [`DaemonLifecycle`] that owns a host drops its [`OwnedChild`], which ends the
//! launched process tree (the `OwnedChild` drop rule). Dropping an attached lifecycle ends nothing.

use std::any::Any;
use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rightkit_child_process::{
    AdoptedIdentity, Generation, IdentityCheck, OwnedChild, RestartOutcome, RestartPolicy,
    RestartTracker, WaitOutcome,
};
use serde_json::Value;

use crate::client::{Client, ClientConfig};
use crate::discovery::{
    record_process_matches, try_adopt_or_spawn, AdoptOrSpawn, SpawnClaim, SpawnClaimError,
};
use crate::error::{ConnectError, ServiceError, ERROR_UNAVAILABLE};
use crate::host_identity::{HostExpectation, HostIdentityError, HostIdentityReason};
use crate::record::{try_read_record, ServiceRecord};

/// A reply (or a request) belongs to a host generation that is no longer the current one. Not
/// retryable: the request may have run on the old host.
pub const ERROR_STALE_GENERATION: &str = "stale_generation";
/// The host is attached, not owned: this lifecycle may not stop it.
pub const ERROR_NOT_OWNED: &str = "not_owned";
/// The owned host exited and the restart budget is spent.
pub const ERROR_RESTART_EXHAUSTED: &str = "restart_exhausted";
/// The session or the lifecycle was already released or shut down.
pub const ERROR_SESSION_RELEASED: &str = "session_released";
/// The host could not be launched or never published itself.
pub const ERROR_LAUNCH_FAILED: &str = "launch_failed";

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Who started the host this client uses, and so what the client may do to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provenance {
    /// This process launched the host and holds its identity. It may stop it.
    Owned,
    /// Another process started the host. Attaching never implies a right to stop it.
    Attached,
}

impl Provenance {
    pub fn as_str(self) -> &'static str {
        match self {
            Provenance::Owned => "owned",
            Provenance::Attached => "attached",
        }
    }

    /// Whether this lifecycle may signal the host. True only for [`Provenance::Owned`].
    pub fn may_stop_host(self) -> bool {
        matches!(self, Provenance::Owned)
    }
}

/// One incarnation of a published host: the facts in `service.json` that change when the host is
/// replaced. Two values are the same generation only when every field is equal.
///
/// Limit: `started_ms` is the publication time in milliseconds. A host replaced inside the same
/// process within the same millisecond is indistinguishable here; a replacement process differs
/// in pid or process start time. A per-host instance nonce in `service.json` would close that
/// gap and is an open decision (see the CHANGELOG).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HostGeneration {
    pub service: String,
    pub endpoint: String,
    pub pid: u32,
    /// Birth time of the host process as the record states it; 0 when the writer did not know it.
    pub process_started_ms: u64,
    /// Publication time of the record.
    pub started_ms: u64,
}

impl HostGeneration {
    /// The generation a record names.
    pub fn of(record: &ServiceRecord) -> Self {
        Self {
            service: record.service.clone(),
            endpoint: record.endpoint.clone(),
            pid: record.pid,
            process_started_ms: record.process_started_ms,
            started_ms: record.started_ms,
        }
    }
}

/// Why a lifecycle operation failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum LifecycleError {
    /// A wire-level or discovery error (kept with its `retryable` flag).
    Service(ServiceError),
    /// The process behind the endpoint is not the host the caller expected. Nothing was sent to
    /// it, and nothing was attached.
    HostIdentity(HostIdentityError),
    /// No live host, and no launcher to start one. Retryable.
    NoHost,
    /// The launcher failed, or the launched process left before it published itself.
    Launch(io::Error),
    /// The launched process did not publish itself within the startup timeout (it was stopped).
    LaunchTimeout,
    /// The owned host exited and the restart budget is spent.
    RestartExhausted,
    /// [`DaemonLifecycle::shutdown_owned`] on a host this lifecycle did not launch.
    NotOwned,
    /// The session or the lifecycle was already released or shut down.
    Released,
    /// The host the session was bound to is gone or was replaced. `issued` is the generation
    /// epoch the session (or request) was bound to, `current` the epoch now. The request may or
    /// may not have run on the old host.
    StaleGeneration { issued: u64, current: u64 },
    /// The host generation could not be confirmed (its record was unreadable). Treated like
    /// stale: a reply that cannot be tied to the current host is not delivered.
    Unconfirmed,
    /// A process operation failed (identity read, signal).
    Process(io::Error),
}

impl LifecycleError {
    /// The wire-style code.
    pub fn code(&self) -> &str {
        match self {
            LifecycleError::Service(e) => &e.code,
            LifecycleError::HostIdentity(_) => crate::error::ERROR_HOST_IDENTITY,
            LifecycleError::NoHost => ERROR_UNAVAILABLE,
            LifecycleError::Launch(_) | LifecycleError::LaunchTimeout => ERROR_LAUNCH_FAILED,
            LifecycleError::RestartExhausted => ERROR_RESTART_EXHAUSTED,
            LifecycleError::NotOwned => ERROR_NOT_OWNED,
            LifecycleError::Released => ERROR_SESSION_RELEASED,
            LifecycleError::StaleGeneration { .. } | LifecycleError::Unconfirmed => {
                ERROR_STALE_GENERATION
            }
            LifecycleError::Process(_) => "io",
        }
    }

    /// Whether trying again (a fresh `acquire_session`) can succeed. Never true for a stale
    /// reply as such: the caller decides whether the request may run twice.
    pub fn is_retryable(&self) -> bool {
        match self {
            LifecycleError::Service(e) => e.retryable,
            LifecycleError::NoHost => true,
            _ => false,
        }
    }

    /// Collapse into the single wire-style error.
    pub fn into_service_error(self) -> ServiceError {
        let text = self.to_string();
        match self {
            LifecycleError::Service(e) => e,
            LifecycleError::HostIdentity(e) => e.into(),
            LifecycleError::NoHost => ServiceError::retryable(ERROR_UNAVAILABLE, text),
            other => ServiceError::new(other.code().to_string(), text),
        }
    }
}

impl fmt::Display for LifecycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LifecycleError::Service(e) => fmt::Display::fmt(e, f),
            LifecycleError::HostIdentity(e) => fmt::Display::fmt(e, f),
            LifecycleError::NoHost => f.write_str("no live host and no launcher to start one"),
            LifecycleError::Launch(e) => write!(f, "the host could not be launched: {e}"),
            LifecycleError::LaunchTimeout => {
                f.write_str("the launched host did not publish itself in time; it was stopped")
            }
            LifecycleError::RestartExhausted => {
                f.write_str("the owned host exited and the restart budget is spent")
            }
            LifecycleError::NotOwned => f.write_str(
                "this host was not launched by this lifecycle; attaching never implies the right to stop it",
            ),
            LifecycleError::Released => f.write_str("the session or the lifecycle was released"),
            LifecycleError::StaleGeneration { issued, current } => write!(
                f,
                "the host generation changed (bound to epoch {issued}, now {current}); the request may have run on the previous host and is not replayed"
            ),
            LifecycleError::Unconfirmed => f.write_str(
                "the host generation could not be confirmed; the reply is not delivered",
            ),
            LifecycleError::Process(e) => write!(f, "process operation failed: {e}"),
        }
    }
}

impl std::error::Error for LifecycleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LifecycleError::Service(e) => Some(e),
            LifecycleError::HostIdentity(e) => Some(e),
            LifecycleError::Launch(e) | LifecycleError::Process(e) => Some(e),
            _ => None,
        }
    }
}

impl From<ServiceError> for LifecycleError {
    fn from(e: ServiceError) -> Self {
        LifecycleError::Service(e)
    }
}

fn unavailable(e: io::Error) -> LifecycleError {
    LifecycleError::Service(ServiceError::retryable(
        ERROR_UNAVAILABLE,
        format!("host discovery failed (try again): {e}"),
    ))
}

// ---------------------------------------------------------------------------------------------
// Generation fence
// ---------------------------------------------------------------------------------------------

/// A request was issued under, or a reply tagged with, an epoch that is no longer current.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stale {
    pub issued: u64,
    pub current: u64,
}

impl From<Stale> for LifecycleError {
    fn from(s: Stale) -> Self {
        LifecycleError::StaleGeneration {
            issued: s.issued,
            current: s.current,
        }
    }
}

/// A request in flight, bound to the epoch it was issued under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ticket {
    epoch: u64,
    request: u64,
}

impl Ticket {
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn request(&self) -> u64 {
        self.request
    }
}

/// Binds requests to a host generation. The epoch advances every time the host the lifecycle is
/// bound to changes (first bind, replacement, exit). A [`Ticket`] issued under an older epoch can
/// never complete: its reply is [`Stale`], so a late reply from a previous host cannot answer a
/// request made to the next one.
///
/// Transports that multiplex requests over one connection use it directly: `issue` when a request
/// is written, `admit_reply` (or `admit_reply_from` when the reply carries the epoch it was read
/// under) when its reply arrives.
#[derive(Debug, Default)]
pub struct GenerationFence {
    epoch: AtomicU64,
    next_request: AtomicU64,
}

impl GenerationFence {
    pub fn new() -> Self {
        Self::default()
    }

    /// The current epoch (0 before any host was bound).
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    /// A different host generation was bound (or the host left): every outstanding [`Ticket`] is
    /// now stale. Returns the new epoch.
    pub fn advance(&self) -> u64 {
        self.epoch.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Issue a request under `bound_epoch`, the epoch the caller's connection was bound to.
    /// Refused when that is no longer the current epoch.
    pub fn issue(&self, bound_epoch: u64) -> Result<Ticket, Stale> {
        let current = self.epoch();
        if bound_epoch != current {
            return Err(Stale {
                issued: bound_epoch,
                current,
            });
        }
        Ok(Ticket {
            epoch: bound_epoch,
            request: self.next_request.fetch_add(1, Ordering::SeqCst) + 1,
        })
    }

    /// A reply arrived for `ticket`. Admitted only while the epoch it was issued under is still
    /// the current one.
    pub fn admit_reply(&self, ticket: &Ticket) -> Result<(), Stale> {
        let current = self.epoch();
        if ticket.epoch != current {
            return Err(Stale {
                issued: ticket.epoch,
                current,
            });
        }
        Ok(())
    }

    /// [`admit_reply`](Self::admit_reply) for a reply that itself names the epoch of the
    /// connection it was read from: it must equal the ticket's epoch as well.
    pub fn admit_reply_from(&self, ticket: &Ticket, reply_epoch: u64) -> Result<(), Stale> {
        if reply_epoch != ticket.epoch {
            return Err(Stale {
                issued: ticket.epoch,
                current: reply_epoch,
            });
        }
        self.admit_reply(ticket)
    }
}

// ---------------------------------------------------------------------------------------------
// Backend: how a host is found and reached
// ---------------------------------------------------------------------------------------------

/// A connection to exactly one host generation, as the lifecycle uses it.
pub trait HostLink: Send + Sync {
    fn call(&self, method: &str, params: Value) -> Result<Value, ServiceError>;
    /// Whether the connection is known closed.
    fn is_closed(&self) -> bool;
    /// Close the connection. Never stops the host.
    fn close(&self);
}

/// Whether this process may start a host now.
pub enum SpawnPermit {
    /// This process holds the cross-process claim until the value is dropped.
    Granted(Box<dyn Any + Send>),
    /// Another process is starting a host: wait for it to publish.
    Contended,
}

/// How a lifecycle finds, verifies and reaches a host. [`LocalBackend`] is the `service.json`
/// transport; tests and other transports implement it too.
pub trait HostBackend: Send + Sync + 'static {
    /// The live published host now, or `None`. An unreadable record is `Err` (unknown), never
    /// `None`: the lifecycle must not start a second host on an unknown.
    fn discover(&self) -> io::Result<Option<HostGeneration>>;

    /// Open a connection to exactly `host`, after verifying who is on the other end. A host that
    /// fails the identity check is [`LifecycleError::HostIdentity`]; nothing application-level was
    /// sent to it.
    fn connect(&self, host: &HostGeneration) -> Result<Box<dyn HostLink>, LifecycleError>;

    /// Whether `host` is still the published, live generation. Called before every request and
    /// before every reply is handed back, so it should be cheap (no connection).
    fn is_current(&self, host: &HostGeneration) -> io::Result<bool> {
        Ok(self.discover()?.as_ref() == Some(host))
    }

    /// Take the cross-process claim that serialises starting a host.
    fn claim_spawn(&self) -> io::Result<SpawnPermit>;
}

/// The local `service.json` transport: discovery through the suite, identity through
/// [`Client::connect_expecting`].
pub struct LocalBackend {
    client: ClientConfig,
    expect: HostExpectation,
}

impl LocalBackend {
    /// `client` names the service, the app label and the suite (see [`ClientConfig`]); `expect`
    /// names the executable the host may be. An expectation is required: attaching to a host
    /// whose program was never checked is not offered.
    pub fn new(client: ClientConfig, expect: HostExpectation) -> Self {
        Self { client, expect }
    }

    fn suite(&self) -> io::Result<crate::suite::Suite> {
        self.client.resolve_suite().map_err(io::Error::other)
    }
}

struct LocalLink(Client);

impl HostLink for LocalLink {
    fn call(&self, method: &str, params: Value) -> Result<Value, ServiceError> {
        self.0.call(method, params)
    }

    fn is_closed(&self) -> bool {
        self.0.is_closed()
    }

    fn close(&self) {
        self.0.abort();
    }
}

impl HostBackend for LocalBackend {
    fn discover(&self) -> io::Result<Option<HostGeneration>> {
        let suite = self.suite()?;
        Ok(match try_adopt_or_spawn(&suite, self.client.service())? {
            AdoptOrSpawn::Adopt(record) => Some(HostGeneration::of(&record)),
            AdoptOrSpawn::Spawn => None,
        })
    }

    fn connect(&self, host: &HostGeneration) -> Result<Box<dyn HostLink>, LifecycleError> {
        let client =
            Client::connect_expecting(&self.client, &self.expect).map_err(|e| match e {
                ConnectError::HostIdentity(identity) => LifecycleError::HostIdentity(identity),
                other => LifecycleError::Service(other.into_service_error()),
            })?;
        // The record the handshake read must be the generation that was discovered.
        if HostGeneration::of(client.record()) != *host {
            client.abort();
            return Err(LifecycleError::Service(ServiceError::retryable(
                ERROR_UNAVAILABLE,
                "the host changed while connecting; discover it again",
            )));
        }
        // The process the kernel put behind the socket must be the process the record names.
        let identity_error = |found_pid: Option<u32>,
                              found_executable: Option<std::path::PathBuf>,
                              reason: HostIdentityReason| {
            LifecycleError::HostIdentity(HostIdentityError {
                expected: self.expect.executables().to_vec(),
                found_pid,
                found_executable,
                reason,
            })
        };
        match client.verified_host() {
            Some(process) if process.pid == host.pid => {}
            Some(process) => {
                let error = identity_error(
                    Some(process.pid),
                    Some(process.executable.clone()),
                    HostIdentityReason::HostChanged(format!(
                        "the endpoint is served by pid {} but service.json names pid {}",
                        process.pid, host.pid
                    )),
                );
                client.abort();
                return Err(error);
            }
            None => {
                client.abort();
                return Err(identity_error(
                    None,
                    None,
                    HostIdentityReason::PidUnavailable,
                ));
            }
        }
        Ok(Box::new(LocalLink(client)))
    }

    fn is_current(&self, host: &HostGeneration) -> io::Result<bool> {
        let suite = self.suite()?;
        Ok(try_read_record(&suite, self.client.service())?
            .is_some_and(|r| record_process_matches(&r) && HostGeneration::of(&r) == *host))
    }

    fn claim_spawn(&self) -> io::Result<SpawnPermit> {
        let suite = self.suite()?;
        match SpawnClaim::acquire(&suite, self.client.service()) {
            Ok(claim) => Ok(SpawnPermit::Granted(Box::new(claim))),
            Err(SpawnClaimError::AlreadyHeld(_)) => Ok(SpawnPermit::Contended),
            Err(SpawnClaimError::Io(e)) => Err(e),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------------------------

/// Starts the host. Called with the cross-process claim held, at most once per start. The child
/// it returns is owned by the lifecycle from then on; the app configures the command, including
/// how it detaches.
pub type Launcher = Box<dyn FnMut() -> io::Result<OwnedChild> + Send>;

/// The app's own polite stop request (for example its `shutdown` method), sent over a freshly
/// verified connection to the owned host before any signal. An `Err` is not a failure of the
/// shutdown: a host that exits as it answers often drops the connection.
pub type GracefulStop = Arc<dyn Fn(&dyn HostLink) -> Result<(), ServiceError> + Send + Sync>;

/// Timeouts, restart budget and the graceful stop request.
#[derive(Clone)]
pub struct LifecycleConfig {
    startup_timeout: Duration,
    poll_interval: Duration,
    stop_grace: Duration,
    restart: RestartPolicy,
    graceful: Option<GracefulStop>,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            startup_timeout: Duration::from_secs(30),
            poll_interval: Duration::from_millis(50),
            stop_grace: Duration::from_secs(5),
            restart: RestartPolicy::default(),
            graceful: None,
        }
    }
}

impl LifecycleConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// How long a launch (or a wait for another process's launch) may take. Default 30 s.
    #[must_use]
    pub fn startup_timeout(mut self, timeout: Duration) -> Self {
        self.startup_timeout = timeout;
        self
    }

    /// Time between checks while waiting for a host to publish. Default 50 ms, at least 1 ms.
    #[must_use]
    pub fn poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval.max(Duration::from_millis(1));
        self
    }

    /// How long an owned host gets to exit after the graceful request before its process tree is
    /// killed. Default 5 s.
    #[must_use]
    pub fn stop_grace(mut self, grace: Duration) -> Self {
        self.stop_grace = grace;
        self
    }

    /// The restart budget for an owned host that exits or fails to start. Default
    /// [`RestartPolicy::default`] (automatic, 3 per 60 s).
    #[must_use]
    pub fn restart_policy(mut self, policy: RestartPolicy) -> Self {
        self.restart = policy;
        self
    }

    /// The graceful stop request for [`DaemonLifecycle::shutdown_owned`].
    #[must_use]
    pub fn graceful_stop<F>(mut self, stop: F) -> Self
    where
        F: Fn(&dyn HostLink) -> Result<(), ServiceError> + Send + Sync + 'static,
    {
        self.graceful = Some(Arc::new(stop));
        self
    }
}

// ---------------------------------------------------------------------------------------------
// The lifecycle
// ---------------------------------------------------------------------------------------------

struct OwnedHost {
    child: OwnedChild,
    /// pid plus start time of the launched process: the only thing that authorises a signal.
    identity: AdoptedIdentity,
    /// The generation the launched process published, once it did.
    host: Option<HostGeneration>,
}

struct Slow {
    owned: Option<OwnedHost>,
    tracker: RestartTracker,
    origin: Instant,
    generation: Option<Generation>,
    /// What the tracker decided at the last exit or failed start, applied at the next launch.
    pending: Option<RestartOutcome>,
    exhausted: bool,
}

#[derive(Clone)]
struct Current {
    host: HostGeneration,
    provenance: Provenance,
    epoch: u64,
}

struct Inner {
    backend: Arc<dyn HostBackend>,
    cfg: LifecycleConfig,
    launcher: Mutex<Option<Launcher>>,
    /// Serialises launching, adopting and stopping. Held across a launch wait; never taken by
    /// [`Session::call`].
    slow: Mutex<Slow>,
    current: Mutex<Option<Current>>,
    sessions: Mutex<BTreeMap<u64, String>>,
    fence: GenerationFence,
    next_session: AtomicU64,
    stopped: AtomicBool,
}

/// A client's handle on a resident service host, with typed [`Provenance`].
pub struct DaemonLifecycle {
    inner: Arc<Inner>,
}

/// What [`DaemonLifecycle::release_session`] did. The host was not touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseReport {
    pub label: String,
    pub provenance: Provenance,
    /// Sessions still held by this lifecycle after the release.
    pub remaining_sessions: usize,
}

/// What [`DaemonLifecycle::shutdown_owned`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownReport {
    /// A graceful request was configured, the host was confirmed current, and it was sent.
    pub graceful_requested: bool,
    /// The host acknowledged the graceful request (it may also drop the connection instead).
    pub graceful_acknowledged: bool,
    /// The process tree had to be killed after the grace period.
    pub killed: bool,
    /// The launched process's exit code, when it has one.
    pub exit_code: Option<i32>,
}

impl DaemonLifecycle {
    /// Attach to the live host of the backend's service, or launch one. See the module docs.
    ///
    /// `launcher` is `None` for a client that only ever attaches. With a launcher, a host that is
    /// not running is started under the cross-process claim, so two shells starting together
    /// launch exactly one: the loser waits for the winner's host and attaches to it.
    pub fn attach_or_launch(
        backend: Arc<dyn HostBackend>,
        launcher: Option<Launcher>,
        config: LifecycleConfig,
    ) -> Result<Self, LifecycleError> {
        let tracker = RestartTracker::new(config.restart);
        let inner = Arc::new(Inner {
            backend,
            cfg: config,
            launcher: Mutex::new(launcher),
            slow: Mutex::new(Slow {
                owned: None,
                tracker,
                origin: Instant::now(),
                generation: None,
                pending: None,
                exhausted: false,
            }),
            current: Mutex::new(None),
            sessions: Mutex::new(BTreeMap::new()),
            fence: GenerationFence::new(),
            next_session: AtomicU64::new(1),
            stopped: AtomicBool::new(false),
        });
        inner.ensure_host()?;
        Ok(Self { inner })
    }

    /// [`attach_or_launch`](Self::attach_or_launch) over the local `service.json` transport.
    pub fn attach_or_launch_local(
        client: ClientConfig,
        expect: HostExpectation,
        launcher: Option<Launcher>,
        config: LifecycleConfig,
    ) -> Result<Self, LifecycleError> {
        Self::attach_or_launch(
            Arc::new(LocalBackend::new(client, expect)),
            launcher,
            config,
        )
    }

    /// How the host this lifecycle uses was started, or `None` when no host is bound now.
    pub fn provenance(&self) -> Option<Provenance> {
        lock(&self.inner.current).as_ref().map(|c| c.provenance)
    }

    /// The generation of the host bound now.
    pub fn host(&self) -> Option<HostGeneration> {
        lock(&self.inner.current).as_ref().map(|c| c.host.clone())
    }

    /// The fence epoch: advances whenever the bound host changes.
    pub fn epoch(&self) -> u64 {
        self.inner.fence.epoch()
    }

    /// Sessions held now. The count a presence or idle-exit policy reads.
    pub fn session_count(&self) -> usize {
        lock(&self.inner.sessions).len()
    }

    /// Forget the spent restart budget (an explicit user start).
    pub fn reset_restart_budget(&self) {
        let mut slow = lock(&self.inner.slow);
        slow.tracker.reset_budget();
        slow.pending = None;
        slow.exhausted = false;
    }

    /// Revalidate the host (identity, generation, process liveness; relaunch an owned host that
    /// exited when the budget allows) and open a verified connection to it as a [`Session`].
    /// `label` names the holder for diagnostics.
    pub fn acquire_session(&self, label: &str) -> Result<Session, LifecycleError> {
        let current = self.inner.ensure_host()?;
        let link = self.inner.backend.connect(&current.host)?;
        // The host must still be the one that was verified when the connection is handed out.
        match self.inner.backend.is_current(&current.host) {
            Ok(true) => {}
            Ok(false) => {
                link.close();
                self.inner.host_left(&current.host);
                return Err(LifecycleError::StaleGeneration {
                    issued: current.epoch,
                    current: self.inner.fence.epoch(),
                });
            }
            Err(_) => {
                link.close();
                return Err(LifecycleError::Unconfirmed);
            }
        }
        let id = self.inner.next_session.fetch_add(1, Ordering::SeqCst);
        lock(&self.inner.sessions).insert(id, label.to_string());
        Ok(Session {
            inner: self.inner.clone(),
            id,
            label: label.to_string(),
            provenance: current.provenance,
            host: current.host,
            epoch: current.epoch,
            link,
            released: AtomicBool::new(false),
        })
    }

    /// End one session: its connection closes and it leaves the count. **Never signals the
    /// host**, whatever its provenance: an attached client leaves a shared service running, and
    /// an owned host keeps running until [`shutdown_owned`](Self::shutdown_owned).
    pub fn release_session(&self, session: Session) -> ReleaseReport {
        session.finish()
    }

    /// Stop the host this lifecycle launched: the app's graceful request first (only after the
    /// host is confirmed to still be the launched generation), then up to the configured grace
    /// for the process to exit, then its process tree is killed, and only while the recorded
    /// [`AdoptedIdentity`] still names the launched process.
    ///
    /// An attached host (or no owned host) is [`LifecycleError::NotOwned`]: nothing is sent and
    /// nothing is signalled. Afterwards this lifecycle is shut down: sessions fail with
    /// [`LifecycleError::Released`] and `acquire_session` is refused.
    pub fn shutdown_owned(&self) -> Result<ShutdownReport, LifecycleError> {
        let inner = &self.inner;
        let mut slow = lock(&inner.slow);
        // Check before changing anything: a refusal leaves the lifecycle exactly as it was.
        let identity_state = match slow.owned.as_ref() {
            None => return Err(LifecycleError::NotOwned),
            Some(owned) => owned.identity.verify().map_err(LifecycleError::Process)?,
        };
        if identity_state == IdentityCheck::Replaced {
            return Err(LifecycleError::Process(io::Error::other(
                "the launched pid now names a different process; nothing was signalled",
            )));
        }
        let Some(mut owned) = slow.owned.take() else {
            return Err(LifecycleError::NotOwned);
        };
        inner.stopped.store(true, Ordering::SeqCst);
        slow.tracker.shutdown();
        slow.generation = None;

        let mut graceful_requested = false;
        let mut graceful_acknowledged = false;
        if let (Some(stop), Some(host)) = (inner.cfg.graceful.as_ref(), owned.host.as_ref()) {
            // Only a host that is still the launched generation hears the request.
            if matches!(inner.backend.is_current(host), Ok(true)) {
                if let Ok(link) = inner.backend.connect(host) {
                    graceful_requested = true;
                    graceful_acknowledged = stop(&*link).is_ok();
                    link.close();
                }
            }
        }
        let outcome = owned
            .child
            .wait_then_kill(inner.cfg.stop_grace)
            .map_err(LifecycleError::Process)?;
        let (killed, exit_code) = match outcome {
            WaitOutcome::Exited(status) => (false, status.code()),
            WaitOutcome::Terminated(status) => (true, status.code()),
        };
        drop(owned);
        drop(slow);
        // Whoever held a session is stale now.
        let mut current = lock(&inner.current);
        if current.take().is_some() {
            inner.fence.advance();
        }
        Ok(ShutdownReport {
            graceful_requested,
            graceful_acknowledged,
            killed,
            exit_code,
        })
    }
}

impl Inner {
    fn bind(&self, to: Option<(HostGeneration, Provenance)>) -> Option<Current> {
        let mut current = lock(&self.current);
        let unchanged = match (current.as_ref(), to.as_ref()) {
            (Some(c), Some((host, provenance))) => c.host == *host && c.provenance == *provenance,
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            return current.clone();
        }
        let epoch = self.fence.advance();
        *current = to.map(|(host, provenance)| Current {
            host,
            provenance,
            epoch,
        });
        current.clone()
    }

    /// `host` was observed gone or replaced: unbind it (and so advance the epoch) if it is still
    /// the bound generation.
    fn host_left(&self, host: &HostGeneration) {
        let mut current = lock(&self.current);
        if current.as_ref().is_some_and(|c| c.host == *host) {
            *current = None;
            self.fence.advance();
        }
    }

    fn release(&self, id: u64) -> usize {
        let mut sessions = lock(&self.sessions);
        sessions.remove(&id);
        sessions.len()
    }

    /// Whether the child this lifecycle launched for `host` has exited. A child that exited but
    /// was not reaped yet still looks alive to a pid probe on Unix (a zombie), so an owned host
    /// is asked through its handle. Never waits behind a launch in progress: when the slow path
    /// is busy the answer is "not known to have exited".
    fn owned_exited(&self, host: &HostGeneration) -> bool {
        let mut slow = match self.slow.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return false,
        };
        match slow.owned.as_mut() {
            Some(owned) if owned.host.as_ref() == Some(host) => {
                !matches!(owned.child.try_wait(), Ok(None))
            }
            _ => false,
        }
    }

    /// The bound, verified host; launching or relaunching one when this lifecycle may.
    fn ensure_host(&self) -> Result<Current, LifecycleError> {
        let mut slow = lock(&self.slow);
        if self.stopped.load(Ordering::SeqCst) {
            return Err(LifecycleError::Released);
        }
        let deadline = Instant::now() + self.cfg.startup_timeout;
        self.reap_exited_owned(&mut slow);
        match self.backend.discover().map_err(unavailable)? {
            Some(host) => self.adopt_found(&mut slow, host),
            None => {
                self.bind(None);
                self.launch(&mut slow, deadline)
            }
        }
    }

    /// An owned child that exited is dropped and the tracker decides about a restart.
    fn reap_exited_owned(&self, slow: &mut Slow) {
        let exited = match slow.owned.as_mut() {
            Some(owned) => match owned.child.try_wait() {
                Ok(None) => false,
                Ok(Some(_)) => true,
                Err(_) => {
                    // The state of the child cannot be read: do not guess it is fine.
                    let _ = owned.child.terminate_tree();
                    true
                }
            },
            None => false,
        };
        if exited {
            slow.owned = None;
            if let Some(generation) = slow.generation.take() {
                let at = slow.origin.elapsed();
                slow.pending = Some(slow.tracker.exited(generation, at));
            }
        }
    }

    /// A live host is published. Verify it, decide whether it is ours, and bind it.
    fn adopt_found(
        &self,
        slow: &mut Slow,
        host: HostGeneration,
    ) -> Result<Current, LifecycleError> {
        if let Some(current) = lock(&self.current).clone() {
            if current.host == host {
                return Ok(current);
            }
        }
        // Identity first: nothing is recorded, kept or signalled for a host that fails it.
        let link = self.backend.connect(&host)?;
        link.close();
        // The launched child may re-publish itself (new publication time, same pid); any other
        // pid is somebody else's host, whatever its record says.
        let ours = slow
            .owned
            .as_ref()
            .is_some_and(|owned| owned.child.id() == host.pid);
        if ours {
            if let Some(owned) = slow.owned.as_mut() {
                owned.host = Some(host.clone());
            }
        } else if slow.owned.is_some() {
            // Our own launched host is not the published one: it lost the election or is stale.
            // It is ours to stop; the published host is not.
            self.retire_owned(slow);
        }
        let provenance = if ours {
            Provenance::Owned
        } else {
            Provenance::Attached
        };
        self.bind(Some((host, provenance)))
            .ok_or(LifecycleError::NoHost)
    }

    /// Stop the child this lifecycle launched without counting it against the budget.
    fn retire_owned(&self, slow: &mut Slow) {
        if let Some(mut owned) = slow.owned.take() {
            stop_launched(&mut owned, self.cfg.stop_grace);
        }
        slow.generation = None;
    }

    /// Stop a launched child that failed to come up and let the tracker decide about a retry.
    fn fail_owned(&self, slow: &mut Slow) {
        if let Some(mut owned) = slow.owned.take() {
            stop_launched(&mut owned, self.cfg.stop_grace);
        }
        let at = slow.origin.elapsed();
        if let Some(generation) = slow.generation.take() {
            slow.pending = Some(slow.tracker.exited(generation, at));
        } else {
            slow.pending = Some(slow.tracker.request_restart(at));
        }
    }

    fn charge_failed_launch(&self, slow: &mut Slow) {
        let at = slow.origin.elapsed();
        slow.pending = Some(slow.tracker.request_restart(at));
    }

    fn launch(&self, slow: &mut Slow, deadline: Instant) -> Result<Current, LifecycleError> {
        if slow.exhausted {
            return Err(LifecycleError::RestartExhausted);
        }
        if lock(&self.launcher).is_none() {
            return Err(LifecycleError::NoHost);
        }
        if let Some(outcome) = slow.pending.take() {
            self.honour(slow, outcome, deadline)?;
        }
        let mut claim: Option<Box<dyn Any + Send>> = None;
        if slow.owned.is_none() {
            loop {
                match self.backend.claim_spawn().map_err(LifecycleError::Launch)? {
                    SpawnPermit::Granted(held) => {
                        claim = Some(held);
                        break;
                    }
                    SpawnPermit::Contended => {
                        if let Some(host) = self.backend.discover().map_err(unavailable)? {
                            return self.adopt_found(slow, host);
                        }
                        if Instant::now() >= deadline {
                            return Err(LifecycleError::LaunchTimeout);
                        }
                        std::thread::sleep(self.cfg.poll_interval);
                    }
                }
            }
            // A winner may have published between our discovery and the claim.
            if let Some(host) = self.backend.discover().map_err(unavailable)? {
                return self.adopt_found(slow, host);
            }
            self.spawn_child(slow)?;
        }
        let result = self.await_publication(slow, deadline);
        drop(claim);
        result
    }

    /// Apply what the restart tracker decided at the last exit or failed start.
    fn honour(
        &self,
        slow: &mut Slow,
        outcome: RestartOutcome,
        deadline: Instant,
    ) -> Result<(), LifecycleError> {
        let at = slow.origin.elapsed();
        let outcome = match outcome {
            // This call is the demand.
            RestartOutcome::AwaitingDemand => slow.tracker.request_restart(at),
            other => other,
        };
        match outcome {
            RestartOutcome::Restart(permit) => {
                let delay = permit.delay();
                if delay > deadline.saturating_duration_since(Instant::now()) {
                    slow.pending = Some(RestartOutcome::Restart(permit));
                    return Err(LifecycleError::Service(ServiceError::retryable(
                        "restart_backoff",
                        format!(
                            "the next start of the owned host waits {} ms (restart backoff)",
                            delay.as_millis()
                        ),
                    )));
                }
                std::thread::sleep(delay);
                if slow.tracker.permit_valid(&permit, slow.origin.elapsed()) {
                    Ok(())
                } else {
                    Err(LifecycleError::NoHost)
                }
            }
            RestartOutcome::Exhausted => {
                slow.exhausted = true;
                Err(LifecycleError::RestartExhausted)
            }
            RestartOutcome::AwaitingDemand
            | RestartOutcome::Stopped
            | RestartOutcome::IgnoredStale => Err(LifecycleError::NoHost),
        }
    }

    fn spawn_child(&self, slow: &mut Slow) -> Result<(), LifecycleError> {
        let launched = {
            let mut launcher = lock(&self.launcher);
            match launcher.as_mut() {
                Some(launch) => launch(),
                None => return Err(LifecycleError::NoHost),
            }
        };
        let mut child = match launched {
            Ok(child) => child,
            Err(e) => {
                self.charge_failed_launch(slow);
                return Err(LifecycleError::Launch(e));
            }
        };
        let Some(pid) = NonZeroU32::new(child.id()) else {
            let _ = child.terminate_tree();
            self.charge_failed_launch(slow);
            return Err(LifecycleError::Launch(io::Error::other(
                "the launcher returned a child without a process id",
            )));
        };
        let identity = match AdoptedIdentity::capture(pid) {
            Ok(Some(identity)) => identity,
            Ok(None) => {
                let _ = child.terminate_tree();
                self.charge_failed_launch(slow);
                return Err(LifecycleError::Launch(io::Error::other(
                    "the launched process exited before its identity could be recorded",
                )));
            }
            Err(e) => {
                let _ = child.terminate_tree();
                self.charge_failed_launch(slow);
                return Err(LifecycleError::Process(e));
            }
        };
        slow.generation = Some(slow.tracker.started(slow.origin.elapsed()));
        slow.owned = Some(OwnedHost {
            child,
            identity,
            host: None,
        });
        Ok(())
    }

    /// Wait for the child this lifecycle launched to publish itself.
    fn await_publication(
        &self,
        slow: &mut Slow,
        deadline: Instant,
    ) -> Result<Current, LifecycleError> {
        loop {
            let pid = match slow.owned.as_mut() {
                Some(owned) => {
                    if !matches!(owned.child.try_wait(), Ok(None)) {
                        self.fail_owned(slow);
                        return Err(LifecycleError::Launch(io::Error::other(
                            "the launched host exited before it published itself",
                        )));
                    }
                    owned.child.id()
                }
                None => return Err(LifecycleError::NoHost),
            };
            match self.backend.discover().map_err(unavailable)? {
                Some(host) if host.pid == pid => {
                    if let Err(e) = self.backend.connect(&host).map(|link| link.close()) {
                        // The program we launched is not the program we expected: it is ours.
                        self.fail_owned(slow);
                        return Err(e);
                    }
                    if let Some(owned) = slow.owned.as_mut() {
                        owned.host = Some(host.clone());
                    }
                    if let Some(generation) = slow.generation {
                        slow.tracker.ready(generation);
                    }
                    return self
                        .bind(Some((host, Provenance::Owned)))
                        .ok_or(LifecycleError::NoHost);
                }
                Some(host) => {
                    // Another host won the election: ours is useless. It is ours to stop; the
                    // winner is not.
                    self.retire_owned(slow);
                    return self.adopt_found(slow, host);
                }
                None => {}
            }
            if Instant::now() >= deadline {
                self.fail_owned(slow);
                return Err(LifecycleError::LaunchTimeout);
            }
            std::thread::sleep(self.cfg.poll_interval);
        }
    }
}

/// Stop a child this lifecycle launched. The identity gate comes first: a pid that now names a
/// different process is never signalled.
fn stop_launched(owned: &mut OwnedHost, grace: Duration) {
    if let Ok(IdentityCheck::Replaced) = owned.identity.verify() {
        return;
    }
    let _ = owned.child.wait_then_kill(grace);
}

/// One client's verified connection to the host, bound to the generation it was acquired
/// against. Dropping it releases it like [`DaemonLifecycle::release_session`].
pub struct Session {
    inner: Arc<Inner>,
    id: u64,
    label: String,
    provenance: Provenance,
    host: HostGeneration,
    epoch: u64,
    link: Box<dyn HostLink>,
    released: AtomicBool,
}

impl Session {
    pub fn provenance(&self) -> Provenance {
        self.provenance
    }

    pub fn host(&self) -> &HostGeneration {
        &self.host
    }

    /// The fence epoch this session is bound to.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    /// Whether the connection is usable: not released, not closed, and still the current epoch.
    /// A `true` is not a promise about the next call, which revalidates again.
    pub fn is_valid(&self) -> bool {
        !self.released.load(Ordering::SeqCst)
            && !self.inner.stopped.load(Ordering::SeqCst)
            && !self.link.is_closed()
            && self.inner.fence.epoch() == self.epoch
    }

    /// One request. The host generation is confirmed before the request is written and again
    /// before the reply is handed back; a reply from a host that is no longer the current one is
    /// [`LifecycleError::StaleGeneration`], never a success. Handler errors arrive as
    /// [`LifecycleError::Service`].
    pub fn call(&self, method: &str, params: Value) -> Result<Value, LifecycleError> {
        if self.released.load(Ordering::SeqCst) || self.inner.stopped.load(Ordering::SeqCst) {
            return Err(LifecycleError::Released);
        }
        self.confirm_current()?;
        let ticket = self.inner.fence.issue(self.epoch)?;
        let reply = self.link.call(method, params);
        // The reply counts only while the host is still the one this session was bound to.
        self.confirm_current()?;
        self.inner.fence.admit_reply(&ticket)?;
        reply.map_err(LifecycleError::Service)
    }

    fn confirm_current(&self) -> Result<(), LifecycleError> {
        let left = if self.provenance == Provenance::Owned && self.inner.owned_exited(&self.host) {
            true
        } else {
            match self.inner.backend.is_current(&self.host) {
                Ok(true) => false,
                Ok(false) => true,
                Err(_) => return Err(LifecycleError::Unconfirmed),
            }
        };
        if left {
            self.inner.host_left(&self.host);
            return Err(LifecycleError::StaleGeneration {
                issued: self.epoch,
                current: self.inner.fence.epoch(),
            });
        }
        Ok(())
    }

    fn finish(&self) -> ReleaseReport {
        let first = !self.released.swap(true, Ordering::SeqCst);
        let remaining = if first {
            self.inner.release(self.id)
        } else {
            lock(&self.inner.sessions).len()
        };
        self.link.close();
        ReleaseReport {
            label: self.label.clone(),
            provenance: self.provenance,
            remaining_sessions: remaining,
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_child_process::RestartMode;
    use serde_json::json;
    use std::sync::atomic::AtomicUsize;

    fn generation(pid: u32, started_ms: u64) -> HostGeneration {
        HostGeneration {
            service: "svc".into(),
            endpoint: "endpoint".into(),
            pid,
            process_started_ms: 1_000,
            started_ms,
        }
    }

    #[derive(Default)]
    struct FakeState {
        host: Option<HostGeneration>,
        refuse_identity: bool,
        contended: bool,
        calls: usize,
        stop_requests: usize,
        /// Replaces the published host while the next call is in flight.
        replace_during_call: Option<Option<HostGeneration>>,
    }

    struct Fake {
        state: Arc<Mutex<FakeState>>,
    }

    struct FakeLink {
        state: Arc<Mutex<FakeState>>,
        host: HostGeneration,
        closed: AtomicBool,
    }

    impl Fake {
        fn new(host: Option<HostGeneration>) -> Arc<Self> {
            Arc::new(Self {
                state: Arc::new(Mutex::new(FakeState {
                    host,
                    ..FakeState::default()
                })),
            })
        }

        fn publish(&self, host: Option<HostGeneration>) {
            lock(&self.state).host = host;
        }
    }

    impl HostLink for FakeLink {
        fn call(&self, method: &str, _params: Value) -> Result<Value, ServiceError> {
            let mut state = lock(&self.state);
            state.calls += 1;
            if method == "stop" {
                state.stop_requests += 1;
            }
            if let Some(next) = state.replace_during_call.take() {
                state.host = next;
            }
            Ok(json!({"pid": self.host.pid}))
        }

        fn is_closed(&self) -> bool {
            self.closed.load(Ordering::SeqCst)
        }

        fn close(&self) {
            self.closed.store(true, Ordering::SeqCst);
        }
    }

    impl HostBackend for Fake {
        fn discover(&self) -> io::Result<Option<HostGeneration>> {
            Ok(lock(&self.state).host.clone())
        }

        fn connect(&self, host: &HostGeneration) -> Result<Box<dyn HostLink>, LifecycleError> {
            let state = lock(&self.state);
            if state.refuse_identity {
                return Err(LifecycleError::HostIdentity(HostIdentityError {
                    expected: Vec::new(),
                    found_pid: Some(host.pid),
                    found_executable: None,
                    reason: HostIdentityReason::ExecutableMismatch,
                }));
            }
            if state.host.as_ref() != Some(host) {
                return Err(LifecycleError::Service(ServiceError::retryable(
                    ERROR_UNAVAILABLE,
                    "gone",
                )));
            }
            Ok(Box::new(FakeLink {
                state: self.state.clone(),
                host: host.clone(),
                closed: AtomicBool::new(false),
            }))
        }

        fn claim_spawn(&self) -> io::Result<SpawnPermit> {
            if lock(&self.state).contended {
                Ok(SpawnPermit::Contended)
            } else {
                Ok(SpawnPermit::Granted(Box::new(())))
            }
        }
    }

    fn attach(fake: &Arc<Fake>) -> DaemonLifecycle {
        DaemonLifecycle::attach_or_launch(fake.clone(), None, LifecycleConfig::new()).unwrap()
    }

    #[test]
    fn an_attached_host_is_never_owned_and_never_stopped() {
        let fake = Fake::new(Some(generation(10, 1)));
        let life = attach(&fake);
        assert_eq!(life.provenance(), Some(Provenance::Attached));
        assert!(!Provenance::Attached.may_stop_host());
        let session = life.acquire_session("shell").unwrap();
        assert_eq!(session.provenance(), Provenance::Attached);
        // The refusal sends nothing, signals nothing and leaves the lifecycle usable.
        let error = life.shutdown_owned().unwrap_err();
        assert!(matches!(error, LifecycleError::NotOwned), "{error}");
        assert_eq!(error.code(), ERROR_NOT_OWNED);
        assert_eq!(lock(&fake.state).stop_requests, 0);
        assert_eq!(lock(&fake.state).host, Some(generation(10, 1)));
        assert_eq!(session.call("echo", json!({})).unwrap(), json!({"pid": 10}));
    }

    #[test]
    fn a_host_that_fails_the_identity_check_is_not_attached() {
        let fake = Fake::new(Some(generation(10, 1)));
        lock(&fake.state).refuse_identity = true;
        let error = DaemonLifecycle::attach_or_launch(fake.clone(), None, LifecycleConfig::new())
            .err()
            .expect("an impostor is refused");
        assert!(matches!(error, LifecycleError::HostIdentity(_)), "{error}");
        assert!(!error.is_retryable());
        assert_eq!(lock(&fake.state).calls, 0, "nothing was sent to it");
    }

    #[test]
    fn no_host_and_no_launcher_is_a_retryable_no_host() {
        let fake = Fake::new(None);
        let error = DaemonLifecycle::attach_or_launch(fake, None, LifecycleConfig::new())
            .err()
            .expect("nothing to attach to");
        assert!(matches!(error, LifecycleError::NoHost));
        assert!(error.is_retryable());
        assert_eq!(error.into_service_error().code, ERROR_UNAVAILABLE);
    }

    #[test]
    fn two_shells_attach_and_one_detaching_leaves_the_other_running() {
        let fake = Fake::new(Some(generation(10, 1)));
        let (a, b) = (attach(&fake), attach(&fake));
        let session_a = a.acquire_session("shell-a").unwrap();
        let session_b = b.acquire_session("shell-b").unwrap();
        assert_eq!(a.session_count(), 1);
        assert_eq!(b.session_count(), 1);
        assert_eq!(a.host(), b.host());
        let report = a.release_session(session_a);
        assert_eq!(report.provenance, Provenance::Attached);
        assert_eq!(report.remaining_sessions, 0);
        // The host is untouched: the other shell still gets answers.
        assert_eq!(lock(&fake.state).host, Some(generation(10, 1)));
        assert_eq!(
            session_b.call("echo", json!({})).unwrap(),
            json!({"pid": 10})
        );
        assert_eq!(b.session_count(), 1);
    }

    #[test]
    fn a_dropped_session_leaves_the_count() {
        let fake = Fake::new(Some(generation(10, 1)));
        let life = attach(&fake);
        {
            let _held = life.acquire_session("shell").unwrap();
            assert_eq!(life.session_count(), 1);
        }
        assert_eq!(life.session_count(), 0);
    }

    #[test]
    fn when_the_owner_exits_the_session_is_stale_and_there_is_no_host() {
        let fake = Fake::new(Some(generation(10, 1)));
        let life = attach(&fake);
        let session = life.acquire_session("shell").unwrap();
        let bound = session.epoch();
        fake.publish(None);
        match session.call("echo", json!({})) {
            Err(LifecycleError::StaleGeneration { issued, current }) => {
                assert_eq!(issued, bound);
                assert!(current > issued);
            }
            other => panic!("expected a stale generation, got {other:?}"),
        }
        assert!(!session.is_valid());
        let error = life
            .acquire_session("shell")
            .err()
            .expect("no host any more");
        assert!(matches!(error, LifecycleError::NoHost));
        assert_eq!(life.provenance(), None);
    }

    #[test]
    fn a_replacement_host_is_a_new_generation_and_the_old_session_stays_stale() {
        let fake = Fake::new(Some(generation(10, 1)));
        let life = attach(&fake);
        let old = life.acquire_session("shell").unwrap();
        fake.publish(Some(generation(11, 2)));
        let new = life.acquire_session("shell").unwrap();
        assert_eq!(new.host().pid, 11);
        assert!(new.epoch() > old.epoch());
        assert!(matches!(
            old.call("echo", json!({})),
            Err(LifecycleError::StaleGeneration { .. })
        ));
        assert_eq!(new.call("echo", json!({})).unwrap(), json!({"pid": 11}));
        assert_eq!(life.provenance(), Some(Provenance::Attached));
    }

    #[test]
    fn a_reply_that_arrives_after_the_host_was_replaced_is_not_delivered() {
        let fake = Fake::new(Some(generation(10, 1)));
        let life = attach(&fake);
        let session = life.acquire_session("shell").unwrap();
        // The host is replaced while this request is in flight on the old one.
        lock(&fake.state).replace_during_call = Some(Some(generation(11, 2)));
        let outcome = session.call("echo", json!({}));
        assert!(
            matches!(outcome, Err(LifecycleError::StaleGeneration { .. })),
            "{outcome:?}"
        );
        // Not replayed: the old host saw the one call, the new host none.
        assert_eq!(lock(&fake.state).calls, 1);
    }

    #[test]
    fn the_fence_refuses_a_stale_ticket_and_a_stale_reply_epoch() {
        let fence = GenerationFence::new();
        let first = fence.advance();
        let old = fence.issue(first).unwrap();
        assert!(fence.admit_reply(&old).is_ok());
        let second = fence.advance();
        assert!(second > first);
        // The old request can no longer complete, and nothing can be issued under the old epoch.
        assert_eq!(
            fence.admit_reply(&old),
            Err(Stale {
                issued: first,
                current: second
            })
        );
        assert!(fence.issue(first).is_err());
        let new = fence.issue(second).unwrap();
        assert_ne!(old.request(), new.request());
        // A reply read from the old connection cannot answer the new request.
        assert!(fence.admit_reply_from(&new, first).is_err());
        assert!(fence.admit_reply_from(&new, second).is_ok());
    }

    #[test]
    fn a_launcher_that_keeps_failing_spends_the_restart_budget() {
        let fake = Fake::new(Some(generation(10, 1)));
        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = attempts.clone();
        let launcher: Launcher = Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Err(io::Error::other("no such engine"))
        });
        let policy = RestartPolicy::new(
            RestartMode::Automatic,
            2,
            Duration::from_secs(60),
            Duration::from_millis(1),
            Duration::from_millis(2),
        )
        .unwrap();
        let config = LifecycleConfig::new()
            .restart_policy(policy)
            .startup_timeout(Duration::from_secs(5))
            .poll_interval(Duration::from_millis(1));
        let life = DaemonLifecycle::attach_or_launch(fake.clone(), Some(launcher), config).unwrap();
        // The attached host goes away; this lifecycle may start its own, within the budget.
        fake.publish(None);
        for _ in 0..3 {
            let error = life.acquire_session("shell").err().expect("launch fails");
            assert!(matches!(error, LifecycleError::Launch(_)), "{error}");
        }
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        let error = life.acquire_session("shell").err().expect("budget spent");
        assert!(matches!(error, LifecycleError::RestartExhausted), "{error}");
        assert_eq!(error.code(), ERROR_RESTART_EXHAUSTED);
        // Spent stays spent, and nothing more is launched.
        let again = life.acquire_session("shell").err().expect("still spent");
        assert!(matches!(again, LifecycleError::RestartExhausted));
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        // An explicit start forgets the budget.
        life.reset_restart_budget();
        let after = life
            .acquire_session("shell")
            .err()
            .expect("launch fails again");
        assert!(matches!(after, LifecycleError::Launch(_)), "{after}");
        assert_eq!(attempts.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn a_contended_claim_waits_for_the_winner_and_attaches() {
        let fake = Fake::new(None);
        lock(&fake.state).contended = true;
        let launcher: Launcher = Box::new(|| Err(io::Error::other("must not launch")));
        let publisher = {
            let fake = fake.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                fake.publish(Some(generation(20, 5)));
            })
        };
        let config = LifecycleConfig::new()
            .startup_timeout(Duration::from_secs(10))
            .poll_interval(Duration::from_millis(5));
        let life = DaemonLifecycle::attach_or_launch(fake, Some(launcher), config).unwrap();
        publisher.join().unwrap();
        assert_eq!(life.provenance(), Some(Provenance::Attached));
        assert_eq!(life.host().map(|h| h.pid), Some(20));
    }

    #[test]
    fn errors_collapse_to_wire_codes() {
        assert_eq!(
            LifecycleError::StaleGeneration {
                issued: 1,
                current: 2
            }
            .into_service_error()
            .code,
            ERROR_STALE_GENERATION
        );
        assert_eq!(
            LifecycleError::NotOwned.into_service_error().code,
            ERROR_NOT_OWNED
        );
        assert_eq!(
            LifecycleError::Unconfirmed.into_service_error().code,
            ERROR_STALE_GENERATION
        );
        assert!(!LifecycleError::Unconfirmed.is_retryable());
    }
}
