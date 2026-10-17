//! A supervisor loop for a daemon-pipe child (issue #295).
//!
//! A [`PrivateChild`] is an engine the app started itself and talks to over the daemon pipe. The
//! `rightkit-job-queue` engine supervisor needs a sidecar stdio `hello` frame, which a daemon
//! service does not speak; this one watches such a child with the two `rightkit-child-process`
//! trackers instead:
//!
//! * **poll**: every [`SupervisorConfig::poll_interval`] the loop asks whether the child has
//!   exited (`try_wait`, no reaping surprises: the supervisor owns the child);
//! * **probe**: a caller-supplied closure says whether the child answers (the usual one is
//!   [`ping_probe`]: connect with the child's own secret, verify the host, call `ping`). Each
//!   result is fed to a [`HealthTracker`];
//! * **relaunch**: an exit, an unhealthy child or a startup that never became healthy is stopped
//!   (stdin closed first, then the tree is killed after [`SupervisorConfig::stop_grace`]) and the
//!   [`RestartTracker`] decides: relaunch after its backoff, wait for demand, or give up
//!   ([`SupervisorEvent::Exhausted`]).
//!
//! Decisions that are easy to get wrong, and what was chosen:
//!
//! * While a generation has never answered a probe, failures are expected (it is starting), so
//!   only [`HealthOutcome::StartupTimedOut`] acts; `Unhealthy` acts only after the first success.
//! * The first successful probe calls [`RestartTracker::ready`], so a child that comes up and later
//!   crashes starts again from the base backoff (per the restart policy's own rules).
//! * A failed launch (the closure returned an error) is charged to the restart budget like a
//!   crash, so a binary that cannot start is not retried at poll speed forever.
//! * [`SupervisorHandle::restart_now`] on a running child counts as an exit of that generation.
//!   After the policy gave up ([`SupervisorState::Exhausted`]) it forgets the budget and starts
//!   again: an explicit user start.
//! * The probe runs on the supervisor thread and may block as long as its own deadline; give it a
//!   short one ([`ping_probe`] takes the bound as an argument).
//! * The child is created, probed and stopped on the supervisor thread only; the launch closure is
//!   the one thing that crosses to it.

use crate::client::{Client, ClientConfig};
use crate::host_identity::HostExpectation;
use crate::private::PrivateChild;
use rightkit_child_process::{
    Generation, HealthOutcome, HealthPolicy, HealthProbe, HealthTracker, RestartOutcome,
    RestartPermit, RestartPolicy, RestartTracker,
};
use serde_json::json;
use std::io;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Default [`SupervisorConfig::poll_interval`].
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// Default [`SupervisorConfig::stop_grace`].
pub const DEFAULT_STOP_GRACE: Duration = Duration::from_secs(5);

const MIN_POLL: Duration = Duration::from_millis(10);

/// How a child is watched and restarted.
#[derive(Debug, Clone, Copy)]
pub struct SupervisorConfig {
    restart: RestartPolicy,
    health: HealthPolicy,
    poll_interval: Duration,
    stop_grace: Duration,
}

impl SupervisorConfig {
    /// `restart` decides when and whether a stopped child starts again; `health` decides when
    /// probe results make a child unhealthy or a start a failure.
    pub fn new(restart: RestartPolicy, health: HealthPolicy) -> Self {
        Self {
            restart,
            health,
            poll_interval: DEFAULT_POLL_INTERVAL,
            stop_grace: DEFAULT_STOP_GRACE,
        }
    }

    /// Time between checks (exit poll plus one probe). Default [`DEFAULT_POLL_INTERVAL`]; raised
    /// to at least 10 ms. A restart's backoff is honoured to its own precision, not this one's.
    #[must_use]
    pub fn poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval.max(MIN_POLL);
        self
    }

    /// How long a child gets to exit by itself after its stdin was closed before its process tree
    /// is killed. Default [`DEFAULT_STOP_GRACE`].
    #[must_use]
    pub fn stop_grace(mut self, grace: Duration) -> Self {
        self.stop_grace = grace;
        self
    }
}

/// What the supervisor is doing now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorState {
    /// A child is running and has not yet answered a probe.
    Starting,
    /// A child is running and has answered a probe.
    Ready,
    /// The child is gone and the next start is waiting out its backoff.
    WaitingRestart,
    /// The child is gone and the policy waits for [`SupervisorHandle::restart_now`].
    AwaitingDemand,
    /// The restart budget is spent; nothing runs until [`SupervisorHandle::restart_now`].
    Exhausted,
    /// The policy never restarts and the child is gone, or the supervisor has stopped.
    Stopped,
}

/// A snapshot of the supervisor, from [`SupervisorHandle::status`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorStatus {
    pub state: SupervisorState,
    /// Children started so far (the running one included).
    pub launches: u64,
    /// The running child's process id, when there is one.
    pub pid: Option<u32>,
}

/// What happened, content-free. `launch` is the 1-based count of children started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorEvent {
    Started { launch: u64, pid: u32 },
    /// The child answered its first probe.
    Ready { launch: u64 },
    /// The child exited by itself.
    Exited {
        launch: u64,
        code: Option<i32>,
        success: bool,
    },
    /// A child that had been healthy failed `consecutive_failures` probes in a row; it is stopped.
    Unhealthy {
        launch: u64,
        consecutive_failures: u32,
    },
    /// The child never answered a probe within the policy's startup timeout; it is stopped.
    StartupTimedOut { launch: u64 },
    /// The launch closure failed (never started a child).
    LaunchFailed { error: String },
    RestartScheduled { delay: Duration },
    AwaitingDemand,
    /// The restart budget is spent.
    Exhausted,
    /// The supervisor will not start another child (a policy that never restarts, or `stop`).
    Stopped,
}

/// Callback for [`SupervisorEvent`]s. Runs on the supervisor thread; keep it short.
pub type SupervisorEventFn = Arc<dyn Fn(&SupervisorEvent) + Send + Sync>;

enum Cmd {
    Stop,
    RestartNow,
}

/// Control of a running supervisor. Dropping it stops the supervisor and its child.
pub struct SupervisorHandle {
    tx: mpsc::Sender<Cmd>,
    status: Arc<Mutex<SupervisorStatus>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SupervisorHandle {
    /// The state now.
    pub fn status(&self) -> SupervisorStatus {
        *self.status.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Ask for a start: a running child is stopped and counted as an exit (then the policy
    /// decides, so a policy that waits for demand starts at once); a child waiting for demand
    /// starts; an exhausted or stopped policy forgets its budget and starts. A pending backoff is
    /// left to run. Returns before the work is done.
    pub fn restart_now(&self) {
        let _ = self.tx.send(Cmd::RestartNow);
    }

    /// Stop the child (stdin closed, then killed after the grace) and end the supervisor. Waits
    /// for the thread to finish.
    pub fn stop(mut self) {
        self.stop_inner();
    }

    fn stop_inner(&mut self) {
        let _ = self.tx.send(Cmd::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for SupervisorHandle {
    fn drop(&mut self) {
        self.stop_inner();
    }
}

/// Start supervising. `launch` makes a child (normally [`PrivateChild::spawn`] of an
/// `OwnedCommand`, with the per-run suite made fresh each time if the child needs one); `probe`
/// says whether the running child answers (see [`ping_probe`]); `on_event` hears what happens.
/// The first child is started at once, on the supervisor thread.
pub fn supervise<L, P>(
    config: SupervisorConfig,
    launch: L,
    probe: P,
    on_event: Option<SupervisorEventFn>,
) -> io::Result<SupervisorHandle>
where
    L: FnMut() -> io::Result<PrivateChild> + Send + 'static,
    P: FnMut(&PrivateChild) -> bool + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let status = Arc::new(Mutex::new(SupervisorStatus {
        state: SupervisorState::Starting,
        launches: 0,
        pid: None,
    }));
    let core = Core {
        cfg: config,
        launch,
        probe,
        tracker: RestartTracker::new(config.restart),
        origin: Instant::now(),
        launches: 0,
        status: status.clone(),
        on_event,
        phase: Phase::Idle(SupervisorState::Stopped),
    };
    let thread = std::thread::Builder::new()
        .name("rk-supervisor".into())
        .spawn(move || core.run(rx))?;
    Ok(SupervisorHandle {
        tx,
        status,
        thread: Some(thread),
    })
}

/// A probe for [`supervise`]: connect to the child's service with the child's own secret, verify
/// the host when `expect` is given, and call `ping`. True only for a `{"pong": true}` answer.
/// `timeout` bounds one probe (connect, hello and the call). Use [`HostExpectation`] whenever the
/// secret is sent: a secret goes to whatever answered at the endpoint.
pub fn ping_probe(
    config: ClientConfig,
    expect: Option<HostExpectation>,
    timeout: Duration,
) -> impl FnMut(&PrivateChild) -> bool + Send + 'static {
    move |child| {
        let config = config
            .clone()
            .secret(child.secret().clone())
            .deadline(Some(timeout));
        let client = match &expect {
            Some(expect) => Client::connect_expecting(&config, expect).ok(),
            None => Client::connect_config(&config).ok(),
        };
        match client {
            Some(client) => matches!(
                client.call("ping", json!({})),
                Ok(value) if value["pong"] == true
            ),
            None => false,
        }
    }
}

struct Running {
    child: PrivateChild,
    generation: Generation,
    health: HealthTracker,
    ready: bool,
    launch: u64,
}

enum Phase {
    Running(Running),
    /// The next start is waiting for `due`.
    Waiting(RestartPermit, Instant),
    /// Nothing runs: `AwaitingDemand`, `Exhausted` or `Stopped`.
    Idle(SupervisorState),
}

struct Core<L, P> {
    cfg: SupervisorConfig,
    launch: L,
    probe: P,
    tracker: RestartTracker,
    origin: Instant,
    launches: u64,
    status: Arc<Mutex<SupervisorStatus>>,
    on_event: Option<SupervisorEventFn>,
    phase: Phase,
}

impl<L, P> Core<L, P>
where
    L: FnMut() -> io::Result<PrivateChild>,
    P: FnMut(&PrivateChild) -> bool,
{
    fn at(&self) -> Duration {
        self.origin.elapsed()
    }

    fn emit(&self, event: SupervisorEvent) {
        if let Some(f) = &self.on_event {
            f(&event);
        }
    }

    fn set_status(&self, state: SupervisorState, pid: Option<u32>) {
        let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
        s.state = state;
        s.launches = self.launches;
        s.pid = pid;
    }

    fn run(mut self, rx: mpsc::Receiver<Cmd>) {
        self.start();
        loop {
            let wait = match &self.phase {
                Phase::Waiting(_, due) => due
                    .saturating_duration_since(Instant::now())
                    .min(self.cfg.poll_interval),
                _ => self.cfg.poll_interval,
            };
            match rx.recv_timeout(wait) {
                Ok(Cmd::RestartNow) => self.restart_now(),
                Ok(Cmd::Stop) | Err(RecvTimeoutError::Disconnected) => {
                    self.shutdown();
                    return;
                }
                Err(RecvTimeoutError::Timeout) => self.tick(),
            }
        }
    }

    /// Launch a child now. A launch failure is charged to the budget like a crash.
    fn start(&mut self) {
        match (self.launch)() {
            Ok(child) => {
                let at = self.at();
                let generation = self.tracker.started(at);
                self.launches += 1;
                let pid = child.child().id();
                self.set_status(SupervisorState::Starting, Some(pid));
                self.emit(SupervisorEvent::Started {
                    launch: self.launches,
                    pid,
                });
                self.phase = Phase::Running(Running {
                    child,
                    generation,
                    health: HealthTracker::new(self.cfg.health, at),
                    ready: false,
                    launch: self.launches,
                });
            }
            Err(error) => {
                self.emit(SupervisorEvent::LaunchFailed {
                    error: error.to_string(),
                });
                let outcome = self.tracker.request_restart(self.at());
                self.apply(outcome);
            }
        }
    }

    fn apply(&mut self, outcome: RestartOutcome) {
        match outcome {
            RestartOutcome::Restart(permit) => {
                let delay = permit.delay();
                self.emit(SupervisorEvent::RestartScheduled { delay });
                self.set_status(SupervisorState::WaitingRestart, None);
                self.phase = Phase::Waiting(permit, Instant::now() + delay);
            }
            RestartOutcome::AwaitingDemand => {
                self.emit(SupervisorEvent::AwaitingDemand);
                self.set_status(SupervisorState::AwaitingDemand, None);
                self.phase = Phase::Idle(SupervisorState::AwaitingDemand);
            }
            RestartOutcome::Exhausted => {
                self.emit(SupervisorEvent::Exhausted);
                self.set_status(SupervisorState::Exhausted, None);
                self.phase = Phase::Idle(SupervisorState::Exhausted);
            }
            // `IgnoredStale` cannot happen for a generation this loop owns; if it ever did,
            // stopping loudly beats a silent loop.
            RestartOutcome::Stopped | RestartOutcome::IgnoredStale => {
                self.emit(SupervisorEvent::Stopped);
                self.set_status(SupervisorState::Stopped, None);
                self.phase = Phase::Idle(SupervisorState::Stopped);
            }
        }
    }

    fn tick(&mut self) {
        let placeholder = Phase::Idle(SupervisorState::Stopped);
        match std::mem::replace(&mut self.phase, placeholder) {
            Phase::Running(running) => self.tick_running(running),
            Phase::Waiting(permit, due) => {
                if Instant::now() >= due && self.tracker.permit_valid(&permit, self.at()) {
                    self.start();
                } else {
                    self.phase = Phase::Waiting(permit, due);
                }
            }
            idle @ Phase::Idle(_) => self.phase = idle,
        }
    }

    fn tick_running(&mut self, mut running: Running) {
        match running.child.child_mut().try_wait() {
            Ok(Some(status)) => {
                self.emit(SupervisorEvent::Exited {
                    launch: running.launch,
                    code: status.code(),
                    success: status.success(),
                });
                // The child has been reaped by `try_wait`; dropping the wrapper closes its stdin.
                drop(running.child);
                let outcome = self.tracker.exited(running.generation, self.at());
                self.apply(outcome);
                return;
            }
            Ok(None) => {}
            Err(_) => {
                // The state of the child cannot be read: do not guess it is fine.
                self.stop_child(running.child);
                let outcome = self.tracker.exited(running.generation, self.at());
                self.apply(outcome);
                return;
            }
        }
        let answered = (self.probe)(&running.child);
        let at = self.at();
        let outcome = running.health.observe(
            at,
            if answered {
                HealthProbe::Success
            } else {
                HealthProbe::Failure
            },
        );
        if answered && !running.ready {
            running.ready = true;
            self.tracker.ready(running.generation);
            self.set_status(SupervisorState::Ready, Some(running.child.child().id()));
            self.emit(SupervisorEvent::Ready {
                launch: running.launch,
            });
        }
        let failure = match outcome {
            HealthOutcome::StartupTimedOut => Some(SupervisorEvent::StartupTimedOut {
                launch: running.launch,
            }),
            // Failures before the first success are a child still starting, not an unhealthy one.
            HealthOutcome::Unhealthy {
                consecutive_failures,
            } if running.ready => Some(SupervisorEvent::Unhealthy {
                launch: running.launch,
                consecutive_failures,
            }),
            _ => None,
        };
        match failure {
            None => self.phase = Phase::Running(running),
            Some(event) => {
                self.emit(event);
                self.stop_child(running.child);
                let outcome = self.tracker.exited(running.generation, self.at());
                self.apply(outcome);
            }
        }
    }

    /// Stdin closed (the polite cue), then the whole tree is killed after the grace.
    fn stop_child(&self, mut child: PrivateChild) {
        child.close_stdin();
        let _ = child.child_mut().wait_then_kill(self.cfg.stop_grace);
    }

    fn restart_now(&mut self) {
        let placeholder = Phase::Idle(SupervisorState::Stopped);
        match std::mem::replace(&mut self.phase, placeholder) {
            Phase::Running(running) => {
                self.stop_child(running.child);
                let at = self.at();
                let mut outcome = self.tracker.exited(running.generation, at);
                if outcome == RestartOutcome::AwaitingDemand {
                    // This is the demand.
                    outcome = self.tracker.request_restart(at);
                }
                self.apply(outcome);
            }
            // A start is already on its way.
            waiting @ Phase::Waiting(..) => self.phase = waiting,
            Phase::Idle(SupervisorState::AwaitingDemand) => {
                match self.tracker.request_restart(self.at()) {
                    // Nothing ever ran, so the tracker was not waiting: start directly.
                    RestartOutcome::AwaitingDemand => self.start(),
                    other => self.apply(other),
                }
            }
            Phase::Idle(_) => {
                self.tracker.reset_budget();
                self.start();
            }
        }
    }

    fn shutdown(&mut self) {
        self.tracker.shutdown();
        if let Phase::Running(running) =
            std::mem::replace(&mut self.phase, Phase::Idle(SupervisorState::Stopped))
        {
            self.stop_child(running.child);
        }
        self.set_status(SupervisorState::Stopped, None);
        self.emit(SupervisorEvent::Stopped);
    }
}
