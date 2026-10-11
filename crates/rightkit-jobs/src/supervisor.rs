//! Engine sidecar supervision: spawn, hello, handshake replay, restart budget,
//! health pings, wedge recovery and lifecycle diagnostics, on top of
//! [`EngineClient`] (frames) and `rightkit-process` (child ownership, restart
//! and health state machines).
//!
//! This is the shared form of five live copies: ScrapeRight's
//! `engine_supervisor` (bounded restart policy, snapshot replay into every
//! replacement child, RPC timeout kills a wedged child, lifecycle diagnostics,
//! `frame_too_large` instead of a child kill), HeardRight's
//! `heardright-app-runtime::engine_supervisor` (env scrub, generation
//! abandonment, bounded reap, on-demand restart after a crash, abandon circuit
//! breaker), CutRight's and GenRight's supervisors, and the donor template.
//!
//! Model:
//! - One engine process per *generation*. Every boot attempt (including a failed
//!   one) is a generation; a stale generation can never mark a newer one down.
//! - `start()` boots generation 1 synchronously. A first boot that fails returns
//!   its error and leaves the supervisor `Down` (a binary that cannot even say
//!   hello is a configuration problem, not a crash to retry); call `start()`
//!   again to retry with a fresh budget.
//! - Once an engine has been `Ready`, any exit (crash, kill, EOF) goes through
//!   the [`RestartPolicy`]: `Automatic` restarts after exponential backoff until
//!   the rolling window's budget is spent (then terminal `Down`), `OnDemand`
//!   waits for the next [`EngineSupervisor::call`], `Disabled` stays down.
//! - Boot failures during a restart (spawn error, no hello, failed handshake)
//!   spend the same budget, so a persistently broken engine ends `Down` instead
//!   of looping.
//! - The handshake is "replay every snapshot, in order, as a call": the app
//!   registers `Init`/`SetSettings`/entitlement style state with
//!   [`EngineSupervisor::set_snapshot`] and each replacement child receives the
//!   latest of each before the supervisor reports `Ready`.
//! - A call that exceeds its timeout treats the engine as wedged: the tree is
//!   killed and the normal restart path runs. An optional health check pings
//!   the engine and does the same after consecutive failures.

use std::fmt;
use std::io::{self, BufReader};
use std::process::Stdio;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use rightkit_process::{
    Generation, HealthOutcome, HealthPolicy, HealthProbe, HealthTracker, OwnedChild, OwnedCommand,
    RestartMode, RestartOutcome, RestartPermit, RestartPolicy, RestartTracker, StderrTail,
};
use serde::Serialize;
use serde_json::{json, Value};

use crate::client::{
    merge_event_callbacks, ChildExit, ClientOptions, ContractRequirement, EngineClient,
    OnEventFrame, ProtocolErrorSink, DEFAULT_HELLO_TIMEOUT,
};
use crate::contract::DEFAULT_FRAME_LIMIT;
use crate::error::{JobError, JobResult, CONTRACT_VERSION_MISMATCH};

type SpawnFn = dyn Fn() -> io::Result<OwnedCommand> + Send + Sync;
type Redactor = dyn Fn(&str) -> Option<String> + Send + Sync;
type LineSink = dyn Fn(&str) + Send + Sync;
type OnEvent = Box<dyn Fn(&Value) + Send>;

/// Shell-visible engine lifecycle: what banners and tests key off.
/// `Recovering` is the window between an unexpected exit and the replacement
/// reaching `Ready`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Booting,
    Ready,
    Recovering,
    Down,
}

/// How [`EngineSupervisor::shutdown_detailed`] ended. Ordered by severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownOutcome {
    /// No engine process was running or booting.
    NoEngine,
    /// Another caller was already shutting down; this one returned after the reap.
    AlreadyShuttingDown,
    /// The engine left within the grace period (graceful exit, or it had already exited)
    /// and was waited.
    Exited,
    /// The engine outlived the grace period; its process tree was killed and waited.
    Killed,
    /// The bound passed with a process still unwaited. The supervisor is `Down` with
    /// terminal `Failed`, not `Stopped`.
    Unconfirmed,
}

/// What [`EngineSupervisor::shutdown_detailed`] saw. When it returns, `reaped` is true
/// unless `outcome` is [`ShutdownOutcome::Unconfirmed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownReport {
    pub outcome: ShutdownOutcome,
    /// Process id of the engine that was stopped, when one was.
    pub pid: Option<u32>,
    /// Its exit status, once waited and observed.
    pub exit_status: Option<std::process::ExitStatus>,
    /// True when no engine process of this supervisor remained unwaited at return.
    pub reaped: bool,
    /// Unwaited engine processes at return (zombies included).
    pub unreaped: usize,
    pub elapsed: Duration,
}

impl ShutdownReport {
    /// Exited and waited: no live process, no zombie. (`kill -0 <pid>` cannot tell this;
    /// a zombie answers it.)
    pub fn is_reaped(&self) -> bool {
        self.reaped && self.unreaped == 0
    }
    /// Reaped, and the engine left by itself within the grace period.
    pub fn is_clean(&self) -> bool {
        self.is_reaped()
            && matches!(
                self.outcome,
                ShutdownOutcome::Exited | ShutdownOutcome::NoEngine
            )
    }
    /// The engine's exit status, when observed.
    pub fn exit_status(&self) -> Option<std::process::ExitStatus> {
        self.exit_status
    }
}

/// Why a `Down` supervisor will not restart by itself. Read it through
/// [`SupervisorStatus::terminal`]; `lifecycle()` stays `Down` for both variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminal {
    /// Boot or restart failed and the policy will not try again: a first
    /// `start()` that failed, a spent restart budget, or a crash with restarts
    /// disabled. `reason` is the lifecycle message that ended the run.
    Failed { reason: String },
    /// Stopped on purpose by [`EngineSupervisor::shutdown`] (or by dropping it).
    Stopped,
}

/// What a failed first boot (spawn, hello, handshake) does. `Retry` (default)
/// treats it like any crash: restart budget is spent and the engine is booted
/// again, so `start()` returns once the engine is `Ready` or the boot is
/// `Down`. `Terminal` gives up at once. A contract mismatch is always terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InitialFailure {
    #[default]
    Retry,
    Terminal,
}

/// Why a generation ended; feeds `last_exit_reason`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitReason {
    /// The engine closed its stdout (clean exit or crash) with no exit code observed.
    Eof,
    /// The process exited and its status was observed.
    Exited,
    /// A call timed out; the engine was treated as wedged and killed.
    RpcTimeoutKill,
    /// Consecutive failed health pings; the engine was killed.
    HealthKill,
    /// The process could not be started.
    SpawnFailed(String),
    /// No valid hello (`hello_timeout`, `engine_exited`, `engine_protocol`).
    HelloFailed(String),
    /// A snapshot replay call failed.
    HandshakeFailed(String),
}

impl fmt::Display for ExitReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExitReason::Eof => f.write_str("eof"),
            ExitReason::Exited => f.write_str("exited"),
            ExitReason::RpcTimeoutKill => f.write_str("rpc_timeout_kill"),
            ExitReason::HealthKill => f.write_str("health_kill"),
            ExitReason::SpawnFailed(e) => write!(f, "spawn_failed: {e}"),
            ExitReason::HelloFailed(e) => write!(f, "hello_failed: {e}"),
            ExitReason::HandshakeFailed(e) => write!(f, "handshake_failed: {e}"),
        }
    }
}

/// Broadcast on every lifecycle transition. Carries enough to debug a terminal
/// `Down` without a debugger: generation, attempt, restarts in the rolling
/// window, the last exit code and reason, the redacted stderr tail.
#[derive(Debug, Clone, Serialize)]
pub struct LifecycleEvent {
    pub state: Lifecycle,
    pub generation: u64,
    pub attempt: u64,
    pub restart_count_window: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_exit_reason: Option<String>,
    /// Exit-to-`Ready` time of a recovery.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_tail: Option<String>,
    /// True when `Down` will restart on the next call (`OnDemand`).
    pub restart_on_demand: bool,
    pub message: String,
}

/// Point-in-time view for diagnostics panels.
#[derive(Debug, Clone)]
pub struct SupervisorStatus {
    pub lifecycle: Lifecycle,
    pub generation: u64,
    pub attempt: u64,
    pub pid: Option<u32>,
    pub hello: Option<Value>,
    pub restarts_in_window: u32,
    /// Restarts scheduled since the supervisor was created: each time the restart
    /// policy allowed another boot, including retries of a failed first boot.
    /// Not reset by `start()`.
    pub restarts: u32,
    /// Set while the supervisor is `Down` and will not restart by itself.
    /// `None` while running, while recoverable (`OnDemand`), or before any start.
    pub terminal: Option<Terminal>,
    pub last_exit_code: Option<i32>,
    pub last_exit_reason: Option<String>,
    /// Lifecycle subscribers registered (a sender is dropped once its receiver is gone and an event
    /// is next sent). Nothing bounds this count.
    pub lifecycle_subscribers: usize,
    /// Unsolicited-event subscribers registered; same note.
    pub event_subscribers: usize,
    /// Resident memory, CPU time and I/O counters of this process (the supervisor and its client
    /// threads run in it), computed on call, or `None` when the host cannot report them. The engine
    /// child is a separate process; read it with `rightkit_process::process_usage` inside the engine.
    /// See `rightkit_process::ProcessUsage`.
    pub process: Option<rightkit_process::ProcessUsage>,
}

/// Periodic liveness ping.
#[derive(Debug, Clone)]
pub struct HealthCheck {
    /// Engine method to call (`ping` is a built-in of the stdio contract).
    pub method: String,
    pub interval: Duration,
    /// A ping slower than this counts as a failure.
    pub timeout: Duration,
    /// Consecutive failures before the engine is killed and restarted.
    pub unhealthy_threshold: u32,
}

impl Default for HealthCheck {
    fn default() -> Self {
        Self {
            method: "ping".into(),
            interval: Duration::from_secs(2),
            timeout: Duration::from_secs(1),
            unhealthy_threshold: 3,
        }
    }
}

/// How to run and recover one engine.
pub struct SupervisorConfig {
    /// Names threads and diagnostics.
    pub name: String,
    spawn: Arc<SpawnFn>,
    /// Wait for the engine's hello frame this long.
    pub hello_timeout: Duration,
    /// How long [`EngineSupervisor::call`] waits for a booting/recovering
    /// engine before answering `not_ready`.
    pub ready_wait: Duration,
    /// Default bound on one call; expiry kills the engine as wedged.
    pub rpc_timeout: Duration,
    /// Largest frame accepted from the engine.
    pub frame_limit: usize,
    pub restart: RestartPolicy,
    pub health: Option<HealthCheck>,
    /// Grace for the engine's own `shutdown` request, and for reaping.
    pub shutdown_grace: Duration,
    /// Most recent stderr bytes kept for diagnostics.
    pub stderr_tail_bytes: usize,
    redact: Option<Arc<Redactor>>,
    on_stderr: Option<Arc<LineSink>>,
    contract_version: Option<ContractRequirement>,
    initial_failure: InitialFailure,
    protocol_error_sink: Option<ProtocolErrorSink>,
    fallback_error_code: String,
}

impl SupervisorConfig {
    /// `spawn` builds a fresh command per generation; apply env scrub
    /// (`OwnedCommand::env_allowlist`/`env_strip`) and `windows_hide` there.
    /// stdin/stdout/stderr are piped by the supervisor.
    pub fn new(
        name: impl Into<String>,
        spawn: impl Fn() -> io::Result<OwnedCommand> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            spawn: Arc::new(spawn),
            hello_timeout: DEFAULT_HELLO_TIMEOUT,
            ready_wait: Duration::from_secs(5),
            rpc_timeout: Duration::from_secs(120),
            frame_limit: DEFAULT_FRAME_LIMIT,
            restart: RestartPolicy::default(),
            health: None,
            shutdown_grace: Duration::from_secs(2),
            stderr_tail_bytes: 16 * 1024,
            redact: None,
            on_stderr: None,
            contract_version: None,
            initial_failure: InitialFailure::Retry,
            protocol_error_sink: None,
            fallback_error_code: "engine_error".into(),
        }
    }
    /// What a failed first boot does; default [`InitialFailure::Retry`].
    pub fn with_initial_failure(mut self, policy: InitialFailure) -> Self {
        self.initial_failure = policy;
        self
    }
    /// Receives each malformed stdout line of every generation (on the reader thread).
    pub fn with_protocol_error_sink(
        mut self,
        f: impl Fn(&crate::client::ProtocolError) + Send + Sync + 'static,
    ) -> Self {
        self.protocol_error_sink = Some(ProtocolErrorSink::new(f));
        self
    }
    /// Code for an engine error reply that carries none. Default `engine_error`.
    pub fn with_fallback_error_code(mut self, code: impl Into<String>) -> Self {
        self.fallback_error_code = code.into();
        self
    }
    /// Refuse an engine whose hello frame does not advertise `version`
    /// (`contract_version`). The boot fails with `contract_version_mismatch`
    /// (not retryable); during a restart that failure spends restart budget.
    pub fn require_contract_version(mut self, version: impl Into<ContractRequirement>) -> Self {
        self.contract_version = Some(version.into());
        self
    }
    pub fn with_restart(mut self, restart: RestartPolicy) -> Self {
        self.restart = restart;
        self
    }
    pub fn with_health(mut self, health: HealthCheck) -> Self {
        self.health = Some(health);
        self
    }
    pub fn with_hello_timeout(mut self, d: Duration) -> Self {
        self.hello_timeout = d;
        self
    }
    pub fn with_rpc_timeout(mut self, d: Duration) -> Self {
        self.rpc_timeout = d;
        self
    }
    pub fn with_ready_wait(mut self, d: Duration) -> Self {
        self.ready_wait = d;
        self
    }
    pub fn with_frame_limit(mut self, n: usize) -> Self {
        self.frame_limit = n;
        self
    }
    pub fn with_shutdown_grace(mut self, d: Duration) -> Self {
        self.shutdown_grace = d;
        self
    }
    /// Redact (or drop, by returning `None`) each stderr line before it is
    /// retained or forwarded. HeardRight's `redact_sidecar_stderr_line`.
    pub fn with_stderr_redaction(
        mut self,
        f: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        self.redact = Some(Arc::new(f));
        self
    }
    /// Receive every (redacted) stderr line as it arrives, for logging.
    pub fn with_stderr_sink(mut self, f: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.on_stderr = Some(Arc::new(f));
        self
    }
}

struct LastExit {
    code: Option<i32>,
    reason: String,
    stderr: String,
}

struct State {
    lifecycle: Lifecycle,
    generation: u64,
    token: Option<Generation>,
    client: Option<Arc<EngineClient>>,
    /// A generation whose child is attached but not yet `Ready`; lets `shutdown` reach it.
    booting: Option<Arc<EngineClient>>,
    tracker: RestartTracker,
    attempt: u64,
    shutting_down: bool,
    replay: Vec<(String, String, Value)>,
    hello: Option<Value>,
    last_exit: Option<LastExit>,
    recovery_from: Option<Instant>,
    on_demand_pending: bool,
    restarts: u32,
    terminal: Option<Terminal>,
    /// Why the most recent first boot failed; what `start()` returns when it ends `Down`.
    last_start_error: Option<JobError>,
}

struct Shared {
    cfg: SupervisorConfig,
    epoch: Instant,
    st: Mutex<State>,
    cv: Condvar,
    lifecycle_subs: Mutex<Vec<Sender<LifecycleEvent>>>,
    event_subs: Mutex<Vec<Sender<Value>>>,
    wedge: Mutex<Option<(u64, ExitReason)>>,
    /// Engine processes spawned and not yet waited, on any path.
    live: Mutex<usize>,
    live_cv: Condvar,
    /// The child of a boot between spawn and hello, so `shutdown` can kill it.
    boot_slot: Mutex<Option<(OwnedChild, LiveChild)>>,
}

/// One engine process that has been spawned and not yet waited; dropped when it is reaped.
struct LiveChild(Arc<Shared>);

impl LiveChild {
    fn new(sh: &Arc<Shared>) -> Self {
        *sh.live.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        Self(Arc::clone(sh))
    }
}

impl Drop for LiveChild {
    fn drop(&mut self) {
        let mut n = self.0.live.lock().unwrap_or_else(|e| e.into_inner());
        *n = n.saturating_sub(1);
        drop(n);
        self.0.live_cv.notify_all();
    }
}

impl Shared {
    /// Wait until no spawned engine process is left unwaited, or `deadline`; true when none.
    fn wait_live_zero(&self, deadline: Instant) -> bool {
        let mut n = self.live.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if *n == 0 {
                return true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            n = self
                .live_cv
                .wait_timeout(n, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
    fn lock(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn now(&self) -> Duration {
        self.epoch.elapsed()
    }
    fn emit(&self, st: &State, message: impl Into<String>, recovery_ms: Option<u64>) {
        let ev = LifecycleEvent {
            state: st.lifecycle,
            generation: st.generation,
            attempt: st.attempt,
            restart_count_window: st.tracker.restarts_in_window(self.now()),
            last_exit_code: st.last_exit.as_ref().and_then(|l| l.code),
            last_exit_reason: st.last_exit.as_ref().map(|l| l.reason.clone()),
            recovery_ms,
            stderr_tail: st
                .last_exit
                .as_ref()
                .map(|l| l.stderr.clone())
                .filter(|s| !s.is_empty()),
            restart_on_demand: st.on_demand_pending,
            message: message.into(),
        };
        self.lifecycle_subs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|tx| tx.send(ev.clone()).is_ok());
    }
}

/// Owns one engine process and keeps it alive according to its policy. Cheap
/// to share behind an `Arc`; dropping it shuts the engine down.
pub struct EngineSupervisor {
    sh: Arc<Shared>,
}

/// Per-call options for [`EngineSupervisor::call_with`].
#[derive(Default)]
pub struct CallOptions {
    /// Overrides [`SupervisorConfig::rpc_timeout`].
    pub timeout: Option<Duration>,
    /// Progress events of this call (run on the calling thread).
    pub on_event: Option<OnEvent>,
    /// Like `on_event`, but each event also carries the request id it belongs
    /// to. Both callbacks may be set; each receives every event.
    pub on_frame: Option<OnEventFrame>,
    /// Keep the engine alive when this call times out. By default a timeout
    /// kills the engine as wedged (ScrapeRight's rule); set this for a call
    /// that is legitimately slow and only the *wait* should be abandoned.
    pub keep_alive_on_timeout: bool,
}

impl EngineSupervisor {
    pub fn new(cfg: SupervisorConfig) -> Self {
        let tracker = RestartTracker::new(cfg.restart);
        let sh = Shared {
            cfg,
            epoch: Instant::now(),
            st: Mutex::new(State {
                lifecycle: Lifecycle::Down,
                generation: 0,
                token: None,
                client: None,
                booting: None,
                tracker,
                attempt: 0,
                shutting_down: false,
                replay: Vec::new(),
                hello: None,
                last_exit: None,
                recovery_from: None,
                on_demand_pending: false,
                restarts: 0,
                terminal: None,
                last_start_error: None,
            }),
            cv: Condvar::new(),
            lifecycle_subs: Mutex::default(),
            event_subs: Mutex::default(),
            wedge: Mutex::new(None),
            live: Mutex::new(0),
            live_cv: Condvar::new(),
            boot_slot: Mutex::new(None),
        };
        Self { sh: Arc::new(sh) }
    }

    /// Boot the engine and return once it is `Ready` (hello received and every
    /// snapshot replayed). No-op when already `Ready`; waits when a boot is in
    /// flight; from `Down` it starts a fresh run with a fresh restart budget.
    /// A failed first boot is retried within the budget unless the config says
    /// [`InitialFailure::Terminal`]; this returns once the boot is `Ready` or `Down`.
    pub fn start(&self) -> JobResult<()> {
        run_start(&self.sh, begin_start(&self.sh))
    }

    /// As [`Self::start`], but returns at once. The lifecycle is already
    /// `Booting` when this returns; the boot runs on a background thread and the
    /// [`Readiness`] handle reports how it ended. When the engine is already
    /// `Ready` there is nothing to boot and the handle is resolved before this returns.
    pub fn start_background(&self) -> Readiness {
        let plan = begin_start(&self.sh);
        let readiness = Readiness::default();
        if plan == StartPlan::Ready {
            // No work to hand to a thread; a spawned no-op would leave `try_ready()` racing it.
            readiness.finish(Ok(()));
            return readiness;
        }
        let (sh, slot) = (Arc::clone(&self.sh), readiness.clone());
        let spawned = thread::Builder::new()
            .name(format!("{}-start", self.sh.cfg.name))
            .spawn(move || slot.finish(run_start(&sh, plan)));
        if spawned.is_err() {
            // No thread to run the boot on: run it here so the state never sticks at `Booting`.
            readiness.finish(run_start(&self.sh, plan));
        }
        readiness
    }

    /// Call an engine method with the default timeout. Waits (bounded by
    /// `ready_wait`) while the engine boots or recovers, restarts an `OnDemand`
    /// engine that crashed, and answers `down` when it cannot.
    pub fn call(&self, method: &str, params: Value) -> JobResult<Value> {
        self.call_with(method, params, CallOptions::default())
    }

    pub fn call_with(&self, method: &str, params: Value, options: CallOptions) -> JobResult<Value> {
        let (client, generation) = self.ready_client()?;
        let timeout = options.timeout.unwrap_or(self.sh.cfg.rpc_timeout);
        let on_frame = merge_event_callbacks(options.on_event, options.on_frame);
        match client.call_timeout_frames(method, params, on_frame, timeout) {
            Err(e) if e.code == "timeout" => {
                if !options.keep_alive_on_timeout {
                    kill_wedged(&self.sh, generation, &client, ExitReason::RpcTimeoutKill);
                }
                Err(JobError::supervisor_retryable(
                    "timeout",
                    format!(
                        "engine did not answer {method} within {timeout:?}{}",
                        if options.keep_alive_on_timeout {
                            ""
                        } else {
                            "; it will be restarted"
                        }
                    ),
                ))
            }
            // The engine went away under this call; a restart (if the policy allows) makes a retry meaningful.
            Err(e) if e.code == "engine_exited" || e.code == "engine_write" => {
                Err(JobError::supervisor_retryable(e.code, e.message))
            }
            other => other,
        }
    }

    /// Record the latest state of `key` and replay it, in registration order, to
    /// every future engine generation before it is reported `Ready` (the
    /// `Init`/`SetSettings`/entitlements/auth snapshots of ScrapeRight). When the
    /// engine is `Ready` it is also sent now; when it is not, it is queued.
    pub fn set_snapshot(&self, key: &str, method: &str, params: Value) -> JobResult<()> {
        let live = {
            let mut g = self.sh.lock();
            match g.replay.iter_mut().find(|(k, _, _)| k == key) {
                Some(slot) => *slot = (key.into(), method.into(), params.clone()),
                None => g.replay.push((key.into(), method.into(), params.clone())),
            }
            (g.lifecycle == Lifecycle::Ready)
                .then(|| g.client.clone())
                .flatten()
        };
        match live {
            Some(client) => client
                .call_timeout(method, params, None, self.sh.cfg.rpc_timeout)
                .map(|_| ()),
            None => Ok(()),
        }
    }

    /// Lifecycle transitions, in order, from now on.
    pub fn subscribe_lifecycle(&self) -> Receiver<LifecycleEvent> {
        let (tx, rx) = channel();
        self.sh
            .lifecycle_subs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(tx);
        rx
    }

    /// Unsolicited engine events (frames without a request id), across
    /// generations, whole (so `session_id` survives).
    pub fn subscribe_events(&self) -> Receiver<Value> {
        let (tx, rx) = channel();
        self.sh
            .event_subs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(tx);
        rx
    }

    pub fn lifecycle(&self) -> Lifecycle {
        self.sh.lock().lifecycle
    }

    pub fn status(&self) -> SupervisorStatus {
        let g = self.sh.lock();
        SupervisorStatus {
            lifecycle: g.lifecycle,
            generation: g.generation,
            attempt: g.attempt,
            pid: g.client.as_ref().and_then(|c| c.child_id()),
            hello: g.hello.clone(),
            restarts_in_window: g.tracker.restarts_in_window(self.sh.now()),
            restarts: g.restarts,
            terminal: g.terminal.clone(),
            last_exit_code: g.last_exit.as_ref().and_then(|l| l.code),
            last_exit_reason: g.last_exit.as_ref().map(|l| l.reason.clone()),
            lifecycle_subscribers: self
                .sh
                .lifecycle_subs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .len(),
            event_subscribers: self
                .sh
                .event_subs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .len(),
            process: rightkit_process::process_usage().ok(),
        }
    }

    /// Block until `Ready` or terminal `Down`, up to `timeout`; true when `Ready`.
    pub fn wait_ready(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut g = self.sh.lock();
        loop {
            match g.lifecycle {
                Lifecycle::Ready => return true,
                Lifecycle::Down => return false,
                _ => {}
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            g = self
                .sh
                .cv
                .wait_timeout(g, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Stop the engine for good: ask it to shut down, then kill the tree after
    /// the grace period. Never restarts. Returns only after every engine process this
    /// supervisor spawned has been waited (no zombie behind `Down`), within the bound
    /// documented on [`Self::shutdown_detailed`]. Use that method to see how it ended.
    pub fn shutdown(&self) {
        let _ = self.shutdown_detailed();
    }

    /// As [`Self::shutdown`], reporting how the engine process ended.
    ///
    /// Bound: about `2 * shutdown_grace` for a running engine, `3 * grace + 500 ms` when a
    /// monitor was already reaping it, plus `hello_timeout` when a boot was caught between
    /// spawn and hello (that child is killed at once, so in practice it ends sooner). If
    /// the bound passes with a process still unwaited, the report says
    /// [`ShutdownOutcome::Unconfirmed`] and the terminal state is `Failed`, not `Stopped`.
    /// A tree that had to be killed reports [`ShutdownOutcome::Killed`].
    ///
    /// `kill -0 <pid>` is not a valid way to see that the engine is gone: the engine is a
    /// child of this process, and an exited child that was not waited is a zombie that
    /// still answers it. Use [`ShutdownReport::is_reaped`], [`ShutdownReport::exit_status`]
    /// or [`Self::unreaped_children`].
    pub fn shutdown_detailed(&self) -> ShutdownReport {
        let started = Instant::now();
        let grace = self.sh.cfg.shutdown_grace;
        let (clients, first) = {
            let mut g = self.sh.lock();
            if g.shutting_down {
                (Vec::new(), false)
            } else {
                g.shutting_down = true;
                g.tracker.shutdown();
                let clients: Vec<_> = g
                    .client
                    .take()
                    .into_iter()
                    .chain(g.booting.take())
                    .collect();
                (clients, true)
            }
        };
        let bound = grace * 4 + Duration::from_secs(1) + self.sh.cfg.hello_timeout;
        let deadline = started + bound;
        if !first {
            // Another caller is stopping it; do not return ahead of the reap.
            let reaped = self.sh.wait_live_zero(deadline);
            return self.report(
                ShutdownOutcome::AlreadyShuttingDown,
                ChildExit::default(),
                reaped,
                started,
            );
        }
        let mut exit = ChildExit::default();
        let mut worst = ShutdownOutcome::NoEngine;
        for c in &clients {
            let e = c.shutdown_detailed(grace);
            if e.pid.is_some() {
                let outcome = if e.reaped && !e.killed {
                    ShutdownOutcome::Exited
                } else if e.reaped {
                    ShutdownOutcome::Killed
                } else {
                    ShutdownOutcome::Unconfirmed
                };
                if outcome > worst {
                    worst = outcome;
                }
                if exit.pid.is_none() {
                    exit = e;
                }
            }
        }
        // A boot between spawn and hello: kill its child so the hello read ends.
        let slot = self
            .sh
            .boot_slot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some((mut child, live)) = slot {
            let pid = child.id();
            let status = match child.terminate_tree_bounded(grace) {
                Ok(rightkit_process::ReapOutcome::Exited(s)) => Some(s),
                _ => None,
            };
            drop(child);
            drop(live);
            if worst < ShutdownOutcome::Killed {
                worst = ShutdownOutcome::Killed;
            }
            if exit.pid.is_none() {
                exit = ChildExit {
                    pid: Some(pid),
                    reaped: true,
                    killed: true,
                    status,
                };
            }
        }
        // Anything still held elsewhere (a monitor mid-reap, a boot about to notice).
        let reaped = self.sh.wait_live_zero(deadline);
        if !reaped {
            worst = ShutdownOutcome::Unconfirmed;
        }
        let mut g = self.sh.lock();
        g.lifecycle = Lifecycle::Down;
        if reaped {
            g.terminal = Some(Terminal::Stopped);
            let msg = if worst == ShutdownOutcome::Killed {
                "engine shut down (killed after the grace period)"
            } else {
                "engine shut down"
            };
            self.sh.emit(&g, msg, None);
        } else {
            let text = "engine shutdown: process not confirmed reaped within the bound".to_string();
            g.terminal = Some(Terminal::Failed {
                reason: text.clone(),
            });
            self.sh.emit(&g, text, None);
        }
        drop(g);
        self.sh.cv.notify_all();
        self.report(worst, exit, reaped, started)
    }

    /// Engine processes this supervisor spawned that have not yet been waited. Zero means
    /// none is running and none is a zombie.
    pub fn unreaped_children(&self) -> usize {
        *self.sh.live.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn report(
        &self,
        outcome: ShutdownOutcome,
        exit: ChildExit,
        reaped: bool,
        started: Instant,
    ) -> ShutdownReport {
        ShutdownReport {
            outcome,
            pid: exit.pid,
            exit_status: exit.status,
            reaped,
            unreaped: self.unreaped_children(),
            elapsed: started.elapsed(),
        }
    }

    fn ready_client(&self) -> JobResult<(Arc<EngineClient>, u64)> {
        ready_shared(&self.sh)
    }
}

impl Drop for EngineSupervisor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// What `start` decided under the lock: nothing to do, wait on a boot in flight, or boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartPlan {
    Ready,
    Wait,
    Boot,
}

/// Moves `Down` to `Booting` (fresh budget) and reports what the caller must do.
fn begin_start(sh: &Arc<Shared>) -> StartPlan {
    let mut g = sh.lock();
    match g.lifecycle {
        Lifecycle::Ready => StartPlan::Ready,
        Lifecycle::Booting | Lifecycle::Recovering => StartPlan::Wait,
        Lifecycle::Down => {
            g.shutting_down = false;
            g.tracker.reset_budget();
            g.on_demand_pending = false;
            g.terminal = None;
            g.last_start_error = None;
            g.lifecycle = Lifecycle::Booting;
            StartPlan::Boot
        }
    }
}

fn run_start(sh: &Arc<Shared>, plan: StartPlan) -> JobResult<()> {
    match plan {
        StartPlan::Ready => Ok(()),
        StartPlan::Wait => ready_shared(sh).map(|_| ()),
        StartPlan::Boot => match boot(sh, true) {
            Ok(()) => Ok(()),
            Err(_) => settle(sh),
        },
    }
}

/// Waits out a boot still in progress, retries of a failed first boot included,
/// and reports where it ended: `Ready`, or `Down` with the error that ended it.
fn settle(sh: &Arc<Shared>) -> JobResult<()> {
    let mut g = sh.lock();
    loop {
        match g.lifecycle {
            Lifecycle::Ready => return Ok(()),
            Lifecycle::Down => {
                return Err(g
                    .last_start_error
                    .clone()
                    .unwrap_or_else(|| JobError::supervisor("down", "engine is down")))
            }
            Lifecycle::Booting | Lifecycle::Recovering => {}
        }
        g = sh
            .cv
            .wait_timeout(g, Duration::from_millis(200))
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
}

/// Outcome of a boot started by [`EngineSupervisor::start_background`]. Cheap to
/// clone; every clone reports the same result.
#[derive(Debug, Clone, Default)]
pub struct Readiness {
    slot: Arc<ReadySlot>,
}

#[derive(Debug, Default)]
struct ReadySlot {
    result: Mutex<Option<JobResult<()>>>,
    done: Condvar,
}

impl Readiness {
    /// Block until the boot ends: `Ok` once the engine is `Ready`, otherwise the
    /// error that `start()` would have returned.
    pub fn ready(&self) -> JobResult<()> {
        let mut g = self.slot.result.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(r) = g.as_ref() {
                return r.clone();
            }
            g = self.slot.done.wait(g).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// As [`Self::ready`], but gives up after `timeout` with `None`.
    pub fn wait_timeout(&self, timeout: Duration) -> Option<JobResult<()>> {
        let deadline = Instant::now() + timeout;
        let mut g = self.slot.result.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(r) = g.as_ref() {
                return Some(r.clone());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            g = self
                .slot
                .done
                .wait_timeout(g, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// The outcome if the boot has ended, without waiting.
    pub fn try_ready(&self) -> Option<JobResult<()>> {
        self.slot
            .result
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn finish(&self, result: JobResult<()>) {
        *self.slot.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
        self.slot.done.notify_all();
    }
}

fn ready_shared(sh: &Arc<Shared>) -> JobResult<(Arc<EngineClient>, u64)> {
    let deadline = Instant::now() + sh.cfg.ready_wait;
    let mut g = sh.lock();
    loop {
        match g.lifecycle {
            Lifecycle::Ready => {
                if let Some(c) = g.client.clone() {
                    return Ok((c, g.generation));
                }
            }
            Lifecycle::Down => {
                if g.shutting_down {
                    return Err(JobError::supervisor("down", "engine is shut down"));
                }
                if sh.cfg.restart.mode() == RestartMode::OnDemand && g.on_demand_pending {
                    let now = sh.now();
                    match g.tracker.request_restart(now) {
                        RestartOutcome::Restart(permit) => {
                            g.on_demand_pending = false;
                            g.restarts = g.restarts.saturating_add(1);
                            g.lifecycle = Lifecycle::Recovering;
                            sh.emit(&g, "restarting on demand", None);
                            schedule_restart(sh, permit);
                            continue;
                        }
                        _ => {
                            let text = "engine is down and its restart budget is spent";
                            g.terminal = Some(Terminal::Failed {
                                reason: text.to_string(),
                            });
                            return Err(JobError::supervisor("down", text));
                        }
                    }
                }
                let why = g
                    .last_exit
                    .as_ref()
                    .map(|l| format!(" (last exit: {})", l.reason))
                    .unwrap_or_default();
                return Err(JobError::supervisor("down", format!("engine is down{why}")));
            }
            Lifecycle::Booting | Lifecycle::Recovering => {}
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(JobError::supervisor_retryable(
                "not_ready",
                "engine did not become ready in time",
            ));
        }
        g = sh
            .cv
            .wait_timeout(g, left)
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
}

/// Boot one generation. `initial` is the user's `start()` (a failure is
/// returned and the supervisor stays `Down`); otherwise it is a restart whose
/// failure spends restart budget through the normal exit path.
fn boot(sh: &Arc<Shared>, initial: bool) -> JobResult<()> {
    let (generation, recovering) = {
        let mut g = sh.lock();
        if g.shutting_down {
            g.lifecycle = Lifecycle::Down;
            g.terminal = Some(Terminal::Stopped);
            sh.cv.notify_all();
            return Err(JobError::supervisor("down", "engine is shut down"));
        }
        g.generation += 1;
        g.attempt += 1;
        let now = sh.now();
        g.token = Some(g.tracker.started(now));
        let recovering = !initial;
        if initial {
            g.lifecycle = Lifecycle::Booting;
            sh.emit(&g, "engine starting", None);
        }
        (g.generation, recovering)
    };

    let fail = |reason: ExitReason, code: &str, message: String, stderr: String| -> JobResult<()> {
        let err = JobError::supervisor(code, message);
        // A contract mismatch is terminal on any boot; a first failure is terminal
        // only under `InitialFailure::Terminal`. Anything else spends restart budget.
        let terminal = code == CONTRACT_VERSION_MISMATCH
            || (initial && sh.cfg.initial_failure == InitialFailure::Terminal);
        if !terminal {
            if initial {
                sh.lock().last_start_error = Some(err.clone());
            }
            on_end(sh, generation, reason, None, stderr);
            return Err(err);
        }
        let mut g = sh.lock();
        if g.generation == generation && !g.shutting_down {
            let text = if initial {
                format!("engine failed to start: {err}")
            } else {
                format!("engine replacement refused: {err}")
            };
            g.last_exit = Some(LastExit {
                code: None,
                reason: reason.to_string(),
                stderr,
            });
            g.token = None;
            g.lifecycle = Lifecycle::Down;
            g.terminal = Some(Terminal::Failed {
                reason: text.clone(),
            });
            if initial {
                g.last_start_error = Some(err.clone());
            }
            sh.emit(&g, text, None);
        }
        drop(g);
        sh.cv.notify_all();
        Err(err)
    };

    let mut command = match (sh.cfg.spawn)() {
        Ok(c) => c,
        Err(e) => {
            return fail(
                ExitReason::SpawnFailed(e.to_string()),
                "spawn_failed",
                format!("could not build engine command: {e}"),
                String::new(),
            )
        }
    };
    command
        .command_mut()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            return fail(
                ExitReason::SpawnFailed(e.to_string()),
                "spawn_failed",
                format!("could not start engine: {e}"),
                String::new(),
            )
        }
    };
    let live = LiveChild::new(sh);
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.take_stdin(), child.take_stdout(), child.take_stderr())
    else {
        let _ = child.terminate_tree_bounded(sh.cfg.shutdown_grace);
        drop(child);
        drop(live);
        return fail(
            ExitReason::SpawnFailed("missing stdio".into()),
            "spawn_failed",
            "engine stdio was not piped".into(),
            String::new(),
        );
    };
    // Visible to `shutdown` from here until the client owns the child. Checked under the
    // lock `shutdown` sets its flag with, so one of the two sees the other.
    {
        let g = sh.lock();
        if g.shutting_down {
            drop(g);
            let _ = child.terminate_tree_bounded(sh.cfg.shutdown_grace);
            drop(child);
            drop(live);
            return Err(JobError::supervisor(
                "down",
                "engine was shut down while starting",
            ));
        }
        *sh.boot_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some((child, live));
    }
    let redact = sh.cfg.redact.clone();
    let sink = sh.cfg.on_stderr.clone();
    let mut tail = StderrTail::capture(
        stderr,
        sh.cfg.stderr_tail_bytes,
        move |l| match &redact {
            Some(r) => r(l),
            None => Some(l.to_string()),
        },
        move |l| {
            if let Some(s) = &sink {
                s(l);
            }
        },
    );
    let opts = ClientOptions {
        hello_timeout: sh.cfg.hello_timeout,
        frame_limit: sh.cfg.frame_limit,
        contract_version: sh.cfg.contract_version.clone(),
        fallback_error_code: sh.cfg.fallback_error_code.clone(),
        protocol_error_sink: sh.cfg.protocol_error_sink.clone(),
    };
    let client = match EngineClient::connect_opts(BufReader::new(stdout), stdin, opts) {
        Ok(c) => Arc::new(c),
        Err(e) => {
            let slot = sh
                .boot_slot
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            if let Some((mut child, live)) = slot {
                let _ = child.terminate_tree_bounded(sh.cfg.shutdown_grace);
                drop(child);
                drop(live);
            }
            let text = tail.finish_within(Duration::from_millis(500));
            return fail(
                ExitReason::HelloFailed(e.code.clone()),
                &e.code,
                e.message,
                text,
            );
        }
    };
    // Hand the child to the client, unless `shutdown` already took and killed it.
    {
        let mut g = sh.lock();
        let slot = sh
            .boot_slot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let Some((child, live)) = slot else {
            drop(g);
            return Err(JobError::supervisor(
                "down",
                "engine was shut down while starting",
            ));
        };
        client.attach_child_with(child, Box::new(live));
        g.booting = Some(Arc::clone(&client));
        if g.shutting_down {
            drop(g);
            client.kill(sh.cfg.shutdown_grace);
            return Err(JobError::supervisor(
                "down",
                "engine was shut down while starting",
            ));
        }
    }
    spawn_event_forwarder(sh, &client);

    // Handshake: replay the latest snapshots, in registration order.
    let replay = sh.lock().replay.clone();
    for (_, method, params) in replay {
        if let Err(e) = client.call_timeout(&method, params, None, sh.cfg.rpc_timeout) {
            client.kill(sh.cfg.shutdown_grace);
            let text = tail.finish_within(Duration::from_millis(500));
            return fail(
                ExitReason::HandshakeFailed(format!("{method}: {}", e.code)),
                "handshake_failed",
                format!("{method} failed during handshake: {e}"),
                text,
            );
        }
    }

    let mut g = sh.lock();
    if g.generation != generation || g.shutting_down {
        drop(g);
        client.kill(sh.cfg.shutdown_grace);
        return Err(JobError::supervisor(
            "down",
            "engine was shut down while starting",
        ));
    }
    g.hello = Some(client.hello().clone());
    g.booting = None;
    g.client = Some(Arc::clone(&client));
    g.lifecycle = Lifecycle::Ready;
    g.terminal = None;
    g.last_start_error = None;
    g.on_demand_pending = false;
    let recovery_ms = g
        .recovery_from
        .take()
        .map(|t| t.elapsed().as_millis() as u64)
        .filter(|_| recovering);
    sh.emit(&g, "engine is ready", recovery_ms);
    drop(g);
    sh.cv.notify_all();

    let shared = Arc::clone(sh);
    let _ = thread::Builder::new()
        .name(format!("{}-monitor", sh.cfg.name))
        .spawn(move || monitor(shared, generation, client, tail));
    Ok(())
}

fn spawn_event_forwarder(sh: &Arc<Shared>, client: &Arc<EngineClient>) {
    let rx = client.subscribe();
    let shared = Arc::clone(sh);
    let _ = thread::Builder::new()
        .name(format!("{}-events", sh.cfg.name))
        .spawn(move || {
            for frame in rx {
                shared
                    .event_subs
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .retain(|tx| tx.send(frame.clone()).is_ok());
            }
        });
}

/// Watches one generation until its connection ends, pinging if configured,
/// then reaps the process and hands the result to the restart policy.
fn monitor(sh: Arc<Shared>, generation: u64, client: Arc<EngineClient>, mut tail: StderrTail) {
    let health = sh.cfg.health.clone();
    let mut tracker = health.as_ref().map(|h| {
        let startup = h.interval * (h.unhealthy_threshold + 2) + h.timeout;
        HealthTracker::new(
            HealthPolicy::new(
                startup.max(Duration::from_millis(1)),
                h.unhealthy_threshold.max(1),
            )
            .expect("non-zero health policy"),
            Duration::ZERO,
        )
    });
    let t0 = Instant::now();
    let mut next_probe = t0 + health.as_ref().map(|h| h.interval).unwrap_or_default();
    let mut killed = None;
    loop {
        let wait = match &health {
            Some(_) => next_probe
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(5)),
            None => Duration::from_millis(200),
        };
        if client.wait_closed(wait) {
            break;
        }
        let (Some(h), Some(tr)) = (&health, tracker.as_mut()) else {
            continue;
        };
        if Instant::now() < next_probe {
            continue;
        }
        next_probe = Instant::now() + h.interval;
        let probe = match client.call_timeout(&h.method, json!({}), None, h.timeout) {
            Ok(_) => HealthProbe::Success,
            Err(e) if e.code == "engine_exited" => break,
            Err(_) => HealthProbe::Failure,
        };
        if matches!(
            tr.observe(t0.elapsed(), probe),
            HealthOutcome::Unhealthy { .. } | HealthOutcome::StartupTimedOut
        ) {
            *sh.wedge.lock().unwrap_or_else(|e| e.into_inner()) =
                Some((generation, ExitReason::HealthKill));
            killed = client.kill(sh.cfg.shutdown_grace);
            break;
        }
    }
    let status = killed.or_else(|| client.reap(sh.cfg.shutdown_grace));
    let code = status.and_then(|s| s.code());
    let text = tail.finish_within(Duration::from_millis(500));
    let reason = {
        let mut w = sh.wedge.lock().unwrap_or_else(|e| e.into_inner());
        match w.take() {
            Some((g, r)) if g == generation => r,
            other => {
                *w = other;
                if code.is_some() {
                    ExitReason::Exited
                } else {
                    ExitReason::Eof
                }
            }
        }
    };
    on_end(&sh, generation, reason, code, text);
}

fn kill_wedged(sh: &Arc<Shared>, generation: u64, client: &Arc<EngineClient>, reason: ExitReason) {
    *sh.wedge.lock().unwrap_or_else(|e| e.into_inner()) = Some((generation, reason));
    client.kill(sh.cfg.shutdown_grace);
}

/// A generation ended (or failed to boot). Stale generations are ignored; an
/// intentional shutdown never restarts; otherwise the restart policy decides.
fn on_end(
    sh: &Arc<Shared>,
    generation: u64,
    reason: ExitReason,
    code: Option<i32>,
    stderr: String,
) {
    let mut g = sh.lock();
    if g.generation != generation {
        return;
    }
    g.client = None;
    g.last_exit = Some(LastExit {
        code,
        reason: reason.to_string(),
        stderr,
    });
    if g.shutting_down {
        g.lifecycle = Lifecycle::Down;
        g.terminal = Some(Terminal::Stopped);
        drop(g);
        sh.cv.notify_all();
        return;
    }
    let Some(token) = g.token.take() else { return };
    let now = sh.now();
    let exit_msg = match code {
        Some(c) => format!("engine exited ({reason}, code {c})"),
        None => format!("engine ended ({reason})"),
    };
    match g.tracker.exited(token, now) {
        RestartOutcome::Restart(permit) => {
            g.lifecycle = Lifecycle::Recovering;
            g.terminal = None;
            g.restarts = g.restarts.saturating_add(1);
            g.recovery_from.get_or_insert_with(Instant::now);
            sh.emit(
                &g,
                format!("{exit_msg}; restarting in {:?}", permit.delay()),
                None,
            );
            drop(g);
            schedule_restart(sh, permit);
        }
        RestartOutcome::AwaitingDemand => {
            g.lifecycle = Lifecycle::Down;
            g.terminal = None;
            g.on_demand_pending = true;
            sh.emit(
                &g,
                format!("{exit_msg}; will restart on the next call"),
                None,
            );
            drop(g);
        }
        RestartOutcome::Exhausted => {
            g.lifecycle = Lifecycle::Down;
            let n = g.tracker.restarts_in_window(now);
            let text = format!("{exit_msg}; restart budget exhausted ({n} restarts in the window)");
            g.terminal = Some(Terminal::Failed {
                reason: text.clone(),
            });
            sh.emit(&g, text, None);
            drop(g);
        }
        RestartOutcome::Stopped | RestartOutcome::IgnoredStale => {
            g.lifecycle = Lifecycle::Down;
            let text = format!("{exit_msg}; restart disabled");
            g.terminal = Some(Terminal::Failed {
                reason: text.clone(),
            });
            sh.emit(&g, text, None);
            drop(g);
        }
    }
    sh.cv.notify_all();
}

/// Wait out the backoff, then boot the replacement if the permit is still valid.
fn schedule_restart(sh: &Arc<Shared>, permit: RestartPermit) {
    let shared = Arc::clone(sh);
    let _ = thread::Builder::new()
        .name(format!("{}-restart", sh.cfg.name))
        .spawn(move || {
            thread::sleep(permit.delay());
            {
                let g = shared.lock();
                if g.shutting_down
                    || g.lifecycle != Lifecycle::Recovering
                    || !g.tracker.permit_valid(&permit, shared.now())
                {
                    return;
                }
            }
            let _ = boot(&shared, false);
        });
}
