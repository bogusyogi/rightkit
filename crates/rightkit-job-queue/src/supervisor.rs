//! Engine sidecar supervision: spawn, hello, handshake replay, restart budget,
//! health pings, wedge recovery and lifecycle diagnostics, on top of
//! [`EngineClient`] (frames) and `rightkit-child-process` (child ownership, restart
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
//! - Secrets that must not be kept are not snapshots: [`SupervisorConfig::with_handshake`]
//!   evaluates a provider once per generation, after the snapshots and before `Ready`, and sends
//!   the pre-serialized, zeroized calls it returns without the supervisor ever retaining them.
//! - A call that exceeds its timeout treats the engine as wedged: the tree is
//!   killed and the normal restart path runs. An optional health check pings
//!   the engine and does the same after consecutive failures.

use std::fmt;
use std::io::{self, BufReader};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::process::Stdio;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use rightkit_child_process::{
    Generation, HealthOutcome, HealthPolicy, HealthProbe, HealthTracker, OwnedChild, OwnedCommand,
    RestartMode, RestartOutcome, RestartPermit, RestartPolicy, RestartTracker, StderrTail,
};
use serde::Serialize;
use serde_json::{json, Value};
use zeroize::Zeroizing;

use crate::client::{
    merge_event_callbacks, ChildExit, ClientOptions, ContractRequirement, EngineClient,
    OnEventFrame, ProgressDeadline, ProtocolErrorSink, DEFAULT_HELLO_TIMEOUT,
};
use crate::error::{ErrorOrigin, JobError, JobResult, CONTRACT_VERSION_MISMATCH, DELIVERY_UNKNOWN};
use crate::wire::{finite_frame_limit, CONTRACT_VERSION, DEFAULT_FRAME_LIMIT};

/// Code of the boot failure raised by [`SupervisorConfig::require_scrubbed_env`].
const ENV_NOT_SCRUBBED: &str = "env_not_scrubbed";

/// Default for [`SupervisorConfig::with_max_unreaped_children`].
const DEFAULT_MAX_UNREAPED: usize = 4;

/// Most handshake replay passes one boot makes. A `set_snapshot` that keeps landing while a
/// generation replays would otherwise extend the handshake without bound; past this the boot
/// fails (`handshake_failed`) and the restart policy decides what happens next.
pub const MAX_REPLAY_PASSES: u32 = 8;

type SpawnFn = dyn Fn() -> io::Result<OwnedCommand> + Send + Sync;
type Redactor = dyn Fn(&str) -> Option<String> + Send + Sync;
type LineSink = dyn Fn(&str) + Send + Sync;
type OnEvent = Box<dyn Fn(&Value) + Send>;
type HandshakeFn = dyn Fn(u64) -> Result<Vec<HandshakeCall>, JobError> + Send + Sync;
type LaunchFrameFn = dyn Fn(u64) -> Result<LaunchFrame, JobError> + Send + Sync;

/// Largest launch frame line in bytes, excluding its newline. See [`LaunchFrame`].
pub const MAX_LAUNCH_FRAME_BYTES: usize = 4096;

/// Code of a boot failure caused by the launch frame: the provider failed or panicked, the
/// frame was refused (not one line, or over [`MAX_LAUNCH_FRAME_BYTES`]), or the write failed or
/// did not finish within the hello budget.
pub const LAUNCH_FRAME_FAILED: &str = "launch_frame_failed";

/// The first line a [`SupervisorConfig::with_launch_frame`] engine reads on stdin, before it says
/// hello: `{"type":"launch","schema":..,"version":..,"params":..}`. It carries configuration the
/// engine needs before it starts slow work (for example which model backend to load).
///
/// The line is held in zeroizing bytes, is never parsed again by the supervisor, and appears in
/// no log, error, exit reason, stderr tail or lifecycle event. The type has no `Clone`, and its
/// `Debug` shows the length only.
///
/// The frame must be one line (no `\n` or `\r`) of at most [`MAX_LAUNCH_FRAME_BYTES`] bytes; the
/// supervisor refuses anything else before it spawns the engine.
pub struct LaunchFrame {
    line: Zeroizing<Vec<u8>>,
}

impl fmt::Debug for LaunchFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LaunchFrame({} bytes)", self.line.len())
    }
}

/// A writer that refuses to grow past a fixed capacity, so serialising a frame never reallocates
/// (a reallocation would leave an unzeroed copy of the payload behind).
struct CappedWriter<'a> {
    buf: &'a mut Vec<u8>,
    cap: usize,
}

impl io::Write for CappedWriter<'_> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.buf.len() + data.len() > self.cap {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "launch frame too large",
            ));
        }
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl LaunchFrame {
    /// Serialise `{"type":"launch","schema":schema,"version":version,"params":params}`. Fails
    /// with `bad_input` when the line would pass [`MAX_LAUNCH_FRAME_BYTES`] or `params` does not
    /// serialise; the message never contains the params.
    pub fn new(schema: &str, version: u32, params: &impl Serialize) -> JobResult<Self> {
        #[derive(Serialize)]
        struct Wire<'a, P: Serialize> {
            #[serde(rename = "type")]
            kind: &'static str,
            schema: &'a str,
            version: u32,
            params: &'a P,
        }
        let mut line = Zeroizing::new(Vec::with_capacity(MAX_LAUNCH_FRAME_BYTES));
        let wire = Wire {
            kind: "launch",
            schema,
            version,
            params,
        };
        let cap = MAX_LAUNCH_FRAME_BYTES;
        let written = serde_json::to_writer(
            CappedWriter {
                buf: &mut line,
                cap,
            },
            &wire,
        );
        match written {
            Ok(()) => Ok(Self { line }),
            Err(e) if e.is_io() => Err(JobError::bad_input(format!(
                "launch frame exceeds {MAX_LAUNCH_FRAME_BYTES} bytes"
            ))),
            Err(_) => Err(JobError::bad_input("launch frame params did not serialize")),
        }
    }

    /// A frame from an already serialised line (no trailing newline). Not checked here: the
    /// supervisor refuses a line with `\n` or `\r` in it, an empty line, or one over
    /// [`MAX_LAUNCH_FRAME_BYTES`] before it spawns the engine, failing the boot with
    /// `launch_frame_failed`.
    pub fn from_json_line(line: Zeroizing<Vec<u8>>) -> Self {
        Self { line }
    }

    /// Length of the line in bytes, without its newline.
    pub fn len(&self) -> usize {
        self.line.len()
    }

    /// Whether the line is empty (always refused at boot).
    pub fn is_empty(&self) -> bool {
        self.line.is_empty()
    }

    fn check(&self) -> Result<(), &'static str> {
        if self.line.is_empty() {
            Err("launch frame is empty")
        } else if self.line.len() > MAX_LAUNCH_FRAME_BYTES {
            Err("launch frame is over the size limit")
        } else if self.line.iter().any(|b| *b == b'\n' || *b == b'\r') {
            Err("launch frame is not one line")
        } else {
            Ok(())
        }
    }
}

/// Write `frame` and a newline to `writer` on its own thread and hand the writer back, so the
/// caller can pass it on. Returns an error if the write fails or has not finished within
/// `timeout`: a child that never reads stdin must not hang the boot (an OS pipe holds only so
/// much, and Windows' anonymous pipes buffer as little as 4 KiB). On a timeout the writer stays
/// with the blocked thread, which ends once the child is killed and the pipe breaks; the frame is
/// dropped (zeroized) when that thread finishes.
fn deliver_launch_frame<W: io::Write + Send + 'static>(
    mut writer: W,
    frame: LaunchFrame,
    timeout: Duration,
) -> Result<W, &'static str> {
    let (tx, rx) = channel::<io::Result<W>>();
    let spawned = thread::Builder::new()
        .name("launch-frame".into())
        .spawn(move || {
            let result = writer
                .write_all(&frame.line)
                .and_then(|()| writer.write_all(b"\n"))
                .and_then(|()| writer.flush())
                .map(|()| writer);
            drop(frame);
            let _ = tx.send(result);
        });
    if spawned.is_err() {
        return Err("launch frame writer thread did not start");
    }
    match rx.recv_timeout(timeout) {
        Ok(Ok(w)) => Ok(w),
        Ok(Err(_)) => Err("launch frame write failed"),
        Err(RecvTimeoutError::Timeout) => Err("launch frame write timed out"),
        Err(RecvTimeoutError::Disconnected) => Err("launch frame writer ended abnormally"),
    }
}

/// How a failed [`HandshakeCall`] is treated. See [`SupervisorConfig::with_handshake`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeMode {
    /// Any failure of the call (an engine error reply, a transport failure, a timeout) fails the
    /// boot, exactly like a failed snapshot replay.
    Required,
    /// An engine *error reply* (`ok: false`) is tolerated: the boot continues and a fixed,
    /// non-secret code is reported on the lifecycle stream (never the params, never the engine's
    /// message). Any *transport* failure still fails the boot, whatever the mode: a write error,
    /// `DELIVERY_UNKNOWN`, a timeout, the engine exiting or closing the connection, a protocol
    /// error, a frame over the limit, or a call the supervisor refused locally (`bad_input`).
    /// A connection that died is never reported `Ready`.
    BestEffort,
}

/// One call of a per-generation handshake: a method and its `params`, already serialized as a
/// single line of JSON, and what a failure means. Built by the provider given to
/// [`SupervisorConfig::with_handshake`], consumed by the supervisor, never kept.
///
/// `params_json` is JSON text, not a value: the supervisor never parses it into a
/// `serde_json::Value`, so the only copies of a secret in it are the caller's `Zeroizing` buffer
/// and the outbound frame buffer (see [`EngineClient::call_timeout_raw`]).
///
/// The type has no `Clone`, and its `Debug` shows the method and the params' length only.
#[non_exhaustive]
pub struct HandshakeCall {
    method: String,
    params_json: Zeroizing<Vec<u8>>,
    mode: HandshakeMode,
}

impl HandshakeCall {
    /// `params_json` must be exactly one JSON value on one line (no `\n` or `\r` anywhere);
    /// anything else is refused with `bad_input` before a byte is written, and fails the boot
    /// whatever `mode` says.
    pub fn new(
        method: impl Into<String>,
        params_json: Zeroizing<Vec<u8>>,
        mode: HandshakeMode,
    ) -> Self {
        Self {
            method: method.into(),
            params_json,
            mode,
        }
    }
    /// The engine method this call invokes.
    pub fn method(&self) -> &str {
        &self.method
    }
    /// How a failure of this call is treated.
    pub fn mode(&self) -> HandshakeMode {
        self.mode
    }
}

impl fmt::Debug for HandshakeCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HandshakeCall")
            .field("method", &self.method)
            .field("params_len", &self.params_json.len())
            .field("mode", &self.mode)
            .finish()
    }
}

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
    /// A snapshot replay call or a [`SupervisorConfig::with_handshake`] call failed.
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

/// Broadcast on lifecycle transitions & boot-stage diagnostics. Carries enough to debug a terminal
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
    /// child is a separate process; read it with `rightkit_child_process::process_usage` inside the engine.
    /// See `rightkit_child_process::ProcessUsage`.
    pub process: Option<rightkit_child_process::ProcessUsage>,
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

/// Receives every abnormal engine exit: a generation that ended (or failed to boot) while the
/// supervisor was not shutting down. Jobs does not depend on a crash reporter; the `crash`
/// feature ships `CrashExitObserver`, an adapter to `rightkit-crash::record_helper_exit`.
/// Called on the supervisor's reader or restart thread with no supervisor lock held, so a slow
/// observer delays only that thread. Keep it quick and never call back into the supervisor.
pub trait ExitObserver: Send + Sync {
    /// `helper` is [`SupervisorConfig::name`]; `exit` is a short description of the exit (reason,
    /// exit code and the restart decision, never the stderr tail); `restart_count` is how many
    /// restarts the supervisor has scheduled before this exit.
    fn engine_exited(&self, helper: &str, exit: &str, restart_count: u32);
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
    contract_check_required: bool,
    env_scrub_required: bool,
    max_unreaped: usize,
    initial_failure: InitialFailure,
    protocol_error_sink: Option<ProtocolErrorSink>,
    fallback_error_code: String,
    exit_observer: Option<Arc<dyn ExitObserver>>,
    handshake: Option<Arc<HandshakeFn>>,
    launch_frame: Option<Arc<LaunchFrameFn>>,
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
            contract_check_required: false,
            env_scrub_required: false,
            max_unreaped: DEFAULT_MAX_UNREAPED,
            initial_failure: InitialFailure::Retry,
            protocol_error_sink: None,
            fallback_error_code: "engine_error".into(),
            exit_observer: None,
            handshake: None,
            launch_frame: None,
        }
    }
    /// Deliver fresh secrets (for example provider API keys) to every engine generation before it
    /// is `Ready`, without the supervisor keeping them. `provider` is called with the generation
    /// number of the boot, on the booting thread with no supervisor lock held, once the snapshot
    /// replay has settled and before the switch to `Ready`; it returns the [`HandshakeCall`]s to
    /// send, in order, with this boot's [`SupervisorConfig::rpc_timeout`] for each. A restart
    /// boots a new generation and calls it again, so every generation reads its secrets fresh
    /// (a rotated key reaches the next generation without any `set_snapshot`).
    ///
    /// Retention: nothing on the supervisor holds handshake data. The provider's return value
    /// lives on the boot's stack; each call's params are sent straight from its `Zeroizing`
    /// buffer and the call is dropped (zeroized) right after its send, before the next one. The
    /// supervisor does not parse the params, log them, or put them in an error, an exit reason or
    /// a lifecycle message: failures are reported by method name and error code only (a failing
    /// provider by its error's `code` only). On the wire the params live only in the outbound
    /// frame buffer, which the client overwrites with zeros once the frame is written or the write
    /// failed. What this does not cover: the engine's own memory and what it does with the secret,
    /// the OS pipe buffers, the `Value` of the engine's reply, and whatever the provider keeps
    /// for itself.
    ///
    /// Failure: a [`HandshakeMode::Required`] call that fails in any way, a
    /// [`HandshakeMode::BestEffort`] call that fails in a way other than an engine error reply
    /// (see [`HandshakeMode`]), a provider that returns `Err` (or panics), and params that are
    /// not one line of JSON all fail the boot (`handshake_failed`) through the normal restart
    /// budget, like a failed snapshot. A shutdown during the handshake aborts it: no further call
    /// is sent and the provider is not asked again.
    ///
    /// A `set_snapshot` that lands while the handshake runs is replayed after it, still before
    /// `Ready`. Replaces any earlier provider.
    pub fn with_handshake(
        mut self,
        provider: impl Fn(u64) -> Result<Vec<HandshakeCall>, JobError> + Send + Sync + 'static,
    ) -> Self {
        self.handshake = Some(Arc::new(provider));
        self
    }
    /// Send a [`LaunchFrame`] as the engine's first stdin line, before it says hello, so it has
    /// its configuration before slow startup work (an engine that loads a model before hello
    /// needs this). `provider` is called with the generation number once per boot, on the booting
    /// thread with no supervisor lock held, before the engine is spawned; a restart boots a new
    /// generation and calls it again, so every generation gets current settings.
    ///
    /// Ordering: the frame is written right after the engine starts, on a writer thread, ahead of
    /// any snapshot replay, handshake call or request. The write counts against
    /// [`SupervisorConfig::hello_timeout`] (the hello wait gets what remains), so a child that
    /// never reads stdin ends the boot at that bound instead of hanging it.
    ///
    /// Failure: a provider that returns `Err` or panics, a frame that is not one line or is over
    /// [`MAX_LAUNCH_FRAME_BYTES`] (refused before the spawn), and a write that fails or does not
    /// finish in time all fail the boot with code `launch_frame_failed`, through the normal
    /// restart budget. Only the provider error's `code` is reported. The payload is never logged:
    /// it is in no stderr tail, lifecycle event, error or exit reason.
    ///
    /// Without a provider nothing is written and the stdin bytes are unchanged. Replaces any
    /// earlier provider.
    pub fn with_launch_frame(
        mut self,
        provider: impl Fn(u64) -> Result<LaunchFrame, JobError> + Send + Sync + 'static,
    ) -> Self {
        self.launch_frame = Some(Arc::new(provider));
        self
    }
    /// Report every abnormal engine exit (not an intentional shutdown) to `observer`, with the
    /// restart count at that point. Off by default.
    pub fn with_exit_observer(mut self, observer: Arc<dyn ExitObserver>) -> Self {
        self.exit_observer = Some(observer);
        self
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
    /// Require this wire contract's own version ([`CONTRACT_VERSION`]) from the engine; shorthand
    /// for `require_contract_version(CONTRACT_VERSION)`.
    pub fn require_current_contract_version(self) -> Self {
        self.require_contract_version(CONTRACT_VERSION)
    }
    /// Make contract-version checking mandatory: `start` and `start_background` refuse to boot
    /// (`contract_check_required`, not retryable, nothing spawned) unless a version was set with
    /// [`Self::require_contract_version`] or [`Self::require_current_contract_version`]. Off by
    /// default, because the check itself is opt-in.
    pub fn with_mandatory_contract_check(mut self) -> Self {
        self.contract_check_required = true;
        self
    }
    /// Refuse an engine command that did not clear the inherited environment. The spawn closure
    /// must call `OwnedCommand::env_allowlist` or `env_allowlist_with_defaults` (the consumer's own
    /// variables, such as a model-cache location, go in the allowlist); a command that did not
    /// fails the boot with `env_not_scrubbed`, which is terminal like a contract mismatch: it is a
    /// configuration error, not a crash to retry. Off by default. A command cleared through
    /// `command_mut().env_clear()` is not recognised (see `OwnedCommand::env_is_cleared`).
    pub fn require_scrubbed_env(mut self) -> Self {
        self.env_scrub_required = true;
        self
    }
    /// Most engine processes that may be spawned and not yet waited when another boot starts
    /// (default 4). A child the OS did not report gone within its kill bound is kept by a reaper
    /// until it is, and counts here. A boot is refused (`unreaped_children`, spends restart
    /// budget) only when MORE than `max` are unconfirmed; exactly `max` still allows one, so
    /// `max` is the number of stuck children tolerated, not the refusal threshold.
    pub fn with_max_unreaped_children(mut self, max: usize) -> Self {
        self.max_unreaped = max;
        self
    }
    fn contract_gate(&self) -> JobResult<()> {
        if self.contract_check_required && self.contract_version.is_none() {
            return Err(JobError::supervisor(
                "contract_check_required",
                "this supervisor requires a contract version but none was configured",
            ));
        }
        Ok(())
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
    /// Largest frame accepted from the engine; bounded to [`crate::MAX_FRAME_LIMIT`], so passing
    /// `usize::MAX` for "no limit" gives the ceiling, not an unbounded read.
    pub fn with_frame_limit(mut self, n: usize) -> Self {
        self.frame_limit = finite_frame_limit(n);
        self
    }
    /// The frame limit a boot uses: `frame_limit` bounded to [`crate::MAX_FRAME_LIMIT`] (the field
    /// is public, so it can be set past the ceiling without `with_frame_limit`).
    pub fn effective_frame_limit(&self) -> usize {
        finite_frame_limit(self.frame_limit)
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

/// One registered snapshot. `rev` changes whenever the value (or method) does, so a boot that
/// replays from a copy can tell which entries moved on while it was replaying.
#[derive(Clone)]
struct Snapshot {
    key: String,
    method: String,
    params: Value,
    rev: u64,
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
    replay: Vec<Snapshot>,
    /// Counter behind [`Snapshot::rev`]; bumped on every `set_snapshot`.
    replay_rev: u64,
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
    /// Fixed boot-stage diagnostics use the existing lifecycle stream, with no retained history.
    /// Unix milliseconds align these events with the fixture's process-side trace.
    fn mark(&self, generation: u64, stage: &str) {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let g = self.lock();
        self.emit(
            &g,
            format!("boot_stage {ms} gen {generation} {stage}"),
            None,
        );
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
                replay_rev: 0,
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
        self.sh.cfg.contract_gate()?;
        run_start(&self.sh, begin_start(&self.sh))
    }

    /// As [`Self::start`], but returns at once. The lifecycle is already
    /// `Booting` when this returns; the boot runs on a background thread and the
    /// [`Readiness`] handle reports how it ended. When the engine is already
    /// `Ready` there is nothing to boot and the handle is resolved before this returns.
    pub fn start_background(&self) -> Readiness {
        if let Err(e) = self.sh.cfg.contract_gate() {
            let readiness = Readiness::default();
            readiness.finish(Err(e));
            return readiness;
        }
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
        let result = client.call_timeout_frames(method, params, on_frame, timeout);
        self.finish_call(
            result,
            &client,
            generation,
            method,
            options.keep_alive_on_timeout,
            &format!("within {timeout:?}"),
        )
    }

    /// As [`Self::call_with`], but the call's budget is a [`ProgressDeadline`]: it fails with
    /// `timeout` after `idle` without an event tagged with this call's request id (and, when set,
    /// after `max_total`), instead of after one absolute deadline. `options.timeout` is ignored
    /// (the deadline replaces it); the callbacks and `keep_alive_on_timeout` apply as in `call_with`.
    /// Unsolicited events and other calls' events never count as progress. The absolute deadline of
    /// [`Self::call`] stays the default; use this only for a call that reports its own progress.
    pub fn call_with_progress_deadline(
        &self,
        method: &str,
        params: Value,
        options: CallOptions,
        deadline: ProgressDeadline,
    ) -> JobResult<Value> {
        let (client, generation) = self.ready_client()?;
        let on_frame = merge_event_callbacks(options.on_event, options.on_frame);
        let result = client.call_progress_frames(method, params, on_frame, deadline);
        self.finish_call(
            result,
            &client,
            generation,
            method,
            options.keep_alive_on_timeout,
            &format!("(idle {:?})", deadline.idle),
        )
    }

    fn finish_call(
        &self,
        result: JobResult<Value>,
        client: &Arc<EngineClient>,
        generation: u64,
        method: &str,
        keep_alive: bool,
        bound: &str,
    ) -> JobResult<Value> {
        match result {
            Err(e) if e.code == "timeout" => {
                if !keep_alive {
                    kill_wedged(&self.sh, generation, client, ExitReason::RpcTimeoutKill);
                }
                Err(JobError::supervisor_retryable(
                    "timeout",
                    format!(
                        "engine did not answer {method} {bound}{}",
                        if keep_alive {
                            ""
                        } else {
                            "; it will be restarted"
                        }
                    ),
                ))
            }
            // The write stalled: the client already killed (bounded) the engine that was not reading.
            // Record it as a wedge so the lifecycle says why, and do not suggest a blind retry:
            // the engine may have received the request.
            Err(e) if e.code == DELIVERY_UNKNOWN => {
                *self.sh.wedge.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some((generation, ExitReason::RpcTimeoutKill));
                Err(e)
            }
            // The engine went away under this call; a restart (if the policy allows) makes a retry meaningful.
            // Local validation refusals (`bad_input`) never reach here: they stay nonretryable.
            Err(e) if e.code == "engine_exited" || e.code == "engine_write" => {
                let mut out = JobError::supervisor_retryable(e.code.clone(), e.message.clone());
                if let Some(d) = e.details() {
                    out = out.with_details(d.clone());
                }
                Err(out)
            }
            other => other,
        }
    }

    /// Record the latest state of `key` and replay it, in registration order, to
    /// every future engine generation before it is reported `Ready` (the
    /// `Init`/`SetSettings`/entitlements/auth snapshots of ScrapeRight). When the
    /// engine is `Ready` it is also sent now; when it is not, it is queued. An update that arrives
    /// while a generation is booting, after its replay started, is replayed to that generation
    /// before it is reported `Ready` (it never boots on a value that was already superseded).
    pub fn set_snapshot(&self, key: &str, method: &str, params: Value) -> JobResult<()> {
        let live = {
            let mut g = self.sh.lock();
            g.replay_rev += 1;
            let entry = Snapshot {
                key: key.into(),
                method: method.into(),
                params: params.clone(),
                rev: g.replay_rev,
            };
            match g.replay.iter_mut().find(|e| e.key == key) {
                Some(slot) => *slot = entry,
                None => g.replay.push(entry),
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
            process: rightkit_child_process::process_usage().ok(),
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
                Ok(rightkit_child_process::ReapOutcome::Exited(s)) => Some(s),
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
    sh.mark(generation, "boot entered");

    let fail = |reason: ExitReason, code: &str, message: String, stderr: String| -> JobResult<()> {
        let err = JobError::supervisor(code, message);
        // A contract mismatch is terminal on any boot; a first failure is terminal
        // only under `InitialFailure::Terminal`. Anything else spends restart budget.
        let terminal = code == CONTRACT_VERSION_MISMATCH
            || code == ENV_NOT_SCRUBBED
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

    let unreaped = *sh.live.lock().unwrap_or_else(|e| e.into_inner());
    if unreaped > sh.cfg.max_unreaped {
        return fail(
            ExitReason::SpawnFailed("unreaped engines".into()),
            "unreaped_children",
            format!(
                "{unreaped} engine processes are still not confirmed gone; not starting another"
            ),
            String::new(),
        );
    }
    // Evaluated before the spawn so a provider error or a refused frame never starts a process.
    let launch_frame = match &sh.cfg.launch_frame {
        None => None,
        Some(provider) => {
            let provided = catch_unwind(AssertUnwindSafe(|| provider(generation)));
            let refused = match provided {
                Ok(Ok(frame)) => match frame.check() {
                    Ok(()) => Ok(frame),
                    Err(why) => Err(why.to_string()),
                },
                Ok(Err(e)) => Err(format!("launch frame provider failed: {}", e.code)),
                Err(_) => Err("launch frame provider panicked".to_string()),
            };
            match refused {
                Ok(frame) => Some(frame),
                Err(message) => {
                    return fail(
                        ExitReason::SpawnFailed(LAUNCH_FRAME_FAILED.into()),
                        LAUNCH_FRAME_FAILED,
                        message,
                        String::new(),
                    )
                }
            }
        }
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
    if sh.cfg.env_scrub_required && !command.env_is_cleared() {
        return fail(
            ExitReason::SpawnFailed("environment not scrubbed".into()),
            ENV_NOT_SCRUBBED,
            "the engine command inherits the parent environment; call env_allowlist".into(),
            String::new(),
        );
    }
    command
        .command_mut()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Diagnostics only: how long the process-wide spawn lock (taken by every spawn on Windows,
    // held across CreateProcess) takes to get, separate from the spawn itself.
    #[cfg(windows)]
    {
        sh.mark(generation, "spawn lock probe begin");
        rightkit_child_process::with_windows_spawn_lock(|| ());
        sh.mark(generation, "spawn lock probe end");
    }
    sh.mark(generation, "spawn begin");
    let spawned = command.spawn();
    sh.mark(
        generation,
        if spawned.is_ok() {
            "spawn returned ok"
        } else {
            "spawn returned error"
        },
    );
    let mut child = match spawned {
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
    // The launch frame goes first on stdin, before the client (and any snapshot or request)
    // can write; its time comes out of the hello budget.
    let booted_at = Instant::now();
    let stdin = match launch_frame {
        None => stdin,
        Some(frame) => {
            sh.mark(generation, "launch frame write begin");
            let delivered = deliver_launch_frame(stdin, frame, sh.cfg.hello_timeout);
            match delivered {
                Ok(stdin) => {
                    sh.mark(generation, "launch frame write ended ok");
                    stdin
                }
                Err(message) => {
                    sh.mark(generation, "launch frame write ended error");
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
                    if sh.lock().shutting_down {
                        return Err(JobError::supervisor(
                            "down",
                            "engine was shut down while starting",
                        ));
                    }
                    let text = tail.finish_within(Duration::from_millis(500));
                    return fail(
                        ExitReason::HelloFailed(LAUNCH_FRAME_FAILED.into()),
                        LAUNCH_FRAME_FAILED,
                        message.into(),
                        text,
                    );
                }
            }
        }
    };
    let opts = ClientOptions {
        hello_timeout: sh.cfg.hello_timeout.saturating_sub(booted_at.elapsed()),
        frame_limit: sh.cfg.effective_frame_limit(),
        contract_version: sh.cfg.contract_version.clone(),
        fallback_error_code: sh.cfg.fallback_error_code.clone(),
        protocol_error_sink: sh.cfg.protocol_error_sink.clone(),
    };
    sh.mark(generation, "hello wait begin");
    let connected = EngineClient::connect_opts(BufReader::new(stdout), stdin, opts);
    sh.mark(
        generation,
        if connected.is_ok() {
            "hello wait ended ok"
        } else {
            "hello wait ended error"
        },
    );
    let client = match connected {
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
    client.set_kill_bound(sh.cfg.shutdown_grace);
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

    // Handshake: replay the latest snapshots, in registration order. A `set_snapshot` that lands
    // while this generation is replaying is picked up by the next pass; the pass that finds
    // nothing new keeps the lock through the switch to `Ready`, so no update falls in the gap.
    let mut sent: Vec<(String, u64)> = Vec::new();
    let mut passes: u32 = 0;
    // The per-generation handshake runs once, when the snapshots have settled; a snapshot that
    // lands during it is picked up by the next pass of this loop.
    let mut handshake_pending = sh.cfg.handshake.is_some();
    let mut g = loop {
        let g = sh.lock();
        let todo: Vec<Snapshot> = g
            .replay
            .iter()
            .filter(|e| !sent.iter().any(|(k, rev)| *k == e.key && *rev == e.rev))
            .cloned()
            .collect();
        if todo.is_empty() {
            if !handshake_pending {
                break g;
            }
            drop(g);
            handshake_pending = false;
            match run_handshake(sh, generation, &client) {
                Ok(()) => continue,
                Err(HandshakeEnd::Shutdown) => {
                    client.kill(sh.cfg.shutdown_grace);
                    return Err(JobError::supervisor(
                        "down",
                        "engine was shut down while starting",
                    ));
                }
                Err(HandshakeEnd::Failed { reason, message }) => {
                    client.kill(sh.cfg.shutdown_grace);
                    let text = tail.finish_within(Duration::from_millis(500));
                    return fail(
                        ExitReason::HandshakeFailed(reason),
                        "handshake_failed",
                        message,
                        text,
                    );
                }
            }
        }
        drop(g);
        passes += 1;
        if passes > MAX_REPLAY_PASSES {
            client.kill(sh.cfg.shutdown_grace);
            let text = tail.finish_within(Duration::from_millis(500));
            return fail(
                ExitReason::HandshakeFailed("snapshot replay did not settle".into()),
                "handshake_failed",
                format!(
                    "snapshots kept changing: replay did not settle within {MAX_REPLAY_PASSES} passes"
                ),
                text,
            );
        }
        for e in todo {
            if let Err(err) =
                client.call_timeout(&e.method, e.params.clone(), None, sh.cfg.rpc_timeout)
            {
                client.kill(sh.cfg.shutdown_grace);
                let text = tail.finish_within(Duration::from_millis(500));
                return fail(
                    ExitReason::HandshakeFailed(format!("{}: {}", e.method, err.code)),
                    "handshake_failed",
                    format!("{} failed during handshake: {err}", e.method),
                    text,
                );
            }
            sent.retain(|(k, _)| *k != e.key);
            sent.push((e.key, e.rev));
        }
    };
    if g.generation != generation || g.shutting_down {
        drop(g);
        client.kill(sh.cfg.shutdown_grace);
        return Err(JobError::supervisor(
            "down",
            "engine was shut down while starting",
        ));
    }
    if let Some(token) = g.token {
        // Reaching ready ends the consecutive-failure run (and, by policy, the rolling budget).
        g.tracker.ready(token);
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
    sh.mark(generation, "ready");
    sh.cv.notify_all();

    let shared = Arc::clone(sh);
    let _ = thread::Builder::new()
        .name(format!("{}-monitor", sh.cfg.name))
        .spawn(move || monitor(shared, generation, client, tail));
    Ok(())
}

/// Why the per-generation handshake did not complete.
enum HandshakeEnd {
    /// The supervisor is shutting down or the generation is stale: abort without a failure.
    Shutdown,
    /// A call, the provider or the params failed. Both texts name a method and a code only.
    Failed { reason: String, message: String },
}

/// Fixed, non-secret code reported when a best-effort call is answered with an engine error.
const HANDSHAKE_BEST_EFFORT_ERROR: &str = "handshake_best_effort_error";

/// Evaluate the configured provider for `generation` and send its calls in order. Holds no
/// supervisor lock across a call, keeps nothing of the calls on the supervisor, and drops each
/// call (zeroizing its params) as soon as it has been sent.
fn run_handshake(
    sh: &Arc<Shared>,
    generation: u64,
    client: &EngineClient,
) -> Result<(), HandshakeEnd> {
    let Some(provider) = sh.cfg.handshake.clone() else {
        return Ok(());
    };
    let live = || {
        let g = sh.lock();
        g.generation == generation && !g.shutting_down
    };
    // Checked before the provider reads any secret, and before every call.
    if !live() {
        return Err(HandshakeEnd::Shutdown);
    }
    let calls = match catch_unwind(AssertUnwindSafe(|| provider(generation))) {
        Ok(Ok(calls)) => calls,
        Ok(Err(e)) => {
            return Err(HandshakeEnd::Failed {
                reason: format!("handshake provider: {}", e.code),
                message: format!("handshake provider failed: {}", e.code),
            })
        }
        Err(_) => {
            return Err(HandshakeEnd::Failed {
                reason: "handshake provider panicked".into(),
                message: "handshake provider panicked".into(),
            })
        }
    };
    // Each call is moved out of the vector by the iterator and dropped at the end of its turn;
    // calls not yet sent when the handshake aborts are dropped (zeroized) with the iterator.
    for call in calls {
        if !live() {
            return Err(HandshakeEnd::Shutdown);
        }
        let HandshakeCall {
            method,
            params_json,
            mode,
        } = call;
        let result = client.call_timeout_raw(&method, &params_json, sh.cfg.rpc_timeout);
        drop(params_json);
        match result {
            Ok(_) => {}
            // Only an engine error *reply* is tolerated, and only for a best-effort call.
            Err(e) if mode == HandshakeMode::BestEffort && e.origin == ErrorOrigin::Engine => {
                let g = sh.lock();
                sh.emit(&g, format!("{HANDSHAKE_BEST_EFFORT_ERROR}: {method}"), None);
            }
            Err(e) => {
                if !live() {
                    return Err(HandshakeEnd::Shutdown);
                }
                return Err(HandshakeEnd::Failed {
                    reason: format!("{method}: {}", e.code),
                    message: format!("{method} failed during handshake: {}", e.code),
                });
            }
        }
    }
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
            other if client.write_stalled() => {
                *w = other;
                ExitReason::RpcTimeoutKill
            }
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
    let restarts_before = g.restarts;
    let outcome = g.tracker.exited(token, now);
    let decision = match &outcome {
        RestartOutcome::Restart(_) => "restarting",
        RestartOutcome::AwaitingDemand => "restart on next call",
        RestartOutcome::Exhausted => "restart budget exhausted",
        RestartOutcome::Stopped | RestartOutcome::IgnoredStale => "restart disabled",
    };
    let observed = sh
        .cfg
        .exit_observer
        .clone()
        .map(|o| (o, format!("{exit_msg}; {decision}")));
    match outcome {
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
    if let Some((observer, text)) = observed {
        observer.engine_exited(&sh.cfg.name, &text, restarts_before);
    }
    sh.cv.notify_all();
}

/// Wait out the backoff, then boot the replacement if the permit is still valid.
fn schedule_restart(sh: &Arc<Shared>, permit: RestartPermit) {
    let shared = Arc::clone(sh);
    let delay = permit.delay();
    // Start after the exit was timed: passing this deadline also passes permit.not_before.
    let deadline = Instant::now().checked_add(delay);
    let _ = thread::Builder::new()
        .name(format!("{}-restart", sh.cfg.name))
        .spawn(move || {
            match deadline {
                Some(deadline) => sleep_until(deadline, Instant::now, thread::sleep),
                None => thread::sleep(delay),
            }
            shared.mark(0, "restart thread woke");
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

/// Recheck the monotonic clock after every wake. An early wake must not silently discard a
/// not-yet-valid restart permit and leave the supervisor Recovering without scheduled work.
fn sleep_until(
    deadline: Instant,
    mut now: impl FnMut() -> Instant,
    mut sleep: impl FnMut(Duration),
) {
    loop {
        let left = deadline.saturating_duration_since(now());
        if left.is_zero() {
            return;
        }
        sleep(left);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_restart_wait_rechecks_the_deadline_after_an_early_wake() {
        let began = Instant::now();
        let elapsed = std::cell::Cell::new(Duration::ZERO);
        let mut wakes = 0;
        sleep_until(
            began + Duration::from_millis(60),
            || began + elapsed.get(),
            |left| {
                wakes += 1;
                // Deterministic early wakes, including a final sub-clock-tick remainder.
                elapsed.set(elapsed.get() + (left / 3).max(Duration::from_nanos(1)));
            },
        );
        assert!(elapsed.get() >= Duration::from_millis(60));
        assert!(wakes > 1);
    }

    #[test]
    fn a_passed_restart_deadline_does_not_sleep() {
        let now = Instant::now();
        sleep_until(now, || now, |_| panic!("a passed deadline must not sleep"));
    }

    #[test]
    fn a_boot_is_refused_while_too_many_engines_are_unreaped() {
        let cfg = SupervisorConfig::new("gate", || -> io::Result<OwnedCommand> {
            panic!("the gate must refuse before the spawn closure runs")
        })
        .with_max_unreaped_children(1)
        .with_restart(
            RestartPolicy::new(
                RestartMode::Disabled,
                0,
                Duration::from_secs(1),
                Duration::from_millis(1),
                Duration::from_millis(1),
            )
            .unwrap(),
        );
        let s = EngineSupervisor::new(cfg);
        // Two engines that were killed but not yet confirmed gone (one more than the cap).
        let held = [LiveChild::new(&s.sh), LiveChild::new(&s.sh)];
        assert_eq!(s.unreaped_children(), 2);
        let err = s.start().unwrap_err();
        assert_eq!(err.code, "unreaped_children");
        drop(held);
        assert_eq!(s.unreaped_children(), 0);
    }

    #[test]
    fn the_cap_is_inclusive_so_a_single_unreaped_engine_still_allows_a_boot_attempt() {
        let cfg = SupervisorConfig::new("gate", || -> io::Result<OwnedCommand> {
            Err(io::Error::new(io::ErrorKind::NotFound, "no engine here"))
        })
        .with_max_unreaped_children(1)
        .with_initial_failure(InitialFailure::Terminal);
        let s = EngineSupervisor::new(cfg);
        let _held = LiveChild::new(&s.sh);
        // One unreaped child is within the cap: the gate passes and the spawn closure's own error
        // is what comes back.
        assert_eq!(s.start().unwrap_err().code, "spawn_failed");
    }
}

#[cfg(test)]
mod launch_frame_tests {
    use super::*;
    use std::sync::mpsc::sync_channel;

    fn frame(bytes: &[u8]) -> LaunchFrame {
        LaunchFrame::from_json_line(Zeroizing::new(bytes.to_vec()))
    }

    /// Shares its buffer so a test can read what the writer thread wrote.
    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Vec<u8>>>);
    impl io::Write for Shared {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A pipe nobody reads: every write blocks until the test releases it.
    struct Stuck(std::sync::mpsc::Receiver<()>);
    impl io::Write for Stuck {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            let _ = self.0.recv();
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct Broken;
    impl io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn writes_the_line_then_a_newline_and_returns_the_writer() {
        let out = Shared::default();
        let back = deliver_launch_frame(out.clone(), frame(b"{\"a\":1}"), Duration::from_secs(5));
        assert!(back.is_ok());
        assert_eq!(*out.0.lock().unwrap(), b"{\"a\":1}\n");
    }

    #[test]
    fn a_reader_that_never_drains_ends_at_the_timeout_not_forever() {
        // A frame at the size limit, as on a Windows pipe whose buffer is only 4 KiB.
        let (release, stuck) = sync_channel::<()>(1);
        let began = Instant::now();
        let r = deliver_launch_frame(
            Stuck(stuck),
            frame(&vec![b'x'; MAX_LAUNCH_FRAME_BYTES]),
            Duration::from_millis(150),
        );
        assert_eq!(r.err(), Some("launch frame write timed out"));
        assert!(began.elapsed() < Duration::from_secs(3));
        // Killing the child breaks the pipe; the writer thread then ends on its own.
        release.send(()).unwrap();
    }

    #[test]
    fn a_broken_pipe_is_a_write_failure() {
        let r = deliver_launch_frame(Broken, frame(b"{}"), Duration::from_secs(5));
        assert_eq!(r.err(), Some("launch frame write failed"));
    }

    #[test]
    fn check_accepts_exactly_one_nonempty_line_within_the_limit() {
        assert!(frame(b"{}").check().is_ok());
        assert!(frame(&vec![b'x'; MAX_LAUNCH_FRAME_BYTES]).check().is_ok());
        assert!(frame(&vec![b'x'; MAX_LAUNCH_FRAME_BYTES + 1])
            .check()
            .is_err());
        assert!(frame(b"").check().is_err());
        assert!(frame(b"a\nb").check().is_err());
        assert!(frame(b"a\r").check().is_err());
        assert!(!format!("{:?}", frame(b"SECRET")).contains("SECRET"));
    }
}
