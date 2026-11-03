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
    AdoptedIdentity, Generation, HealthOutcome, HealthPolicy, HealthProbe, HealthTracker,
    RestartOutcome, RestartPermit, RestartPolicy, RestartTracker,
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
    Started {
        launch: u64,
        pid: u32,
    },
    /// The child answered its first probe.
    Ready {
        launch: u64,
    },
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
    StartupTimedOut {
        launch: u64,
    },
    /// The launch closure failed (never started a child).
    LaunchFailed {
        error: String,
    },
    RestartScheduled {
        delay: Duration,
    },
    AwaitingDemand,
    /// The restart budget is spent.
    Exhausted,
    /// The supervisor will not start another child (a policy that never restarts, or `stop`).
    Stopped,
    /// A stop was not confirmed (the child is still there after its grace and the kill): no
    /// replacement starts until it is gone (rightkit#936 b2).
    StopUnconfirmed {
        launch: u64,
        pid: u32,
    },
    /// The child exited on purpose after forwarding; the instance with `pid` is supervised by
    /// identity now ([`ExitAction::AdoptForeign`]).
    Adopted {
        launch: u64,
        pid: u32,
    },
    /// The adopted foreign instance is gone.
    ForeignExited {
        launch: u64,
        pid: u32,
    },
}

/// Callback for [`SupervisorEvent`]s. Runs on the supervisor thread; keep it short.
pub type SupervisorEventFn = Arc<dyn Fn(&SupervisorEvent) + Send + Sync>;

/// The app's lock state, as input to a restart (the lock mirror itself stays the app's: a
/// replacement core starts locked exactly when the app says the lock blocked its data).
/// [`Unknown`](Self::Unknown) is the state of an unlock (or lock) request that got no answer
/// because the child died first: it starts the replacement locked, so an unanswered unlock
/// can never restart a child unlocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockState {
    Locked,
    Unlocked,
    Unknown,
}

impl LockState {
    /// The `locked` start flag a launch gets for this state. Only a positively answered
    /// [`Unlocked`](Self::Unlocked) starts a replacement unlocked.
    pub fn starts_locked(self) -> bool {
        !matches!(self, LockState::Unlocked)
    }
}

/// Supplies the current [`LockState`] at each launch. Runs on the supervisor thread.
pub type LockStateFn = Arc<dyn Fn() -> LockState + Send + Sync>;

/// How a child that exited by itself ended (what [`SupervisorHooks::exit_action`] is asked about
/// and what [`SupervisorHandle::last_exit`] reports).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitInfo {
    /// The 1-based launch count of the child that exited.
    pub launch: u64,
    /// Its exit code, when it had one (a signal gives none on Unix).
    pub code: Option<i32>,
    pub success: bool,
}

/// What the supervisor does after a child exited by itself ([`SupervisorHooks::exit_action`]).
#[derive(Debug)]
#[non_exhaustive]
pub enum ExitAction {
    /// The restart policy decides (the default).
    Default,
    /// This exit is final: nothing restarts and the supervisor ends in
    /// [`SupervisorState::Stopped`]. The supervising process reads the code from
    /// [`SupervisorHandle::final_exit`] and exits with it, so launchd and the notches see the
    /// child's own disposition (0, 70, 75, ...) (rightkit#936 b20).
    Final,
    /// The child exited on purpose after forwarding to an instance that already runs (a second
    /// launch of a single-instance app). Supervise that instance by identity instead: liveness
    /// only (no probe, it is not this supervisor's child), never signalled. When it is gone the
    /// restart policy decides (rightkit#936 b2).
    AdoptForeign(AdoptedIdentity),
}

/// Asked before each launch ([`SupervisorHooks::start_gate`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartGateInfo {
    /// The 1-based launch count the next child would have.
    pub next_launch: u64,
}

/// Whether the next launch may happen now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartGate {
    /// Launch now.
    Proceed,
    /// Not yet (another supervisor's child is still finishing a handover): ask again after this
    /// long. No restart budget is charged for waiting.
    Wait(Duration),
    /// Never launch: the supervisor ends in [`SupervisorState::Stopped`].
    Stop,
}

type ExitActionHook = Box<dyn FnMut(&ExitInfo) -> ExitAction + Send>;
type StartGateHook = Box<dyn FnMut(&StartGateInfo) -> StartGate + Send>;

/// Optional decisions the supervisor asks its host (rightkit#936). Every hook runs on the
/// supervisor thread; keep each short.
#[derive(Default)]
pub struct SupervisorHooks {
    /// Called when a child exits by itself.
    pub exit_action: Option<ExitActionHook>,
    /// Called before every launch, the first included (a start precondition: wait for a handover).
    pub start_gate: Option<StartGateHook>,
}

enum Cmd {
    Stop,
    RestartNow(Option<LockState>),
    ReturnChild(mpsc::Sender<Option<PrivateChild>>),
}

/// Control of a running supervisor. Dropping it stops the supervisor and its child.
pub struct SupervisorHandle {
    tx: mpsc::Sender<Cmd>,
    status: Arc<Mutex<SupervisorStatus>>,
    exits: Arc<Mutex<ExitRecord>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[derive(Default)]
struct ExitRecord {
    last: Option<ExitInfo>,
    final_exit: Option<ExitInfo>,
}

impl SupervisorHandle {
    /// The most recent self-exit of a child, if any child has exited by itself.
    pub fn last_exit(&self) -> Option<ExitInfo> {
        self.exits.lock().unwrap_or_else(|e| e.into_inner()).last
    }

    /// The exit a [`SupervisorHooks::exit_action`] declared [`ExitAction::Final`], if one did. The
    /// supervising process exits with its `code`.
    pub fn final_exit(&self) -> Option<ExitInfo> {
        self.exits
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .final_exit
    }

    /// The state now.
    pub fn status(&self) -> SupervisorStatus {
        *self.status.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Ask for a start: a running child is stopped and counted as an exit (then the policy
    /// decides, so a policy that waits for demand starts at once); a child waiting for demand
    /// starts; an exhausted or stopped policy forgets its budget and starts. A pending backoff is
    /// left to run. Returns before the work is done.
    pub fn restart_now(&self) {
        let _ = self.tx.send(Cmd::RestartNow(None));
    }

    /// [`restart_now`](Self::restart_now) with the lock state the next launch starts from,
    /// overriding the [`LockStateFn`] for that one launch. [`LockState::Unknown`] (an unlock
    /// that was never answered) starts the replacement locked.
    pub fn restart_now_with_lock(&self, state: LockState) {
        let _ = self.tx.send(Cmd::RestartNow(Some(state)));
    }

    /// Stop the child (stdin closed, then killed after the grace) and end the supervisor. Waits
    /// for the thread to finish.
    pub fn stop(mut self) {
        self.stop_inner();
    }

    /// End the supervisor without stopping the child, handing the running child back once
    /// (`None` when no child is running) so the caller can shut it down in its own order.
    /// No restart follows: the supervisor thread has ended when this returns. Consumes the
    /// handle, so the child can be taken only once.
    pub fn stop_returning_child(mut self) -> Option<PrivateChild> {
        let (tx, rx) = mpsc::channel();
        if self.tx.send(Cmd::ReturnChild(tx)).is_err() {
            return None;
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        rx.recv().ok().flatten()
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
    mut launch: L,
    probe: P,
    on_event: Option<SupervisorEventFn>,
) -> io::Result<SupervisorHandle>
where
    L: FnMut() -> io::Result<PrivateChild> + Send + 'static,
    P: FnMut(&PrivateChild) -> bool + Send + 'static,
{
    spawn_core(
        config,
        move |_locked| launch(),
        probe,
        None,
        None,
        SupervisorHooks::default(),
        on_event,
    )
}

/// [`supervise_with_lock_state`] with [`SupervisorHooks`] (rightkit#936): a start gate before each
/// launch, an exit disposition, a hold-off while a stop is unconfirmed (always on) and supervision
/// of a foreign instance by identity.
pub fn supervise_with_hooks<L, P>(
    config: SupervisorConfig,
    launch: L,
    probe: P,
    lock_state: Option<LockStateFn>,
    hooks: SupervisorHooks,
    on_event: Option<SupervisorEventFn>,
) -> io::Result<SupervisorHandle>
where
    L: FnMut(bool) -> io::Result<PrivateChild> + Send + 'static,
    P: FnMut(&PrivateChild) -> bool + Send + 'static,
{
    spawn_core(config, launch, probe, None, lock_state, hooks, on_event)
}

/// [`supervise`] for a child whose replacement must start from the app's lock state: every
/// launch receives `locked`, resolved at launch time from a
/// [`restart_now_with_lock`](SupervisorHandle::restart_now_with_lock) override first, then
/// `lock_state`, then `false`. [`LockState::Unknown`] resolves to locked, so an unlock the
/// dying child never answered cannot restart it unlocked. The lock mirror that produces
/// the state stays the app's input.
pub fn supervise_with_lock_state<L, P>(
    config: SupervisorConfig,
    launch: L,
    probe: P,
    lock_state: Option<LockStateFn>,
    on_event: Option<SupervisorEventFn>,
) -> io::Result<SupervisorHandle>
where
    L: FnMut(bool) -> io::Result<PrivateChild> + Send + 'static,
    P: FnMut(&PrivateChild) -> bool + Send + 'static,
{
    spawn_core(
        config,
        launch,
        probe,
        None,
        lock_state,
        SupervisorHooks::default(),
        on_event,
    )
}

/// Supervise a child the caller already spawned (and usually already verified: it counts
/// as launch 1, already ready, so its first crash restarts from the base backoff). The
/// child is transferred untouched — its stdin stays open and it is not probed before the
/// first poll. Replacements come from `launch`, with the lock flag resolved as in
/// [`supervise_with_lock_state`].
pub fn supervise_adopted<L, P>(
    config: SupervisorConfig,
    child: PrivateChild,
    launch: L,
    probe: P,
    lock_state: Option<LockStateFn>,
    on_event: Option<SupervisorEventFn>,
) -> io::Result<SupervisorHandle>
where
    L: FnMut(bool) -> io::Result<PrivateChild> + Send + 'static,
    P: FnMut(&PrivateChild) -> bool + Send + 'static,
{
    spawn_core(
        config,
        launch,
        probe,
        Some(child),
        lock_state,
        SupervisorHooks::default(),
        on_event,
    )
}

fn spawn_core<L, P>(
    config: SupervisorConfig,
    launch: L,
    probe: P,
    adopted: Option<PrivateChild>,
    lock_state: Option<LockStateFn>,
    hooks: SupervisorHooks,
    on_event: Option<SupervisorEventFn>,
) -> io::Result<SupervisorHandle>
where
    L: FnMut(bool) -> io::Result<PrivateChild> + Send + 'static,
    P: FnMut(&PrivateChild) -> bool + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let exits = Arc::new(Mutex::new(ExitRecord::default()));
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
        adopted,
        lock_state,
        pending_locked: None,
        hooks,
        exits: exits.clone(),
    };
    let thread = std::thread::Builder::new()
        .name("rk-supervisor".into())
        .spawn(move || core.run(rx))?;
    Ok(SupervisorHandle {
        tx,
        status,
        exits,
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
    Running(Box<Running>),
    /// The next start is waiting for `due`.
    Waiting(RestartPermit, Instant),
    /// Nothing runs: `AwaitingDemand`, `Exhausted` or `Stopped`.
    Idle(SupervisorState),
    /// The start gate said wait; ask again at the instant.
    Gated(Instant),
    /// A stop was not confirmed: the child is held (and stopped again each tick) and nothing
    /// replaces it until it is gone. The flag is an explicit restart demand to honour then.
    Unconfirmed(Box<Running>, bool),
    /// An instance this supervisor did not spawn, watched by identity.
    Foreign(Box<ForeignRun>),
}

struct ForeignRun {
    identity: AdoptedIdentity,
    generation: Generation,
    launch: u64,
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
    adopted: Option<PrivateChild>,
    lock_state: Option<LockStateFn>,
    /// One-launch lock override from `restart_now_with_lock`.
    pending_locked: Option<bool>,
    hooks: SupervisorHooks,
    exits: Arc<Mutex<ExitRecord>>,
}

impl<L, P> Core<L, P>
where
    L: FnMut(bool) -> io::Result<PrivateChild>,
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
        match self.adopted.take() {
            Some(child) => self.adopt(child),
            None => self.start(),
        }
        loop {
            let wait = match &self.phase {
                Phase::Waiting(_, due) | Phase::Gated(due) => due
                    .saturating_duration_since(Instant::now())
                    .min(self.cfg.poll_interval),
                _ => self.cfg.poll_interval,
            };
            match rx.recv_timeout(wait) {
                Ok(Cmd::RestartNow(state)) => self.restart_now(state),
                Ok(Cmd::ReturnChild(reply)) => {
                    self.return_child(reply);
                    return;
                }
                Ok(Cmd::Stop) | Err(RecvTimeoutError::Disconnected) => {
                    self.shutdown();
                    return;
                }
                Err(RecvTimeoutError::Timeout) => self.tick(),
            }
        }
    }

    /// Take over an already-spawned child as launch 1, already ready (the caller spawned
    /// and verified it). The child itself is not touched: no stdin close, no probe yet.
    fn adopt(&mut self, child: PrivateChild) {
        let at = self.at();
        let generation = self.tracker.started(at);
        self.tracker.ready(generation);
        self.launches += 1;
        let pid = child.child().id();
        let mut health = HealthTracker::new(self.cfg.health, at);
        let _ = health.observe(at, HealthProbe::Success);
        self.set_status(SupervisorState::Ready, Some(pid));
        self.emit(SupervisorEvent::Started {
            launch: self.launches,
            pid,
        });
        self.emit(SupervisorEvent::Ready {
            launch: self.launches,
        });
        self.phase = Phase::Running(Box::new(Running {
            child,
            generation,
            health,
            ready: true,
            launch: self.launches,
        }));
    }

    /// The `locked` flag for the next launch: explicit restart override, then the app's
    /// lock-state input, then unlocked. `Unknown` resolves locked (see [`LockState`]).
    fn next_locked(&mut self) -> bool {
        if let Some(locked) = self.pending_locked.take() {
            return locked;
        }
        match &self.lock_state {
            Some(f) => f().starts_locked(),
            None => false,
        }
    }

    /// Launch a child now. A launch failure is charged to the budget like a crash.
    fn start(&mut self) {
        if let Some(gate) = self.hooks.start_gate.as_mut() {
            match gate(&StartGateInfo {
                next_launch: self.launches + 1,
            }) {
                StartGate::Proceed => {}
                StartGate::Wait(wait) => {
                    self.set_status(SupervisorState::WaitingRestart, None);
                    self.phase = Phase::Gated(Instant::now() + wait.max(MIN_POLL));
                    return;
                }
                StartGate::Stop => {
                    self.tracker.shutdown();
                    self.emit(SupervisorEvent::Stopped);
                    self.set_status(SupervisorState::Stopped, None);
                    self.phase = Phase::Idle(SupervisorState::Stopped);
                    return;
                }
            }
        }
        let locked = self.next_locked();
        match (self.launch)(locked) {
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
                self.phase = Phase::Running(Box::new(Running {
                    child,
                    generation,
                    health: HealthTracker::new(self.cfg.health, at),
                    ready: false,
                    launch: self.launches,
                }));
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
            Phase::Running(running) => self.tick_running(*running),
            Phase::Waiting(permit, due) => {
                if Instant::now() >= due && self.tracker.permit_valid(&permit, self.at()) {
                    self.start();
                } else {
                    self.phase = Phase::Waiting(permit, due);
                }
            }
            Phase::Gated(due) => {
                if Instant::now() >= due {
                    self.start();
                } else {
                    self.phase = Phase::Gated(due);
                }
            }
            Phase::Unconfirmed(running, demand) => self.tick_unconfirmed(*running, demand),
            Phase::Foreign(foreign) => self.tick_foreign(*foreign),
            idle @ Phase::Idle(_) => self.phase = idle,
        }
    }

    /// The held child again: stop it once more; once it is gone the replacement may follow.
    fn tick_unconfirmed(&mut self, mut running: Running, demand: bool) {
        let gone = matches!(running.child.child_mut().try_wait(), Ok(Some(_)))
            || self.stop_child(&mut running.child);
        if gone {
            self.settle(running, demand);
        } else {
            self.phase = Phase::Unconfirmed(Box::new(running), demand);
        }
    }

    /// An adopted foreign instance: liveness by identity only. A read that fails is not taken
    /// for an exit.
    fn tick_foreign(&mut self, foreign: ForeignRun) {
        if matches!(foreign.identity.is_running(), Ok(true) | Err(_)) {
            self.phase = Phase::Foreign(Box::new(foreign));
            return;
        }
        self.emit(SupervisorEvent::ForeignExited {
            launch: foreign.launch,
            pid: foreign.identity.pid(),
        });
        let outcome = self.tracker.exited(foreign.generation, self.at());
        self.apply(outcome);
    }

    /// The child is confirmed gone: tell the restart tracker and act on its answer. `demand`
    /// turns a wait-for-demand answer into a start (an explicit restart).
    fn settle(&mut self, running: Running, demand: bool) {
        drop(running.child);
        let at = self.at();
        let mut outcome = self.tracker.exited(running.generation, at);
        if demand && outcome == RestartOutcome::AwaitingDemand {
            outcome = self.tracker.request_restart(at);
        }
        self.apply(outcome);
    }

    /// Stop the child and settle, or hold it when the stop is unconfirmed.
    fn stop_and_settle(&mut self, mut running: Running, demand: bool) {
        if self.stop_child(&mut running.child) {
            self.settle(running, demand);
        } else {
            let pid = running.child.child().id();
            self.emit(SupervisorEvent::StopUnconfirmed {
                launch: running.launch,
                pid,
            });
            self.set_status(SupervisorState::WaitingRestart, Some(pid));
            self.phase = Phase::Unconfirmed(Box::new(running), demand);
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
                let info = ExitInfo {
                    launch: running.launch,
                    code: status.code(),
                    success: status.success(),
                };
                self.exits.lock().unwrap_or_else(|e| e.into_inner()).last = Some(info);
                let action = match self.hooks.exit_action.as_mut() {
                    Some(decide) => decide(&info),
                    None => ExitAction::Default,
                };
                // The child has been reaped by `try_wait`; dropping the wrapper closes its stdin.
                drop(running.child);
                match action {
                    ExitAction::Final => {
                        self.exits
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .final_exit = Some(info);
                        self.tracker.shutdown();
                        self.emit(SupervisorEvent::Stopped);
                        self.set_status(SupervisorState::Stopped, None);
                        self.phase = Phase::Idle(SupervisorState::Stopped);
                    }
                    ExitAction::AdoptForeign(identity) => {
                        let pid = identity.pid();
                        self.emit(SupervisorEvent::Adopted {
                            launch: running.launch,
                            pid,
                        });
                        self.set_status(SupervisorState::Ready, Some(pid));
                        self.phase = Phase::Foreign(Box::new(ForeignRun {
                            identity,
                            generation: running.generation,
                            launch: running.launch,
                        }));
                    }
                    _ => {
                        let outcome = self.tracker.exited(running.generation, self.at());
                        self.apply(outcome);
                    }
                }
                return;
            }
            Ok(None) => {}
            Err(_) => {
                // The state of the child cannot be read: do not guess it is fine.
                self.stop_and_settle(running, false);
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
            None => self.phase = Phase::Running(Box::new(running)),
            Some(event) => {
                self.emit(event);
                self.stop_and_settle(running, false);
            }
        }
    }

    /// Stdin closed (the polite cue), then the whole tree is killed after the grace. True when the
    /// child is confirmed gone; false when the stop could not be confirmed.
    fn stop_child(&self, child: &mut PrivateChild) -> bool {
        child.close_stdin();
        match child.child_mut().wait_then_kill(self.cfg.stop_grace) {
            Ok(_) => true,
            Err(_) => matches!(child.child_mut().try_wait(), Ok(Some(_))),
        }
    }

    fn restart_now(&mut self, state: Option<LockState>) {
        if let Some(state) = state {
            self.pending_locked = Some(state.starts_locked());
        }
        let placeholder = Phase::Idle(SupervisorState::Stopped);
        match std::mem::replace(&mut self.phase, placeholder) {
            // This is the demand: a wait-for-demand answer becomes a start.
            Phase::Running(running) => self.stop_and_settle(*running, true),
            Phase::Unconfirmed(running, _) => self.phase = Phase::Unconfirmed(running, true),
            // A start is already on its way, or an instance this supervisor never signals runs.
            held @ (Phase::Waiting(..) | Phase::Gated(_) | Phase::Foreign(_)) => self.phase = held,
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

    /// Hand the running child back untouched and end supervision: no stdin close, no
    /// kill, no restart. The caller owns the child's shutdown from here.
    fn return_child(&mut self, reply: mpsc::Sender<Option<PrivateChild>>) {
        self.tracker.shutdown();
        let child = match std::mem::replace(&mut self.phase, Phase::Idle(SupervisorState::Stopped))
        {
            Phase::Running(running) => Some(running.child),
            _ => None,
        };
        self.set_status(SupervisorState::Stopped, None);
        self.emit(SupervisorEvent::Stopped);
        let _ = reply.send(child);
    }

    fn shutdown(&mut self) {
        self.tracker.shutdown();
        match std::mem::replace(&mut self.phase, Phase::Idle(SupervisorState::Stopped)) {
            Phase::Running(mut running) | Phase::Unconfirmed(mut running, _) => {
                self.stop_child(&mut running.child);
            }
            _ => {}
        }
        self.set_status(SupervisorState::Stopped, None);
        self.emit(SupervisorEvent::Stopped);
    }
}
