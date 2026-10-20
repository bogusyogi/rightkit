//! Engine sidecar wiring over `rightkit-job-queue::EngineClient` and `rightkit-child-process`.
//!
//! The shell owns only the window-side seam: start the engine off the UI thread, expose typed
//! calls to the webview (`rk_shell_engine_call`), stream engine events to the webview, restart on
//! demand, and shut the engine down in the ordered exit. Framing, multiplexing and the bounded
//! shutdown are `EngineClient`; child ownership is `rightkit-child-process`. Donor: HeardRight
//! `engine.rs` / `engine_events.rs` (event bridge) and the template's `engine_supervisor`.
//!
//! # Host composition (PK06-3, K06)
//!
//! The app supplies the command (`EngineSpec`), the method allowlist and deadlines
//! (`EnginePolicy`) and, when it has any, the snapshot slots. The host owns the rest of the
//! lifecycle, once:
//!
//! * **Startup and health**: `start`, the `lifecycle()` state replayed to a late subscriber, the
//!   job-queue supervisor's restart budget and handshake underneath.
//! * **Presence and idle exit** ([`PresencePolicy`], [`EngineHost::hold`]): who needs the engine
//!   is a count of holders (a window, a CLI session) plus calls in flight. Once something has used
//!   the engine and the count has been zero for `idle_exit`, the engine is stopped; the next hold
//!   or call starts it again. An engine nobody ever used is never idled out (CodeRight's rule). An
//!   idle stop is not a shutdown and not a crash: no restart budget is spent.
//! * **A generation latch** (`rightkit_daemon::lifecycle::GenerationFence`): every call is
//!   bound to the engine generation it was issued under. A reply that arrives after the generation
//!   changed is not delivered ([`ERROR_STALE_GENERATION`]); its events stop at once. Whether the
//!   request ran on the old engine is unknowable, so it is never replayed.
//! * **Snapshots** ([`SnapshotSlot`], [`EngineHost::set_snapshot`]): the latest value of each named
//!   slot (init, settings, entitlements, auth) is registered in declaration order, with
//!   a `required` slot holding back the slots after it until it has a value (ScrapeRight's
//!   "`Init` first"), and replayed to every engine generation before it is `Ready`.
//!   [`EngineHost::revoke_snapshot`] is the revoking variant: new calls are refused while it
//!   applies, and an engine that does not acknowledge it is replaced, so a revoked entitlement
//!   cannot stay live in a running engine.
//! * **Ordered shutdown** ([`EngineHost::shutdown_ordered`]): refuse new calls and holds, let calls
//!   in flight finish (bounded by [`EnginePolicy::shutdown_drain`]), stop the engine (ask, grace,
//!   kill), report what happened.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rightkit_child_process::OwnedCommand;
pub use rightkit_child_process::{RestartMode, RestartPolicy};
pub use rightkit_daemon::lifecycle::ERROR_STALE_GENERATION;
use rightkit_daemon::lifecycle::{GenerationFence, Stale, Ticket};
use rightkit_job_queue::{
    CallOptions, EngineClient, EngineSupervisor, JobError, Lifecycle, SupervisorConfig, Terminal,
};
pub use rightkit_job_queue::{EventFrame, LifecycleEvent};
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Wry};

use crate::EVENT_ENGINE;

pub type CommandFactory = Arc<dyn Fn() -> OwnedCommand + Send + Sync>;

/// The code of the refusal new calls get while a revoking snapshot is applied.
pub const ERROR_REVOCATION_PENDING: &str = "revocation_pending";
/// The code of the refusal for a snapshot key the policy did not declare.
pub const ERROR_SNAPSHOT_NOT_DECLARED: &str = "snapshot_not_declared";
/// Largest snapshot value, serialised.
pub const MAX_SNAPSHOT_BYTES: usize = 1024 * 1024;
/// How long [`EngineHost::shutdown_ordered`] waits for calls in flight unless the policy says.
pub const DEFAULT_SHUTDOWN_DRAIN: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct EngineSpec {
    pub command: CommandFactory,
    pub hello_timeout: Duration,
    pub shutdown_grace: Duration,
    /// Per-call deadline. `None` is unbounded in legacy mode; supervised mode uses
    /// the configured supervisor RPC timeout (120 s unless configured otherwise).
    pub call_timeout: Option<Duration>,
}

impl EngineSpec {
    pub fn new(command: impl Fn() -> OwnedCommand + Send + Sync + 'static) -> Self {
        Self {
            command: Arc::new(command),
            hello_timeout: Duration::from_secs(30),
            shutdown_grace: Duration::from_secs(3),
            call_timeout: Some(Duration::from_secs(60)),
        }
    }
}

/// Who needs the engine, and when it may leave. See [`EngineHost::hold`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresencePolicy {
    /// Stop the engine once it has been used and nothing has held it or called it for this long.
    pub idle_exit: Duration,
}

impl PresencePolicy {
    /// Stop the engine after it has been unused for `idle_exit`.
    pub fn idle_exit(idle_exit: Duration) -> Self {
        Self { idle_exit }
    }
}

/// One named snapshot the app keeps current: the latest value replays to every engine generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotSlot {
    /// What the app calls it (`"init"`, `"settings"`); never sent to the engine.
    pub key: String,
    /// The engine method the value is sent to.
    pub method: String,
    /// Slots declared after this one are held back until this one has a value.
    pub required: bool,
}

impl SnapshotSlot {
    pub fn new(key: &str, method: &str) -> Self {
        Self {
            key: key.to_owned(),
            method: method.to_owned(),
            required: false,
        }
    }

    /// Hold the slots declared after this one back until it has a value.
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }
}

/// Shell engine policy. The default denies every method: list the methods the webview may
/// call with [`EnginePolicy::allow_methods`], or opt into unrestricted calls explicitly with
/// [`EnginePolicy::allow_any_method`].
#[derive(Clone, Default)]
pub struct EnginePolicy {
    /// Methods the webview may call. `None` (the default) denies every method unless
    /// [`EnginePolicy::allow_any`] is set.
    pub allowed_methods: Option<HashSet<String>>,
    /// Explicit opt-in to unrestricted calls; ignored when `allowed_methods` is set.
    pub allow_any: bool,
    /// Exact method names; missing entries use `EngineSpec::call_timeout`.
    pub method_timeouts: HashMap<String, Duration>,
    /// Opt into job-queue's bounded supervisor (automatic, on-demand or disabled).
    pub restart: Option<RestartPolicy>,
    /// Supervised calls abandon only their wait on timeout instead of killing the child.
    pub keep_alive_on_timeout: bool,
    /// Optional contract, stderr, readiness & handshake configuration. Also enables supervision.
    pub configure_supervisor:
        Option<Arc<dyn Fn(SupervisorConfig) -> SupervisorConfig + Send + Sync>>,
    /// Presence and idle exit. `None` (the default): the engine runs until shutdown.
    pub presence: Option<PresencePolicy>,
    /// The snapshot slots, in the order they are registered. Declaring any enables supervision.
    pub snapshots: Vec<SnapshotSlot>,
    /// How long ordered shutdown waits for calls in flight (default [`DEFAULT_SHUTDOWN_DRAIN`]).
    pub shutdown_drain: Option<Duration>,
}

impl EnginePolicy {
    pub fn allow_methods(mut self, methods: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.allowed_methods = Some(methods.into_iter().map(Into::into).collect());
        self
    }

    /// Explicit opt-in: the webview may call every engine method. Prefer
    /// [`EnginePolicy::allow_methods`]; this exists for engines whose whole surface is app-safe.
    pub fn allow_any_method(mut self) -> Self {
        self.allow_any = true;
        self
    }

    pub fn method_timeout(mut self, method: impl Into<String>, timeout: Duration) -> Self {
        self.method_timeouts.insert(method.into(), timeout);
        self
    }

    pub fn restart_policy(mut self, policy: RestartPolicy) -> Self {
        self.restart = Some(policy);
        self
    }

    pub fn supervisor_config(
        mut self,
        configure: impl Fn(SupervisorConfig) -> SupervisorConfig + Send + Sync + 'static,
    ) -> Self {
        self.configure_supervisor = Some(Arc::new(configure));
        self
    }

    /// Stop the engine when it is unused (see [`PresencePolicy`]).
    pub fn presence(mut self, presence: PresencePolicy) -> Self {
        self.presence = Some(presence);
        self
    }

    /// Declare a snapshot slot. Slots register in declaration order.
    ///
    /// # Panics
    /// Panics on an empty key or method, or a key declared twice: a programmer error at startup.
    pub fn snapshot(mut self, slot: SnapshotSlot) -> Self {
        assert!(
            !slot.key.is_empty() && !slot.method.is_empty(),
            "rightkit-tauri-shell: a snapshot slot needs a key and a method"
        );
        assert!(
            !self.snapshots.iter().any(|s| s.key == slot.key),
            "rightkit-tauri-shell: snapshot slot {:?} is declared twice",
            slot.key
        );
        self.snapshots.push(slot);
        self
    }

    /// How long ordered shutdown waits for calls in flight before it stops the engine anyway.
    pub fn shutdown_drain(mut self, drain: Duration) -> Self {
        self.shutdown_drain = Some(drain);
        self
    }

    /// Validate at the native boundary, before obtaining or starting a client.
    pub fn check_method(&self, method: &str) -> Result<(), JobError> {
        let allowed = match &self.allowed_methods {
            Some(methods) => methods.contains(method),
            None => self.allow_any,
        };
        if !allowed {
            return Err(JobError::supervisor(
                "method_not_allowed",
                format!("method {method:?} is not available to the app"),
            ));
        }
        Ok(())
    }

    pub fn timeout_for(&self, method: &str, fallback: Option<Duration>) -> Option<Duration> {
        self.method_timeouts.get(method).copied().or(fallback)
    }
}

/// Additive fields retain the existing `{call, event}` event contract.
#[derive(Debug, Clone, Serialize)]
pub struct EngineEvent {
    pub call: String,
    pub id: Option<u64>,
    pub tag: Option<String>,
    pub event: Value,
}

/// Typed error returned by the tagged IPC command. Legacy IPC keeps string errors.
#[derive(Debug, Clone, Serialize)]
pub struct EngineCallError {
    pub code: String,
    pub message: String,
    /// Distinguishes pre-execution readiness errors from engine replies with matching codes.
    pub origin: &'static str,
    /// Structured M4 error details carried through from the job-queue error, when present.
    /// Absent details stay absent on the wire (never serialised as `null`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl From<JobError> for EngineCallError {
    fn from(e: JobError) -> Self {
        Self {
            details: e.details().cloned(),
            code: e.code,
            message: e.message,
            origin: if e.origin == rightkit_job_queue::ErrorOrigin::Supervisor {
                "supervisor"
            } else {
                "engine"
            },
        }
    }
}

pub type EngineEventSink = Arc<dyn Fn(EngineEvent) + Send + Sync>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn shutting_down() -> JobError {
    JobError::supervisor("shutting_down", "the app is exiting")
}

// ---- presence gate ---------------------------------------------------------------------------

/// Where the engine process is, as presence sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunPhase {
    Running,
    /// Being stopped for idleness.
    Stopping,
    /// Stopped for idleness, or not started yet; the next hold or call starts it.
    Idle,
    Starting,
}

#[derive(Debug)]
struct GateState {
    /// Shared app-lock fence, checked under this same mutex as admission counts.
    app_locked: bool,
    holders: usize,
    busy: usize,
    /// Something has held or called the engine at least once.
    ever_used: bool,
    zero_since: Option<Instant>,
    phase: RunPhase,
    closed: bool,
}

impl GateState {
    /// When the engine becomes due for an idle stop, if it is idle-eligible at all. Pure.
    fn due_at(&self, idle_exit: Duration) -> Option<Instant> {
        if self.closed
            || !self.ever_used
            || self.holders > 0
            || self.busy > 0
            || self.phase != RunPhase::Running
        {
            return None;
        }
        self.zero_since.map(|t| t + idle_exit)
    }
}

struct Gate {
    state: Mutex<GateState>,
    cv: Condvar,
}

impl Gate {
    fn new(presence: bool) -> Self {
        Self {
            state: Mutex::new(GateState {
                app_locked: false,
                holders: 0,
                busy: 0,
                ever_used: false,
                zero_since: None,
                // Without presence the phase is never consulted.
                phase: if presence {
                    RunPhase::Idle
                } else {
                    RunPhase::Running
                },
                closed: false,
            }),
            cv: Condvar::new(),
        }
    }

    fn state(&self) -> MutexGuard<'_, GateState> {
        lock(&self.state)
    }

    /// Block until the engine is due for an idle stop (`true`) or the gate is closed (`false`).
    fn wait_idle_due(&self, idle_exit: Duration) -> bool {
        let mut g = self.state();
        loop {
            if g.closed {
                return false;
            }
            match g.due_at(idle_exit) {
                None => {
                    g = self.cv.wait(g).unwrap_or_else(|e| e.into_inner());
                }
                Some(at) => {
                    let now = Instant::now();
                    if now >= at {
                        return true;
                    }
                    g = self
                        .cv
                        .wait_timeout(g, at - now)
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
            }
        }
    }
}

/// Keeps the engine in use until dropped. Made by [`EngineHost::hold`].
pub struct EngineHold {
    host: Arc<EngineHost>,
    label: String,
}

impl EngineHold {
    /// What the holder called itself.
    pub fn label(&self) -> &str {
        &self.label
    }
}

impl std::fmt::Debug for EngineHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineHold")
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

impl Drop for EngineHold {
    fn drop(&mut self) {
        self.host.release(false);
    }
}

/// A call in flight: counted as use until it ends, however it ends.
struct Busy<'a>(&'a EngineHost);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.release(true);
    }
}

// ---- generation latch ------------------------------------------------------------------------

/// Binds calls to the engine generation: advances when the generation the host observes changes.
#[derive(Default)]
pub(crate) struct GenerationLatch {
    fence: Arc<GenerationFence>,
    seen: Mutex<u64>,
}

impl GenerationLatch {
    /// Record the generation now observed; the epoch calls issue under.
    pub(crate) fn observe(&self, generation: u64) -> u64 {
        let mut seen = lock(&self.seen);
        if generation != *seen {
            *seen = generation;
            self.fence.advance();
        }
        self.fence.epoch()
    }

    pub(crate) fn issue(&self, epoch: u64) -> Result<Ticket, Stale> {
        self.fence.issue(epoch)
    }

    /// Deliver `result` only while `ticket`'s generation is still current. An `Err` passes through.
    pub(crate) fn admit(
        &self,
        ticket: &Ticket,
        result: Result<Value, JobError>,
    ) -> Result<Value, JobError> {
        match result {
            Ok(value) => self
                .fence
                .admit_reply(ticket)
                .map(|()| value)
                .map_err(stale_error),
            err => err,
        }
    }

    pub(crate) fn fence(&self) -> Arc<GenerationFence> {
        Arc::clone(&self.fence)
    }
}

fn stale_error(s: Stale) -> JobError {
    JobError::supervisor(
        ERROR_STALE_GENERATION,
        format!(
            "the engine was replaced while the call ran (issued under generation {}, now {}); \
             it may have run and is not repeated",
            s.issued, s.current
        ),
    )
}

// ---- snapshots -------------------------------------------------------------------------------

#[derive(Default)]
struct SnapshotBoard {
    values: BTreeMap<String, Value>,
    registered: HashSet<String>,
}

/// What [`EngineHost::shutdown_ordered`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineShutdownReport {
    /// This call performed the shutdown (a second call finds it done).
    pub first: bool,
    /// Every call in flight had finished before the engine was stopped.
    pub drained: bool,
    /// Calls still running when the drain bound passed (the engine stop ends them).
    pub abandoned_calls: usize,
    /// Holders that had not let go.
    pub holders_left: usize,
    /// How the supervised engine process ended, when there was one.
    pub engine: Option<rightkit_job_queue::ShutdownReport>,
}

pub struct EngineHost {
    spec: EngineSpec,
    policy: EnginePolicy,
    supervisor: Option<EngineSupervisor>,
    client: Mutex<Option<Arc<EngineClient>>>,
    closing: AtomicBool,
    lifecycle: Mutex<Value>,
    gate: Arc<Gate>,
    latch: GenerationLatch,
    /// Boots of the unsupervised engine (the supervised one counts its own).
    legacy_generation: AtomicU64,
    snapshots: Mutex<SnapshotBoard>,
    revoking: AtomicUsize,
    stale_frames: Arc<AtomicU64>,
    monitor_started: AtomicBool,
}

impl EngineHost {
    /// A host with the default policy, which denies every method; use [`EngineHost::with_policy`]
    /// to permit calls.
    pub fn new(spec: EngineSpec) -> Self {
        Self::with_policy(spec, EnginePolicy::default())
    }

    pub fn with_policy(spec: EngineSpec, policy: EnginePolicy) -> Self {
        // Snapshots are replayed by the supervisor: declaring any means supervision.
        let supervised = policy.restart.is_some()
            || policy.configure_supervisor.is_some()
            || !policy.snapshots.is_empty();
        let supervisor = if supervised {
            let command = spec.command.clone();
            let mut config = SupervisorConfig::new("rk-shell-engine", move || Ok(command()))
                .with_hello_timeout(spec.hello_timeout)
                .with_shutdown_grace(spec.shutdown_grace);
            if let Some(t) = spec.call_timeout {
                config = config.with_rpc_timeout(t);
            }
            if let Some(restart) = policy.restart {
                config = config.with_restart(restart);
            }
            if let Some(configure) = &policy.configure_supervisor {
                config = configure(config);
            }
            Some(EngineSupervisor::new(config))
        } else {
            None
        };
        let presence = policy.presence.is_some();
        Self {
            spec,
            policy,
            supervisor,
            client: Mutex::new(None),
            closing: Default::default(),
            lifecycle: Mutex::new(serde_json::json!({"state": "starting"})),
            gate: Arc::new(Gate::new(presence)),
            latch: GenerationLatch::default(),
            legacy_generation: AtomicU64::new(0),
            snapshots: Mutex::new(SnapshotBoard::default()),
            revoking: AtomicUsize::new(0),
            stale_frames: Arc::new(AtomicU64::new(0)),
            monitor_started: AtomicBool::new(false),
        }
    }

    /// The policy this host was built with.
    pub fn policy(&self) -> &EnginePolicy {
        &self.policy
    }

    /// Spawn the engine (blocking until hello). Replaces a previous client.
    ///
    /// With a [`PresencePolicy`] this is idempotent: a running engine answers its hello, and a
    /// start already in flight is waited for, so the shell's startup and a first call cannot both
    /// spawn one.
    pub fn start(&self) -> Result<Value, JobError> {
        if self.policy.presence.is_none() {
            return self.start_inner();
        }
        let mut g = self.gate.state();
        loop {
            if g.closed || self.closing.load(Ordering::SeqCst) {
                return Err(shutting_down());
            }
            match g.phase {
                RunPhase::Running => {
                    drop(g);
                    return Ok(self.hello().unwrap_or(Value::Null));
                }
                RunPhase::Idle => {
                    g.phase = RunPhase::Starting;
                    drop(g);
                    let started = self.start_inner();
                    let mut g2 = self.gate.state();
                    g2.phase = if started.is_ok() {
                        RunPhase::Running
                    } else {
                        RunPhase::Idle
                    };
                    drop(g2);
                    self.gate.cv.notify_all();
                    return started;
                }
                RunPhase::Starting | RunPhase::Stopping => {
                    g = self
                        .gate
                        .cv
                        .wait_timeout(g, Duration::from_millis(200))
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
            }
        }
    }

    /// The spawn itself, without the presence protocol.
    fn start_inner(&self) -> Result<Value, JobError> {
        if self.closing.load(Ordering::SeqCst) {
            return Err(shutting_down());
        }
        if let Some(supervisor) = &self.supervisor {
            supervisor.start()?;
            return supervisor
                .status()
                .hello
                .ok_or_else(|| JobError::supervisor("engine_not_running", "engine has no hello"));
        }
        let client = Arc::new(EngineClient::spawn_with(
            (self.spec.command)(),
            self.spec.hello_timeout,
        )?);
        let hello = client.hello().clone();
        self.legacy_generation.fetch_add(1, Ordering::SeqCst);
        *lock(&self.client) = Some(client);
        Ok(hello)
    }

    /// Last start outcome (`{state: starting|ready|error|idle, ...}`). The start event can fire
    /// before the webview subscribes, so `rk_shell_ready` & `rk_shell_engine_status` replay this.
    pub fn lifecycle(&self) -> Value {
        let idle = self.policy.presence.is_some() && {
            let g = self.gate.state();
            !g.closed && g.phase == RunPhase::Idle && g.ever_used
        };
        if let Some(s) = &self.supervisor {
            let status = s.status();
            if idle {
                return serde_json::json!({"state": "idle", "hello": Value::Null,
                    "restarts": status.restarts, "last_error": Value::Null,
                    "generation": status.generation});
            }
            let state = match status.lifecycle {
                Lifecycle::Booting => "starting",
                Lifecycle::Ready => "ready",
                Lifecycle::Recovering => "restarting",
                Lifecycle::Down => match &status.terminal {
                    Some(Terminal::Stopped) => "stopped",
                    Some(Terminal::Failed { .. }) => "failed",
                    None if status.generation == 0 => "starting",
                    None => "restarting",
                },
            };
            return serde_json::json!({"state": state, "hello": status.hello,
            "generation": status.generation,
            "restarts": status.restarts, "last_error": match status.terminal {
                Some(Terminal::Failed { reason }) => Some(reason),
                _ if status.lifecycle == Lifecycle::Ready => None,
                _ => status.last_exit_reason,
            }});
        }
        if idle {
            return serde_json::json!({"state": "idle",
                "generation": self.legacy_generation.load(Ordering::SeqCst)});
        }
        lock(&self.lifecycle).clone()
    }

    pub(crate) fn set_lifecycle(&self, v: Value) {
        *lock(&self.lifecycle) = v;
    }

    pub fn hello(&self) -> Option<Value> {
        if let Some(s) = &self.supervisor {
            return s.status().hello;
        }
        lock(&self.client).as_ref().map(|c| c.hello().clone())
    }

    /// The engine generation now: boots of the supervised engine, or of the plain one.
    pub fn generation(&self) -> u64 {
        match &self.supervisor {
            Some(s) => s.status().generation,
            None => self.legacy_generation.load(Ordering::SeqCst),
        }
    }

    /// Calls in flight.
    pub fn in_flight(&self) -> usize {
        self.gate.state().busy
    }

    /// Holders that have not let go.
    pub fn holders(&self) -> usize {
        self.gate.state().holders
    }

    /// Events dropped because their call's generation had passed.
    pub fn stale_frames_dropped(&self) -> u64 {
        self.stale_frames.load(Ordering::Relaxed)
    }

    fn client(&self) -> Result<Arc<EngineClient>, JobError> {
        if self.closing.load(Ordering::SeqCst) {
            return Err(shutting_down());
        }
        lock(&self.client)
            .clone()
            .ok_or_else(|| JobError::supervisor("engine_not_running", "engine has not started"))
    }

    /// Close admission before native session revocation. Existing calls must drain before
    /// Locked may be announced; timeout leaves admission closed & coordinator Faulted.
    #[cfg(feature = "app-lock")]
    pub(crate) fn app_lock_revoke(&self) -> Result<(), crate::app_lock::EngineLockError> {
        let deadline =
            Instant::now() + self.policy.shutdown_drain.unwrap_or(DEFAULT_SHUTDOWN_DRAIN);
        let mut g = self.gate.state();
        g.app_locked = true;
        self.gate.cv.notify_all();
        while g.busy > 0 {
            let now = Instant::now();
            if now >= deadline {
                return Err(crate::app_lock::EngineLockError);
            }
            g = self
                .gate
                .cv
                .wait_timeout(g, deadline - now)
                .map_err(|_| crate::app_lock::EngineLockError)?
                .0;
        }
        Ok(())
    }

    /// Only native coordinator's verified path calls this after daemon/session resume succeeds.
    #[cfg(feature = "app-lock")]
    pub(crate) fn app_lock_resume(&self) -> Result<(), crate::app_lock::EngineLockError> {
        let mut g = self.gate.state();
        if g.closed || self.closing.load(Ordering::SeqCst) {
            return Err(crate::app_lock::EngineLockError);
        }
        g.app_locked = false;
        self.gate.cv.notify_all();
        Ok(())
    }

    // ---- presence ----

    /// Count `label` as a user of the engine until the returned guard drops; starts an idle-stopped
    /// engine first. `label` is for the host's own logs.
    pub fn hold(self: &Arc<Self>, label: &str) -> Result<EngineHold, JobError> {
        self.admit(false)?;
        Ok(EngineHold {
            host: Arc::clone(self),
            label: label.to_owned(),
        })
    }

    /// Count one use (a hold or a call), starting an idle-stopped engine on the way.
    fn admit(&self, call: bool) -> Result<(), JobError> {
        let presence = self.policy.presence.is_some();
        let mut g = self.gate.state();
        loop {
            if g.closed || self.closing.load(Ordering::SeqCst) {
                return Err(shutting_down());
            }
            if g.app_locked {
                return Err(JobError::supervisor("app_locked", "app is locked"));
            }
            if call && self.revoking.load(Ordering::SeqCst) > 0 {
                return Err(JobError::supervisor(
                    ERROR_REVOCATION_PENDING,
                    "an entitlement change is being applied; try again in a moment",
                ));
            }
            if !presence || g.phase == RunPhase::Running {
                if call {
                    g.busy += 1;
                } else {
                    g.holders += 1;
                }
                g.ever_used = true;
                g.zero_since = None;
                return Ok(());
            }
            match g.phase {
                RunPhase::Idle => {
                    g.phase = RunPhase::Starting;
                    drop(g);
                    let started = self.start_inner();
                    g = self.gate.state();
                    g.phase = if started.is_ok() {
                        RunPhase::Running
                    } else {
                        RunPhase::Idle
                    };
                    self.gate.cv.notify_all();
                    started?;
                }
                _ => {
                    g = self
                        .gate
                        .cv
                        .wait_timeout(g, Duration::from_millis(200))
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
            }
        }
    }

    fn release(&self, call: bool) {
        let mut g = self.gate.state();
        if call {
            g.busy = g.busy.saturating_sub(1);
        } else {
            g.holders = g.holders.saturating_sub(1);
        }
        if g.holders == 0 && g.busy == 0 {
            g.zero_since = Some(Instant::now());
        }
        drop(g);
        self.gate.cv.notify_all();
    }

    /// Start the thread that stops an unused engine (feature of [`PresencePolicy`]). It sleeps on a
    /// condition variable until the idle moment, never polls, and ends with the host. `false` when
    /// the policy has no presence or the monitor already runs. The shell calls it at startup.
    pub fn start_idle_monitor(self: &Arc<Self>) -> bool {
        let Some(presence) = self.policy.presence else {
            return false;
        };
        if self.monitor_started.swap(true, Ordering::SeqCst) {
            return false;
        }
        let gate = Arc::clone(&self.gate);
        let host = Arc::downgrade(self);
        std::thread::Builder::new()
            .name("rk-shell-engine-idle".into())
            .spawn(move || {
                while gate.wait_idle_due(presence.idle_exit) {
                    let Some(host) = host.upgrade() else { break };
                    host.stop_if_still_idle(presence.idle_exit);
                }
            })
            .is_ok()
    }

    fn stop_if_still_idle(&self, idle_exit: Duration) {
        {
            let mut g = self.gate.state();
            match g.due_at(idle_exit) {
                Some(at) if Instant::now() >= at => g.phase = RunPhase::Stopping,
                _ => return,
            }
        }
        let _ = self.stop_engine_process();
        let mut g = self.gate.state();
        if !g.closed {
            g.phase = RunPhase::Idle;
        }
        g.zero_since = None;
        drop(g);
        self.gate.cv.notify_all();
    }

    /// Ask the engine to stop, wait out the grace, kill the tree. Not a shutdown: the host can start
    /// it again.
    fn stop_engine_process(&self) -> Option<rightkit_job_queue::ShutdownReport> {
        let report = self.supervisor.as_ref().map(|s| s.shutdown_detailed());
        if let Some(c) = lock(&self.client).take() {
            c.shutdown(self.spec.shutdown_grace);
        }
        report
    }

    /// Stop the engine and start a fresh one (a new generation; snapshots replay before it is
    /// `Ready`). Calls in flight end with the old engine. Refused while the app is exiting.
    pub fn restart(&self) -> Result<Value, JobError> {
        if self.closing.load(Ordering::SeqCst) {
            return Err(shutting_down());
        }
        if self.policy.presence.is_some() {
            let mut g = self.gate.state();
            while matches!(g.phase, RunPhase::Starting | RunPhase::Stopping) {
                g = self
                    .gate
                    .cv
                    .wait_timeout(g, Duration::from_millis(200))
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
            g.phase = RunPhase::Stopping;
        }
        let _ = self.stop_engine_process();
        if self.policy.presence.is_some() {
            let mut g = self.gate.state();
            g.phase = RunPhase::Idle;
            drop(g);
            self.gate.cv.notify_all();
        }
        self.start()
    }

    // ---- snapshots ----

    /// Set the latest value of the declared slot `key`. Registered in declaration order (a
    /// `required` slot holds the later ones back until it has a value), sent now when the engine is
    /// `Ready`, and replayed to every later generation before it is `Ready`. An `Err` from a live
    /// engine means it did not take the value now; the value is still replayed.
    pub fn set_snapshot(&self, key: &str, params: Value) -> Result<(), JobError> {
        if self.closing.load(Ordering::SeqCst) {
            return Err(shutting_down());
        }
        let Some(target) = self.policy.snapshots.iter().position(|s| s.key == key) else {
            return Err(JobError::supervisor(
                ERROR_SNAPSHOT_NOT_DECLARED,
                format!("snapshot {key:?} is not declared by this app"),
            ));
        };
        let Some(supervisor) = &self.supervisor else {
            return Err(JobError::supervisor(
                "unsupported",
                "snapshots need a supervised engine",
            ));
        };
        let size = serde_json::to_vec(&params).map_or(usize::MAX, |v| v.len());
        if size > MAX_SNAPSHOT_BYTES {
            return Err(JobError::supervisor(
                "snapshot_too_large",
                format!("snapshot {key:?} is larger than {MAX_SNAPSHOT_BYTES} bytes"),
            ));
        }
        // One writer at a time: the order the engine hears them in is the order they were set.
        let mut board = lock(&self.snapshots);
        board.values.insert(key.to_owned(), params);
        let mut first_error = None;
        for (ix, slot) in self.policy.snapshots.iter().enumerate() {
            let Some(value) = board.values.get(&slot.key).cloned() else {
                continue;
            };
            let released = self.policy.snapshots[..ix]
                .iter()
                .filter(|s| s.required)
                .all(|s| board.values.contains_key(&s.key));
            if !released || (ix != target && board.registered.contains(&slot.key)) {
                continue;
            }
            // The supervisor records the value for replay before it sends it live, so a live
            // failure still leaves the slot registered.
            let sent = supervisor.set_snapshot(&slot.key, &slot.method, value);
            board.registered.insert(slot.key.clone());
            if let Err(e) = sent {
                first_error.get_or_insert(e);
            }
        }
        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// [`EngineHost::set_snapshot`] for a value that REVOKES something (an entitlement, a sign-in).
    /// New calls are refused (`revocation_pending`) while it applies. When the live engine does not
    /// acknowledge it, the engine is replaced, so the revoked state cannot stay live; the fresh
    /// engine boots on the revoked value. Calls already in flight when it began are not recalled.
    pub fn revoke_snapshot(&self, key: &str, params: Value) -> Result<(), JobError> {
        struct Revoking<'a>(&'a AtomicUsize);
        impl Drop for Revoking<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        self.revoking.fetch_add(1, Ordering::SeqCst);
        let _guard = Revoking(&self.revoking);
        match self.set_snapshot(key, params) {
            Ok(()) => Ok(()),
            // Not declared, too large, shutting down: nothing was applied and nothing is live that
            // should not be.
            Err(e)
                if matches!(
                    e.code.as_str(),
                    ERROR_SNAPSHOT_NOT_DECLARED
                        | "snapshot_too_large"
                        | "shutting_down"
                        | "unsupported"
                ) =>
            {
                Err(e)
            }
            Err(_) => self.restart().map(|_| ()),
        }
    }

    // ---- calls ----

    /// One call; engine events stream to the webview as `rightkit-shell://engine`
    /// `{call, event}` while it runs. A dead engine is restarted and the call retried
    /// only when the write itself failed (the request provably never reached the engine); a call lost mid-flight is reported, not replayed.
    pub fn call(
        &self,
        app: &AppHandle<Wry>,
        method: &str,
        params: Value,
    ) -> Result<Value, JobError> {
        let app = app.clone();
        self.call_tagged(
            method,
            params,
            None,
            Arc::new(move |event| {
                let _ = app.emit(EVENT_ENGINE, event);
            }),
        )
    }

    /// Native/IPC entry point with caller correlation & request ids. In-flight failures are never replayed.
    pub fn call_tagged(
        &self,
        method: &str,
        params: Value,
        tag: Option<String>,
        sink: EngineEventSink,
    ) -> Result<Value, JobError> {
        self.policy.check_method(method)?;
        if self.closing.load(Ordering::SeqCst) {
            return Err(shutting_down());
        }
        self.admit(true)?;
        let _busy = Busy(self);
        // Bind the call to the engine generation it is sent under. A supervised engine that is not
        // `Ready` now (stopped for idleness, recovering, on-demand restart pending) is started or
        // waited for inside the call, so no generation is known yet: that call is not bound, and
        // only a call bound to a live engine can be reported stale.
        let mut ticket: Option<Ticket> = match &self.supervisor {
            Some(s) if s.lifecycle() != Lifecycle::Ready => None,
            _ => {
                let epoch = self.latch.observe(self.generation());
                Some(self.latch.issue(epoch).map_err(|s| {
                    // The engine changed between looking and asking: the request was not sent.
                    JobError::supervisor_retryable(
                        ERROR_STALE_GENERATION,
                        format!(
                            "the engine changed generation ({} -> {})",
                            s.issued, s.current
                        ),
                    )
                })?)
            }
        };
        let timeout = self.policy.timeout_for(method, self.spec.call_timeout);
        let fence = self.latch.fence();
        let dropped = Arc::clone(&self.stale_frames);
        let on_frame = |ticket: Option<Ticket>| {
            let (call, tag, sink) = (method.to_string(), tag.clone(), sink.clone());
            let (fence, dropped) = (Arc::clone(&fence), Arc::clone(&dropped));
            Box::new(move |frame: &EventFrame<'_>| {
                // Events of a call whose engine was replaced are not forwarded.
                if ticket.is_some_and(|t| fence.admit_reply(&t).is_err()) {
                    dropped.fetch_add(1, Ordering::Relaxed);
                    return;
                }
                sink(EngineEvent {
                    call: call.clone(),
                    id: Some(frame.id),
                    tag: tag.clone(),
                    event: frame.event.clone(),
                })
            }) as rightkit_job_queue::OnEventFrame
        };
        let result = if let Some(supervisor) = &self.supervisor {
            supervisor.call_with(
                method,
                params,
                CallOptions {
                    timeout,
                    on_frame: Some(on_frame(ticket)),
                    keep_alive_on_timeout: self.policy.keep_alive_on_timeout,
                    ..Default::default()
                },
            )
        } else {
            let attempt = |client: Arc<EngineClient>, ticket: Option<Ticket>| match timeout {
                Some(t) => {
                    client.call_timeout_frames(method, params.clone(), Some(on_frame(ticket)), t)
                }
                None => client.call_with_frames(method, params.clone(), Some(on_frame(ticket))),
            };
            let closing = || self.closing.load(Ordering::SeqCst);
            match self.client().and_then(|c| attempt(c, ticket)) {
                // The request never left this process (write to a dead pipe): safe to retry once on a
                // fresh engine, under the new engine's generation.
                Err(e) if matches!(e.code.as_str(), "io" | "engine_write") && !closing() => {
                    self.start_inner().and_then(|_| {
                        let epoch = self.latch.observe(self.generation());
                        ticket = Some(self.latch.issue(epoch).map_err(stale_error)?);
                        attempt(self.client()?, ticket)
                    })
                }
                // The engine died while the call was in flight: it may have partly run, so the call is
                // NOT replayed. Restart so the next call works, and report the failure.
                Err(e) if e.code == "engine_exited" && !closing() => {
                    let _ = self.start_inner();
                    Err(e)
                }
                other => other,
            }
        };
        // Deliver the reply only if the engine it was sent to is still the current one.
        self.latch.observe(self.generation());
        match ticket {
            Some(ticket) => self.latch.admit(&ticket, result),
            None => result,
        }
    }

    /// Subscribe before `start()` to observe recovery even when no call is active.
    pub fn subscribe_lifecycle(&self) -> Option<std::sync::mpsc::Receiver<LifecycleEvent>> {
        self.supervisor.as_ref().map(|s| s.subscribe_lifecycle())
    }

    pub fn is_shutting_down(&self) -> bool {
        self.closing.load(Ordering::SeqCst)
    }

    /// Unsolicited engine frames, with no request id. Supervised mode only.
    pub fn subscribe_events(&self) -> Option<std::sync::mpsc::Receiver<Value>> {
        self.supervisor.as_ref().map(|s| s.subscribe_events())
    }

    /// Ordered-exit step: refuse new calls, then ask the engine to stop (bounded).
    pub fn shutdown(&self) {
        let _ = self.shutdown_ordered();
    }

    /// The ordered shutdown, reporting what happened: refuse new calls and holds, let calls in
    /// flight finish (at most [`EnginePolicy::shutdown_drain`]), then stop the engine (ask, grace,
    /// kill). Idempotent; a second call finds it done and reports `first: false`.
    pub fn shutdown_ordered(&self) -> EngineShutdownReport {
        let first = !self.closing.swap(true, Ordering::SeqCst);
        // 1. No new calls, no new holds, the idle monitor ends.
        {
            let mut g = self.gate.state();
            g.closed = true;
        }
        self.gate.cv.notify_all();
        // 2. Calls in flight get to finish.
        let drain = self.policy.shutdown_drain.unwrap_or(DEFAULT_SHUTDOWN_DRAIN);
        let deadline = Instant::now() + drain;
        let (abandoned, holders) = {
            let mut g = self.gate.state();
            while g.busy > 0 {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                g = self
                    .gate
                    .cv
                    .wait_timeout(g, left)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
            (g.busy, g.holders)
        };
        // 3. The engine.
        let engine = if first {
            self.stop_engine_process()
        } else {
            None
        };
        EngineShutdownReport {
            first,
            drained: abandoned == 0,
            abandoned_calls: abandoned,
            holders_left: holders,
            engine,
        }
    }
}

impl Drop for EngineHost {
    /// A host dropped without a shutdown still ends its idle monitor thread.
    fn drop(&mut self) {
        lock(&self.gate.state).closed = true;
        self.gate.cv.notify_all();
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;

    #[test]
    fn deny_all_default_and_exact_allowlist_with_empty_deny_all() {
        assert_eq!(
            EnginePolicy::default()
                .check_method("anything")
                .unwrap_err()
                .code,
            "method_not_allowed"
        );
        let policy = EnginePolicy::default().allow_methods(["project.show"]);
        assert!(policy.check_method("project.show").is_ok());
        for method in ["project", "project.show.extra", "PROJECT.SHOW"] {
            assert_eq!(
                policy.check_method(method).unwrap_err().code,
                "method_not_allowed"
            );
        }
        assert!(EnginePolicy::default()
            .allow_methods(Vec::<String>::new())
            .check_method("project.show")
            .is_err());
    }

    #[test]
    fn method_deadlines_override_default_only_for_exact_names() {
        let policy = EnginePolicy::default().method_timeout("render", Duration::from_secs(300));
        assert_eq!(
            policy.timeout_for("render", Some(Duration::from_secs(10))),
            Some(Duration::from_secs(300))
        );
        assert_eq!(
            policy.timeout_for("render.cancel", Some(Duration::from_secs(10))),
            Some(Duration::from_secs(10))
        );
        assert_eq!(policy.timeout_for("other", None), None);
    }

    #[test]
    fn denial_precedes_client_acquisition_and_spawn() {
        let host = EngineHost::with_policy(
            EngineSpec::new(|| panic!("denied method spawned engine")),
            EnginePolicy::default().allow_methods(["ok"]),
        );
        assert_eq!(
            host.call_tagged("bad", Value::Null, None, Arc::new(|_| panic!("event")))
                .unwrap_err()
                .code,
            "method_not_allowed"
        );
    }

    #[test]
    fn correlated_event_and_error_wire_shapes() {
        let event = EngineEvent {
            call: "render".into(),
            id: Some(42),
            tag: Some("ui-2".into()),
            event: serde_json::json!({"progress": 1}),
        };
        assert_eq!(
            serde_json::to_value(event).unwrap(),
            serde_json::json!({"call": "render", "id": 42, "tag": "ui-2", "event": {"progress": 1}})
        );
        let reply = EngineCallError::from(JobError::new("not_ready", "engine reply"));
        let readiness = EngineCallError::from(JobError::supervisor("not_ready", "starting"));
        assert_eq!(reply.origin, "engine");
        assert_eq!(readiness.origin, "supervisor");
        let error = EngineCallError::from(JobError::supervisor("method_not_allowed", "denied"));
        assert_eq!(
            serde_json::to_value(error).unwrap(),
            serde_json::json!({"code": "method_not_allowed", "message": "denied", "origin": "supervisor"})
        );
        // M4 details pass through untouched; absent details stay off the wire (above).
        let detailed = EngineCallError::from(
            JobError::new("needs_auth", "sign in")
                .with_details(serde_json::json!({"provider": "x", "nested": {"n": 1}})),
        );
        assert_eq!(
            serde_json::to_value(detailed).unwrap(),
            serde_json::json!({"code": "needs_auth", "message": "sign in", "origin": "engine", "details": {"provider": "x", "nested": {"n": 1}}})
        );
    }

    // ---- composition (PK06-3) ----

    fn state(holders: usize, busy: usize, ever: bool, phase: RunPhase) -> GateState {
        GateState {
            app_locked: false,
            holders,
            busy,
            ever_used: ever,
            zero_since: Some(Instant::now()),
            phase,
            closed: false,
        }
    }

    #[test]
    fn an_engine_is_due_for_idle_exit_only_after_use_with_nothing_holding_it() {
        let idle = Duration::from_secs(30);
        let due = |s: &GateState| s.due_at(idle);
        // Used, free, running: due one `idle_exit` after it went free.
        let s = state(0, 0, true, RunPhase::Running);
        assert_eq!(due(&s), s.zero_since.map(|t| t + idle));
        // Never used: never idles out.
        assert_eq!(due(&state(0, 0, false, RunPhase::Running)), None);
        // A holder or a call in flight keeps it.
        assert_eq!(due(&state(1, 0, true, RunPhase::Running)), None);
        assert_eq!(due(&state(0, 1, true, RunPhase::Running)), None);
        // Not running (starting, stopping, already idle): nothing to stop.
        for phase in [RunPhase::Starting, RunPhase::Stopping, RunPhase::Idle] {
            assert_eq!(due(&state(0, 0, true, phase)), None);
        }
        // A closed gate is past idling.
        let mut closed = state(0, 0, true, RunPhase::Running);
        closed.closed = true;
        assert_eq!(due(&closed), None);
    }

    #[test]
    fn the_idle_wait_ends_when_due_or_closed_and_never_before() {
        let gate = Arc::new(Gate::new(true));
        {
            let mut g = gate.state();
            g.phase = RunPhase::Running;
            g.ever_used = true;
            g.zero_since = Some(Instant::now());
        }
        let started = Instant::now();
        assert!(gate.wait_idle_due(Duration::from_millis(60)));
        assert!(started.elapsed() >= Duration::from_millis(55));
        // Held: the wait does not end by itself; closing ends it with `false`.
        gate.state().holders = 1;
        let closer = {
            let gate = Arc::clone(&gate);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(80));
                gate.state().closed = true;
                gate.cv.notify_all();
            })
        };
        assert!(!gate.wait_idle_due(Duration::from_millis(10)));
        closer.join().unwrap();
    }

    #[test]
    fn a_reply_from_a_replaced_engine_is_not_delivered_but_errors_pass() {
        let latch = GenerationLatch::default();
        let epoch = latch.observe(1);
        let ticket = latch.issue(epoch).unwrap();
        // Same generation: delivered.
        assert_eq!(
            latch.admit(&ticket, Ok(serde_json::json!(1))).unwrap(),
            serde_json::json!(1)
        );
        // The engine is replaced: the late reply is refused, as an error that is not retryable.
        latch.observe(2);
        let stale = latch
            .admit(&ticket, Ok(serde_json::json!("old engine answer")))
            .unwrap_err();
        assert_eq!(stale.code, ERROR_STALE_GENERATION);
        assert!(!stale.message.contains("old engine answer"));
        // An error reply is the call's own error and passes through unchanged.
        assert_eq!(
            latch
                .admit(&ticket, Err(JobError::new("boom", "x")))
                .unwrap_err()
                .code,
            "boom"
        );
        // A new request under the new generation is fine, and an old epoch cannot issue.
        let fresh = latch.issue(latch.observe(2)).unwrap();
        assert!(latch.admit(&fresh, Ok(Value::Null)).is_ok());
        assert!(latch.issue(epoch).is_err());
    }

    #[test]
    fn a_generation_that_does_not_change_never_advances_the_latch() {
        let latch = GenerationLatch::default();
        let a = latch.observe(3);
        let b = latch.observe(3);
        assert_eq!(a, b);
        let ticket = latch.issue(b).unwrap();
        latch.observe(3);
        assert!(latch.admit(&ticket, Ok(Value::Null)).is_ok());
    }

    #[test]
    #[should_panic(expected = "declared twice")]
    fn a_snapshot_slot_is_declared_once() {
        let _ = EnginePolicy::default()
            .snapshot(SnapshotSlot::new("init", "set_init"))
            .snapshot(SnapshotSlot::new("init", "set_other"));
    }

    #[test]
    fn declaring_snapshots_makes_the_host_supervised_and_undeclared_keys_are_refused() {
        let host = EngineHost::with_policy(
            EngineSpec::new(|| panic!("a refused snapshot must not spawn the engine")),
            EnginePolicy::default().snapshot(SnapshotSlot::new("init", "set_init")),
        );
        assert!(
            host.subscribe_lifecycle().is_some(),
            "snapshots imply supervision"
        );
        assert_eq!(
            host.set_snapshot("other", Value::Null).unwrap_err().code,
            ERROR_SNAPSHOT_NOT_DECLARED
        );
        let big = Value::String("x".repeat(MAX_SNAPSHOT_BYTES + 1));
        assert_eq!(
            host.set_snapshot("init", big).unwrap_err().code,
            "snapshot_too_large"
        );
        // A host without snapshots refuses them as unsupported, not as a panic.
        let plain = EngineHost::new(EngineSpec::new(|| panic!("no spawn")));
        assert_eq!(
            plain.set_snapshot("init", Value::Null).unwrap_err().code,
            ERROR_SNAPSHOT_NOT_DECLARED
        );
    }

    #[test]
    fn shutdown_refuses_new_use_and_a_second_shutdown_finds_it_done() {
        let host = Arc::new(EngineHost::with_policy(
            EngineSpec::new(|| panic!("never started")),
            EnginePolicy::default()
                .allow_methods(["echo"])
                .presence(PresencePolicy::idle_exit(Duration::from_secs(60))),
        ));
        let first = host.shutdown_ordered();
        assert!(first.first && first.drained && first.abandoned_calls == 0);
        assert_eq!(host.hold("w").unwrap_err().code, "shutting_down");
        assert_eq!(
            host.call_tagged("echo", Value::Null, None, Arc::new(|_| {}))
                .unwrap_err()
                .code,
            "shutting_down"
        );
        assert_eq!(host.start().unwrap_err().code, "shutting_down");
        let second = host.shutdown_ordered();
        assert!(!second.first);
        assert!(!host.start_idle_monitor() || host.is_shutting_down());
    }
}
