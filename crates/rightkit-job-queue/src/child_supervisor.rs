//! Supervision of a resident child that cannot say hello (a vendor CLI), with a caller-supplied
//! readiness probe in place of the hello frame.
//!
//! [`EngineSupervisor`](crate::EngineSupervisor) owns a child that speaks this crate's framed
//! protocol: it waits for a `hello` frame, replays snapshots and routes calls. A vendor CLI
//! (Claude Code `stream-json`, for instance) cannot be changed to emit a hello. [`ChildSupervisor`]
//! is the layer below: it owns spawn, generation numbering, the restart budget, bounded and
//! idempotent stop, and lifecycle events, and it decides that a generation is ready by asking the
//! caller's probe. It speaks no protocol of its own: the child's stdio belongs to the caller.
//!
//! Model:
//! - One supervision thread per run. Every boot attempt is a generation, as in `EngineSupervisor`.
//! - A generation is booted by spawning the command the config builds, then polling the probe
//!   until it answers `true` (ready), errors (the boot fails), or `ready_timeout` passes. The
//!   child exiting before it is ready fails the boot.
//! - A boot failure, and any later exit of the child that nobody asked for, goes through the
//!   [`RestartPolicy`] exactly as an `EngineSupervisor` exit does (`Automatic` restarts after
//!   backoff until the rolling budget is spent, `OnDemand` and `Disabled` stay down until
//!   `start`).
//! - [`ChildSupervisor::stop`] is idempotent and bounded: the tree is asked to stop, then killed
//!   after the grace period, and the report says whether the child was reaped.
//! - [`ChildSupervisor::restart`] boots the next generation on demand without spending budget.

use std::io;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::process::{ChildStderr, ChildStdin, ChildStdout, ExitStatus};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use rightkit_child_process::{
    Generation, OwnedChild, OwnedCommand, RestartOutcome, RestartPolicy, RestartTracker,
    WaitOutcome,
};

use crate::error::{JobError, JobResult};
use crate::supervisor::{
    ExitReason, Lifecycle, LifecycleEvent, ShutdownOutcome, ShutdownReport, Terminal,
};

type SpawnFn = dyn Fn() -> io::Result<OwnedCommand> + Send + Sync;
type ProbeFn = dyn Fn(&mut ChildBoot) -> JobResult<bool> + Send + Sync;
type ReadyFn = dyn Fn(ChildSession) -> JobResult<()> + Send + Sync;

/// Default for [`ChildSupervisorConfig::ready_timeout`].
pub const DEFAULT_CHILD_READY_TIMEOUT: Duration = Duration::from_secs(30);

/// The child's standard streams, as far as the spawn command piped them. A stream the command did
/// not pipe is `None`.
#[derive(Debug, Default)]
pub struct ChildStdio {
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
}

/// What the readiness probe sees of one booting generation. The probe may `take()` the streams
/// it needs (to send a first message and read the reply, for example); whatever it leaves is
/// passed on to the [`ChildSupervisorConfig::with_on_ready`] hook once the probe says ready.
#[derive(Debug)]
pub struct ChildBoot {
    pub generation: u64,
    pub pid: u32,
    /// Time since this generation was spawned, refreshed before every probe call.
    pub elapsed: Duration,
    pub stdio: ChildStdio,
}

/// A generation that became ready, handed to [`ChildSupervisorConfig::with_on_ready`] once, with
/// the streams the probe left in place. The supervisor keeps none of them.
#[derive(Debug)]
pub struct ChildSession {
    pub generation: u64,
    pub pid: u32,
    pub stdio: ChildStdio,
}

/// How to run and recover one child.
pub struct ChildSupervisorConfig {
    /// Names threads and diagnostics.
    pub name: String,
    spawn: Arc<SpawnFn>,
    probe: Arc<ProbeFn>,
    on_ready: Option<Arc<ReadyFn>>,
    pub restart: RestartPolicy,
    /// A generation that is not ready within this long fails its boot.
    pub ready_timeout: Duration,
    /// Pause between probe calls, and the granularity at which stop and exit are noticed.
    pub poll_interval: Duration,
    /// Grace for the child to leave after a stop request before its tree is killed.
    pub stop_grace: Duration,
}

impl ChildSupervisorConfig {
    /// `spawn` builds a fresh command per generation and configures its stdio itself (pipe what
    /// the probe or the ready hook needs, `Stdio::null()` for the rest) and applies
    /// `windows_hide` and any environment scrub. `probe` is called repeatedly until it returns
    /// `Ok(true)`; `Ok(false)` means not yet, `Err` fails the boot.
    ///
    /// The probe runs on the supervision thread and must not block for long: stop and an exit of
    /// the child are noticed between probe calls. Read the child's output from a thread of your
    /// own, or with a bounded read, and have the probe test what that thread has seen.
    pub fn new(
        name: impl Into<String>,
        spawn: impl Fn() -> io::Result<OwnedCommand> + Send + Sync + 'static,
        probe: impl Fn(&mut ChildBoot) -> JobResult<bool> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            spawn: Arc::new(spawn),
            probe: Arc::new(probe),
            on_ready: None,
            restart: RestartPolicy::default(),
            ready_timeout: DEFAULT_CHILD_READY_TIMEOUT,
            poll_interval: Duration::from_millis(50),
            stop_grace: Duration::from_secs(2),
        }
    }
    /// Receive each ready generation with the streams the probe left. Called on the supervision
    /// thread before the supervisor reports `Ready`; an `Err` or a panic fails the boot and the
    /// child is stopped. Without this hook the streams left by the probe are dropped when the
    /// generation becomes ready, which closes the child's stdin.
    pub fn with_on_ready(
        mut self,
        hook: impl Fn(ChildSession) -> JobResult<()> + Send + Sync + 'static,
    ) -> Self {
        self.on_ready = Some(Arc::new(hook));
        self
    }
    pub fn with_restart(mut self, restart: RestartPolicy) -> Self {
        self.restart = restart;
        self
    }
    pub fn with_ready_timeout(mut self, d: Duration) -> Self {
        self.ready_timeout = d;
        self
    }
    pub fn with_poll_interval(mut self, d: Duration) -> Self {
        self.poll_interval = d.max(Duration::from_millis(1));
        self
    }
    pub fn with_stop_grace(mut self, d: Duration) -> Self {
        self.stop_grace = d;
        self
    }
}

/// Point-in-time view of a [`ChildSupervisor`].
#[derive(Debug, Clone)]
pub struct ChildStatus {
    pub lifecycle: Lifecycle,
    /// The newest generation booted (0 before any boot).
    pub generation: u64,
    /// Process id of the running generation, once spawned and until it ends.
    pub pid: Option<u32>,
    /// Restarts scheduled by the policy since the supervisor was created.
    pub restarts: u32,
    pub restarts_in_window: u32,
    /// Set while `Down` and not restarting by itself.
    pub terminal: Option<Terminal>,
    pub last_exit_code: Option<i32>,
    pub last_exit_reason: Option<String>,
}

#[derive(Clone)]
struct StopRecord {
    outcome: ShutdownOutcome,
    pid: Option<u32>,
    status: Option<ExitStatus>,
    reaped: bool,
}

struct State {
    lifecycle: Lifecycle,
    generation: u64,
    attempt: u64,
    pid: Option<u32>,
    /// Identifies the current supervision thread; bumped by every start.
    run_id: u64,
    /// True from the spawn of a supervision thread until it has fully ended.
    thread_alive: bool,
    stop_requested: bool,
    restart_requested: bool,
    tracker: RestartTracker,
    restarts: u32,
    terminal: Option<Terminal>,
    last_exit: Option<(Option<i32>, String)>,
    /// Why the most recent boot failed; what `start` returns when the run ends `Down`.
    last_error: Option<JobError>,
    recovery_from: Option<Instant>,
    last_stop: Option<StopRecord>,
}

struct Shared {
    cfg: ChildSupervisorConfig,
    epoch: Instant,
    st: Mutex<State>,
    cv: Condvar,
    subs: Mutex<Vec<Sender<LifecycleEvent>>>,
}

impl Shared {
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
            last_exit_code: st.last_exit.as_ref().and_then(|l| l.0),
            last_exit_reason: st.last_exit.as_ref().map(|l| l.1.clone()),
            recovery_ms,
            stderr_tail: None,
            restart_on_demand: false,
            message: message.into(),
        };
        self.subs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|tx| tx.send(ev.clone()).is_ok());
    }
}

/// Owns one resident child and keeps it alive according to its policy. See the module
/// documentation. Dropping it stops the child.
pub struct ChildSupervisor {
    sh: Arc<Shared>,
}

impl ChildSupervisor {
    pub fn new(cfg: ChildSupervisorConfig) -> Self {
        let tracker = RestartTracker::new(cfg.restart);
        let sh = Shared {
            cfg,
            epoch: Instant::now(),
            st: Mutex::new(State {
                lifecycle: Lifecycle::Down,
                generation: 0,
                attempt: 0,
                pid: None,
                run_id: 0,
                thread_alive: false,
                stop_requested: false,
                restart_requested: false,
                tracker,
                restarts: 0,
                terminal: None,
                last_exit: None,
                last_error: None,
                recovery_from: None,
                last_stop: None,
            }),
            cv: Condvar::new(),
            subs: Mutex::default(),
        };
        Self { sh: Arc::new(sh) }
    }

    /// Boot the child and return once it is `Ready`, or with the error that ended the run
    /// (`Down`). A run that is already booting or ready is waited on. From `Down` this starts a
    /// fresh run with a fresh restart budget. A failed boot is retried within the restart budget
    /// before `start` gives up.
    pub fn start(&self) -> JobResult<()> {
        self.start_background()?;
        self.await_ready_or_down(None)
    }

    /// As [`Self::start`], returning as soon as the supervision thread is running. Follow with
    /// [`Self::wait_ready`] or [`Self::subscribe_lifecycle`].
    pub fn start_background(&self) -> JobResult<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut g = self.sh.lock();
        loop {
            if !g.thread_alive {
                break;
            }
            if g.stop_requested {
                return Err(JobError::supervisor("down", "child is stopping"));
            }
            if g.lifecycle != Lifecycle::Down {
                return Ok(());
            }
            // A run that ended by itself is still winding its thread down.
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(JobError::supervisor(
                    "down",
                    "the previous run has not finished ending",
                ));
            }
            g = self
                .sh
                .cv
                .wait_timeout(g, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        g.stop_requested = false;
        g.restart_requested = false;
        g.tracker.reset_budget();
        g.terminal = None;
        g.last_error = None;
        g.last_stop = None;
        g.run_id += 1;
        g.lifecycle = Lifecycle::Booting;
        g.thread_alive = true;
        self.sh.emit(&g, "child starting", None);
        let run_id = g.run_id;
        drop(g);
        let shared = Arc::clone(&self.sh);
        let spawned = thread::Builder::new()
            .name(format!("{}-child", self.sh.cfg.name))
            .spawn(move || run(shared, run_id));
        if let Err(e) = spawned {
            let mut g = self.sh.lock();
            g.thread_alive = false;
            g.lifecycle = Lifecycle::Down;
            let text = format!("child supervision thread did not start: {e}");
            g.terminal = Some(Terminal::Failed {
                reason: text.clone(),
            });
            self.sh.emit(&g, text.clone(), None);
            drop(g);
            self.sh.cv.notify_all();
            return Err(JobError::supervisor("spawn_failed", text));
        }
        Ok(())
    }

    /// Stop the running generation and boot the next one at once, without spending restart
    /// budget and without backoff. Returns when the new generation is `Ready` or the run ended
    /// `Down`. From `Down` this is [`Self::start`].
    pub fn restart(&self) -> JobResult<()> {
        let from = {
            let mut g = self.sh.lock();
            if g.thread_alive && !g.stop_requested && g.lifecycle != Lifecycle::Down {
                g.restart_requested = true;
                Some(g.generation)
            } else {
                None
            }
        };
        match from {
            None => self.start(),
            Some(from) => {
                self.sh.cv.notify_all();
                self.await_ready_or_down(Some(from))
            }
        }
    }

    /// Stop the child for good (until the next [`Self::start`]): ask its tree to stop, kill it
    /// after the grace period, wait for it. Idempotent and bounded (about `4 * stop_grace` plus
    /// two seconds); a second caller while a stop is in flight waits for it and reports
    /// [`ShutdownOutcome::AlreadyShuttingDown`]. Never restarts.
    pub fn stop(&self) -> ShutdownReport {
        let started = Instant::now();
        let bound = self.sh.cfg.stop_grace * 4 + Duration::from_secs(2);
        let deadline = started + bound;
        let mut g = self.sh.lock();
        if !g.thread_alive {
            drop(g);
            return self.report(ShutdownOutcome::NoEngine, None, None, true, started);
        }
        let first = !g.stop_requested;
        if first {
            g.stop_requested = true;
            g.tracker.shutdown();
            g.last_stop = None;
        }
        self.sh.cv.notify_all();
        while g.thread_alive {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            g = self
                .sh
                .cv
                .wait_timeout(g, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        let alive = g.thread_alive;
        let record = g.last_stop.clone();
        drop(g);
        if alive {
            return self.report(ShutdownOutcome::Unconfirmed, None, None, false, started);
        }
        if !first {
            let (pid, status) = record.map_or((None, None), |r| (r.pid, r.status));
            return self.report(
                ShutdownOutcome::AlreadyShuttingDown,
                pid,
                status,
                true,
                started,
            );
        }
        match record {
            Some(r) => self.report(r.outcome, r.pid, r.status, r.reaped, started),
            None => self.report(ShutdownOutcome::NoEngine, None, None, true, started),
        }
    }

    fn report(
        &self,
        outcome: ShutdownOutcome,
        pid: Option<u32>,
        exit_status: Option<ExitStatus>,
        reaped: bool,
        started: Instant,
    ) -> ShutdownReport {
        ShutdownReport {
            outcome,
            pid,
            exit_status,
            reaped,
            unreaped: usize::from(!reaped),
            elapsed: started.elapsed(),
        }
    }

    /// Lifecycle transitions, in order, from now on.
    pub fn subscribe_lifecycle(&self) -> Receiver<LifecycleEvent> {
        let (tx, rx) = channel();
        self.sh
            .subs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(tx);
        rx
    }

    pub fn lifecycle(&self) -> Lifecycle {
        self.sh.lock().lifecycle
    }

    pub fn status(&self) -> ChildStatus {
        let g = self.sh.lock();
        ChildStatus {
            lifecycle: g.lifecycle,
            generation: g.generation,
            pid: g.pid,
            restarts: g.restarts,
            restarts_in_window: g.tracker.restarts_in_window(self.sh.now()),
            terminal: g.terminal.clone(),
            last_exit_code: g.last_exit.as_ref().and_then(|l| l.0),
            last_exit_reason: g.last_exit.as_ref().map(|l| l.1.clone()),
        }
    }

    /// Block until `Ready` or `Down`, up to `timeout`; true when `Ready`.
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

    /// Wait until the child is `Ready` (for a newer generation than `after`, when given) or the
    /// run is `Down`.
    fn await_ready_or_down(&self, after: Option<u64>) -> JobResult<()> {
        let mut g = self.sh.lock();
        loop {
            match g.lifecycle {
                Lifecycle::Ready if after.is_none_or(|from| g.generation > from) => return Ok(()),
                Lifecycle::Down => {
                    return Err(g
                        .last_error
                        .clone()
                        .unwrap_or_else(|| JobError::supervisor("down", "child is down")))
                }
                _ => {}
            }
            g = self
                .sh
                .cv
                .wait_timeout(g, Duration::from_millis(200))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

impl Drop for ChildSupervisor {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Marks the supervision thread over, however it ends.
struct AliveGuard(Arc<Shared>);

impl Drop for AliveGuard {
    fn drop(&mut self) {
        self.0.lock().thread_alive = false;
        self.0.cv.notify_all();
    }
}

enum Interrupt {
    Stop,
    Restart,
}

/// How one generation ended.
enum Ended {
    /// A stop was requested; the child was stopped and reaped.
    Stopped(StopRecord),
    /// `restart()` asked for the next generation; the child was stopped.
    RestartRequested,
    /// The boot failed or the child ended by itself.
    Exited {
        reason: ExitReason,
        code: Option<i32>,
    },
}

fn interrupt_of(g: &State, run_id: u64) -> Option<Interrupt> {
    if g.run_id != run_id || g.stop_requested {
        Some(Interrupt::Stop)
    } else if g.restart_requested {
        Some(Interrupt::Restart)
    } else {
        None
    }
}

fn interrupt(sh: &Shared, run_id: u64) -> Option<Interrupt> {
    interrupt_of(&sh.lock(), run_id)
}

/// Ask the tree to stop, kill it after `grace`, and reap it.
fn stop_child(child: &mut OwnedChild, grace: Duration) -> StopRecord {
    let pid = child.id();
    match child.wait_or_kill(grace) {
        Ok(WaitOutcome::Exited(status)) => StopRecord {
            outcome: ShutdownOutcome::Exited,
            pid: Some(pid),
            status: Some(status),
            reaped: true,
        },
        Ok(WaitOutcome::Terminated(status)) => StopRecord {
            outcome: ShutdownOutcome::Killed,
            pid: Some(pid),
            status: Some(status),
            reaped: true,
        },
        Err(_) => StopRecord {
            outcome: ShutdownOutcome::Unconfirmed,
            pid: Some(pid),
            status: None,
            reaped: false,
        },
    }
}

fn end_for(interrupt: Interrupt, child: &mut OwnedChild, grace: Duration) -> Ended {
    let record = stop_child(child, grace);
    match interrupt {
        Interrupt::Stop => Ended::Stopped(record),
        Interrupt::Restart => Ended::RestartRequested,
    }
}

/// Records why a boot failed (what `start` reports) and returns the matching end.
fn failed(
    sh: &Shared,
    code: &str,
    message: String,
    reason: ExitReason,
    exit: Option<i32>,
) -> Ended {
    sh.lock().last_error = Some(JobError::supervisor(code, message));
    Ended::Exited { reason, code: exit }
}

/// The supervision thread: boot a generation, watch it, apply the policy, repeat.
fn run(sh: Arc<Shared>, run_id: u64) {
    let _alive = AliveGuard(Arc::clone(&sh));
    loop {
        let (generation, token) = {
            let mut g = sh.lock();
            if g.run_id != run_id || g.stop_requested {
                settle_stopped(&sh, &mut g, None);
                return;
            }
            g.restart_requested = false;
            g.generation += 1;
            g.attempt += 1;
            let token = g.tracker.started(sh.now());
            (g.generation, token)
        };
        match boot_and_watch(&sh, run_id, generation, token) {
            Ended::Stopped(record) => {
                let mut g = sh.lock();
                settle_stopped(&sh, &mut g, Some(record));
                return;
            }
            Ended::RestartRequested => {
                let mut g = sh.lock();
                g.pid = None;
                g.lifecycle = Lifecycle::Recovering;
                g.recovery_from.get_or_insert_with(Instant::now);
                sh.emit(&g, "child restarting on request", None);
                drop(g);
                sh.cv.notify_all();
            }
            Ended::Exited { reason, code } => {
                let mut g = sh.lock();
                g.pid = None;
                g.last_exit = Some((code, reason.to_string()));
                if g.stop_requested || g.run_id != run_id {
                    settle_stopped(&sh, &mut g, None);
                    return;
                }
                let exit_msg = match code {
                    Some(c) => format!("child exited ({reason}, code {c})"),
                    None => format!("child ended ({reason})"),
                };
                let now = sh.now();
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
                        sh.cv.notify_all();
                        wait_backoff(&sh, run_id, permit.delay());
                    }
                    RestartOutcome::AwaitingDemand => {
                        g.lifecycle = Lifecycle::Down;
                        g.terminal = None;
                        sh.emit(&g, format!("{exit_msg}; start again to restart"), None);
                        drop(g);
                        sh.cv.notify_all();
                        return;
                    }
                    RestartOutcome::Exhausted => {
                        g.lifecycle = Lifecycle::Down;
                        let n = g.tracker.restarts_in_window(now);
                        let text = format!(
                            "{exit_msg}; restart budget exhausted ({n} restarts in the window)"
                        );
                        g.terminal = Some(Terminal::Failed {
                            reason: text.clone(),
                        });
                        sh.emit(&g, text, None);
                        drop(g);
                        sh.cv.notify_all();
                        return;
                    }
                    RestartOutcome::Stopped | RestartOutcome::IgnoredStale => {
                        g.lifecycle = Lifecycle::Down;
                        let text = format!("{exit_msg}; restart disabled");
                        g.terminal = Some(Terminal::Failed {
                            reason: text.clone(),
                        });
                        sh.emit(&g, text, None);
                        drop(g);
                        sh.cv.notify_all();
                        return;
                    }
                }
            }
        }
    }
}

fn settle_stopped(sh: &Shared, g: &mut State, record: Option<StopRecord>) {
    g.pid = None;
    g.lifecycle = Lifecycle::Down;
    g.terminal = Some(Terminal::Stopped);
    g.last_stop = Some(record.unwrap_or(StopRecord {
        outcome: ShutdownOutcome::NoEngine,
        pid: None,
        status: None,
        reaped: true,
    }));
    sh.emit(g, "child stopped", None);
    sh.cv.notify_all();
}

/// Sleep out the restart backoff; a stop or an explicit restart cuts it short.
fn wait_backoff(sh: &Shared, run_id: u64, delay: Duration) {
    let deadline = Instant::now() + delay;
    let mut g = sh.lock();
    loop {
        if g.run_id != run_id || g.stop_requested || g.restart_requested {
            return;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        g = sh
            .cv
            .wait_timeout(g, left)
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
}

fn boot_and_watch(sh: &Arc<Shared>, run_id: u64, generation: u64, token: Generation) -> Ended {
    let cfg = &sh.cfg;
    let command = match (cfg.spawn)() {
        Ok(c) => c,
        Err(e) => {
            return failed(
                sh,
                "spawn_failed",
                format!("could not build child command: {e}"),
                ExitReason::SpawnFailed(e.to_string()),
                None,
            )
        }
    };
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            return failed(
                sh,
                "spawn_failed",
                format!("could not start child: {e}"),
                ExitReason::SpawnFailed(e.to_string()),
                None,
            )
        }
    };
    child.set_drop_reap_timeout(cfg.stop_grace);
    let pid = child.id();
    sh.lock().pid = Some(pid);
    let mut boot = ChildBoot {
        generation,
        pid,
        elapsed: Duration::ZERO,
        stdio: ChildStdio {
            stdin: child.take_stdin(),
            stdout: child.take_stdout(),
            stderr: child.take_stderr(),
        },
    };
    let started = Instant::now();
    let deadline = started + cfg.ready_timeout;
    let handle = match child.process_handle() {
        Ok(h) => h,
        Err(e) => {
            let _ = stop_child(&mut child, cfg.stop_grace);
            return failed(
                sh,
                "spawn_failed",
                format!("could not watch child: {e}"),
                ExitReason::SpawnFailed(e.to_string()),
                None,
            );
        }
    };
    // Probe until ready.
    loop {
        if let Some(i) = interrupt(sh, run_id) {
            return end_for(i, &mut child, cfg.stop_grace);
        }
        match handle.wait_exited_timeout(Duration::ZERO) {
            Ok(true) => {
                let code = reap_exited(&mut child, cfg.stop_grace);
                return failed(
                    sh,
                    "child_exited_before_ready",
                    "the child exited before it was ready".into(),
                    ExitReason::HelloFailed("exited before ready".into()),
                    code,
                );
            }
            Ok(false) => {}
            Err(e) => {
                let _ = stop_child(&mut child, cfg.stop_grace);
                return failed(
                    sh,
                    "wait_failed",
                    format!("could not watch child: {e}"),
                    ExitReason::HelloFailed("wait failed".into()),
                    None,
                );
            }
        }
        boot.elapsed = started.elapsed();
        match catch_unwind(AssertUnwindSafe(|| (cfg.probe)(&mut boot))) {
            Ok(Ok(true)) => break,
            Ok(Ok(false)) => {}
            Ok(Err(e)) => {
                let _ = stop_child(&mut child, cfg.stop_grace);
                return failed(
                    sh,
                    "readiness_failed",
                    format!("readiness probe failed: {}", e.code),
                    ExitReason::HelloFailed(format!("readiness probe: {}", e.code)),
                    None,
                );
            }
            Err(_) => {
                let _ = stop_child(&mut child, cfg.stop_grace);
                return failed(
                    sh,
                    "readiness_failed",
                    "readiness probe panicked".into(),
                    ExitReason::HelloFailed("readiness probe panicked".into()),
                    None,
                );
            }
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            let _ = stop_child(&mut child, cfg.stop_grace);
            return failed(
                sh,
                "readiness_timeout",
                format!("child was not ready within {:?}", cfg.ready_timeout),
                ExitReason::HelloFailed("readiness timeout".into()),
                None,
            );
        }
        thread::sleep(cfg.poll_interval.min(left));
    }
    // Ready according to the probe: hand over what it left, then publish `Ready`.
    let ChildBoot { stdio, .. } = boot;
    if let Some(on_ready) = &cfg.on_ready {
        let session = ChildSession {
            generation,
            pid,
            stdio,
        };
        match catch_unwind(AssertUnwindSafe(|| on_ready(session))) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let _ = stop_child(&mut child, cfg.stop_grace);
                return failed(
                    sh,
                    "adopt_failed",
                    format!("ready hook failed: {}", e.code),
                    ExitReason::HelloFailed(format!("ready hook: {}", e.code)),
                    None,
                );
            }
            Err(_) => {
                let _ = stop_child(&mut child, cfg.stop_grace);
                return failed(
                    sh,
                    "adopt_failed",
                    "ready hook panicked".into(),
                    ExitReason::HelloFailed("ready hook panicked".into()),
                    None,
                );
            }
        }
    }
    {
        let mut g = sh.lock();
        if let Some(i) = interrupt_of(&g, run_id) {
            drop(g);
            return end_for(i, &mut child, cfg.stop_grace);
        }
        g.tracker.ready(token);
        g.lifecycle = Lifecycle::Ready;
        g.terminal = None;
        g.last_error = None;
        let recovery_ms = g
            .recovery_from
            .take()
            .map(|t| t.elapsed().as_millis() as u64);
        sh.emit(&g, "child is ready", recovery_ms);
    }
    sh.cv.notify_all();
    // Watch until it ends or is told to.
    loop {
        match handle.wait_exited_timeout(cfg.poll_interval) {
            Ok(true) => {
                let code = reap_exited(&mut child, cfg.stop_grace);
                return Ended::Exited {
                    reason: ExitReason::Exited,
                    code,
                };
            }
            Ok(false) => {}
            Err(e) => {
                let _ = stop_child(&mut child, cfg.stop_grace);
                return failed(
                    sh,
                    "wait_failed",
                    format!("could not watch child: {e}"),
                    ExitReason::Eof,
                    None,
                );
            }
        }
        if let Some(i) = interrupt(sh, run_id) {
            return end_for(i, &mut child, cfg.stop_grace);
        }
    }
}

/// The leader has exited: bring down anything it left behind, reap it, and return its exit code.
fn reap_exited(child: &mut OwnedChild, grace: Duration) -> Option<i32> {
    match child.wait_or_kill(grace) {
        Ok(WaitOutcome::Exited(status)) | Ok(WaitOutcome::Terminated(status)) => status.code(),
        Err(_) => None,
    }
}
