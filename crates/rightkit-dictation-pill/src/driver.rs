//! The native pill's owner thread and its lifecycle.
//!
//! One thread (named by `Config::thread_name`) owns the three overlays (pill, hint, clip), the
//! visual state the painters read, and every timer. It blocks in `recv()` with no timeout
//! whenever nothing is scheduled, so an idle pill costs no wakeups; frames are pumped only while
//! something animates (recording bars, a size morph, a fade, the launch bloom).
//!
//! Lifecycle (see `lifecycle.rs` for the pure rules & `retire.rs` for retained threads):
//!
//! * Each start is a generation with its own [`Startup`]. The owner thread builds the shared
//!   state (so no host code runs on the caller's thread), checks the generation's cancellation
//!   before it creates a window and after each window and each forwarder thread, and must win
//!   [`Startup::mark_ready`] before it publishes them. A caller that times out, or a `shutdown`
//!   during startup, cancels the generation and the owner destroys what it made and exits.
//! * At most one owner thread is alive at a time. Every owner has an [`ExitSignal`]; the latest
//!   one is kept in `LAST_EXIT`, and a new start, and `shutdown`, wait for it (bounded) before
//!   they report anything, so a second generation never runs while the first still owns the
//!   same window names. The owner closes each window with
//!   `OverlayWindow::close_into_join_handle` (the close is posted at once, nothing is joined on
//!   the owner) and keeps the returned `JoinHandle`, and the starter keeps the owner thread's own
//!   handle, in `RETIRE` (process-wide, per generation; see `retire.rs`) until that thread has
//!   finished; a window thread's last steps destroy the window (`hwnd` becomes 0) and deliver
//!   `Closed`, so a finished window thread is a closed window. Handles are only joined once
//!   `is_finished()` says so, by the admission check and `shutdown`, so nothing waits on a
//!   wedged thread past its bound. Every window also has one event forwarder and every host
//!   callback a worker, both counted in `RETIRE`. A new generation is admitted only when the
//!   previous owner has exited AND every retained thread has finished AND every forwarder and
//!   worker of any generation has retired; otherwise `ensure_installed` returns
//!   [`Install::RetirementPending`] (retry later). `shutdown` returns `true` only after the owner
//!   finished tearing down AND that same condition holds within its bound.
//! * A running generation ends through [`shutdown`] (the owner destroys the overlays and exits,
//!   bounded) or on its own when the owner panics, a painter faults or an overlay window ends by
//!   itself (`overlay_closed`): the overlays are destroyed and the generation is marked dead,
//!   including when that happens between the owner publishing the overlays and the starter
//!   installing them.
//! * The owner reads one bounded command queue (`cmd_queue`): host commands beyond its capacity
//!   are dropped (one `call_dropped reason=queue_full` per burst) and reported to the caller;
//!   shutdown, a painter fault and a frame pump going idle travel on flags that are never dropped.
//! * After `shutdown` or a failed start only an explicit [`ensure_installed`] starts a new
//!   generation. Every other call is implicit: it neither starts a generation nor waits for one;
//!   it enqueues to a running owner or is dropped with one report per generation. A UI thread
//!   therefore never blocks in this crate except in `ensure_installed` and `shutdown`.
//! * Everything a generation publishes (shown flags, rectangles) lives in that generation's
//!   `Shared`, so a late-exiting old owner can never overwrite a newer generation's state.
//!
//! Lock discipline: the shared visual state is only locked for short reads and writes. No overlay
//! handle method and no look method (`pill`, `hint`, `clip`) is called while it is held, because
//! a painter running on the overlay thread takes the same lock; the one exception is
//! `Painters::capsule_size`, which the model asks under the lock and which must be pure.
//!
//! Host work never reports success before the host does: a clip copy, a `HostFlash` action and a
//! `Host` action with `close_card_after` run on a worker thread, carry a token and a deadline
//! (`pending::Tracker`), and the card, check mark or close happens only on the matching result.

use std::cell::Cell;
use std::collections::HashSet;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rightkit_dictation_core::pill::PillEvent;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_MONITOR,
};

use crate::anim::{AuxPose, Mode};
use crate::clipboard::{self, ClipItem, ClipRefusal, ClipState};
use crate::clock::{after, after_duration};
use crate::cmd_queue::{CmdQueue, Control, Next, Push};
use crate::config::{self, bound, metrics, motion, ActionOutcome, CardOrder};
use crate::draw::VRect;
use crate::gfx;
use crate::hint::{self, HintContent, HintState};
use crate::lifecycle::{
    panic_type, Begin, CancelOutcome, Finish, Lifecycle, Phase, ShutdownTake, Startup,
};
use crate::look::{AuxFrame, ClipView, HintView, Pass};
use crate::pending::{Resolution, Tracker};
use crate::pill::{painter::PillPainter, Effect, PillModel};
use crate::regions::{Action, Activation, Cursor, HotSet, HoverRole, Region, RegionId};
use crate::retire::{Retire, Verdict};

/// The crate's own region ids: the area the capsule paints at while it morphs
/// (`RegionId::VISUAL`; the overlay window is shaped to the union of its hit regions, so without
/// it a shrinking or entering capsule would be clipped to its settled rectangle; it never counts
/// as hover and is not clickable) and the capsule body (`RegionId::CAPSULE`).
pub(crate) use crate::regions::Surface as Surf;

pub use crate::visibility::Visibility;

pub(crate) struct Visual {
    pub pill: PillModel,
    pub hint: Option<HintState>,
    pub clip: Option<ClipState>,
    /// Monitor scale (`dpi / 96`) the painters multiply points by.
    pub scale: f32,
}

/// What one generation publishes for the host's getters.
#[derive(Default)]
pub(crate) struct Published {
    pub pill_shown: AtomicBool,
    pub hint_shown: AtomicBool,
    pub clip_shown: AtomicBool,
    pub monitor: Mutex<Option<[f64; 4]>>,
    pub capsule: Mutex<Option<[f64; 4]>>,
    /// Last frame handed to each overlay: `[x, y, width, height, scale]`, x/y physical px, size
    /// logical.
    pub frames: Mutex<[Option<[f64; 5]>; 3]>,
}

impl Published {
    fn clear(&self) {
        self.pill_shown.store(false, Ordering::Relaxed);
        self.hint_shown.store(false, Ordering::Relaxed);
        self.clip_shown.store(false, Ordering::Relaxed);
        *lock(&self.monitor) = None;
        *lock(&self.capsule) = None;
        *lock(&self.frames) = [None; 3];
    }
}

pub(crate) struct Shared {
    pub visual: Mutex<Visual>,
    /// Latest microphone level (`f32` bits); read by the pill painter every frame.
    pub level_bits: AtomicU32,
    /// A frame pump is running for this surface.
    pub pump: [AtomicBool; 3],
    pub published: Published,
    /// `pill_cursor_inert` was reported for this generation.
    cursor_inert_reported: AtomicBool,
}

impl Shared {
    /// Builds the model, which asks the host's look for the resting capsule size. Runs on the
    /// owner thread only, so a slow or panicking look cannot stall or unwind a caller.
    fn new(now: Instant) -> Shared {
        Shared {
            visual: Mutex::new(Visual {
                pill: PillModel::new(now),
                hint: None,
                clip: None,
                scale: 1.0,
            }),
            level_bits: AtomicU32::new(0f32.to_bits()),
            pump: [
                AtomicBool::new(false),
                AtomicBool::new(false),
                AtomicBool::new(false),
            ],
            published: Published::default(),
            cursor_inert_reported: AtomicBool::new(false),
        }
    }
}

pub(crate) enum Cmd {
    Apply {
        event: PillEvent,
        vis: Visibility,
    },
    SetVisibility(Visibility),
    Reveal {
        intro: bool,
    },
    Overlay(Surf, gfx::OverlayEvent),
    ShowHint {
        content: HintContent,
    },
    DismissHint,
    ShowClip {
        items: Vec<ClipItem>,
        single: bool,
    },
    DismissClip,
    /// A clip copy finished on its worker thread (`ok` only after the host confirmed it).
    CopyDone {
        token: u64,
        ok: bool,
    },
    /// The off-thread host action of a `HostFlash` region finished.
    FlashDone {
        token: u64,
        outcome: ActionOutcome,
    },
    /// The off-thread host action of a `Host` region with `close_card_after` finished.
    ActionDone {
        token: u64,
        outcome: ActionOutcome,
    },
    Reassert,
    Reposition,
}

/// The sending side of the owner's bounded command queue (see `cmd_queue`: shutdown, a painter
/// fault and a frame pump going idle are flags on it that are never dropped).
pub(crate) type CmdTx = Arc<CmdQueue<Cmd>>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// The owner thread's end, as the starter and `shutdown` see it.
///
/// `signal` runs exactly once, last, after the owner asked every window it made to close
/// (`close_into_join_handle`) and handed each window thread's `JoinHandle` to [`RETIRE`]. It proves
/// neither that a window thread nor the owner thread itself has finished: [`RETIRE`] holds those
/// handles and says when they have.
#[derive(Default)]
struct ExitSignal {
    done: Mutex<bool>,
    changed: Condvar,
    /// Set before the generation is marked dead (a fault, a panic, a window that ended by itself),
    /// so a starter that installs its controller afterwards can still see it (`start`).
    died: AtomicBool,
}

/// What [`ensure_installed`] found. Only `Running` means a generation is up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Install {
    /// A generation is running (it was, or this call started it).
    Running,
    /// No generation was started: nothing installed, the call came from the owner thread, the
    /// overlays could not be created in `Timings::startup_timeout_ms`, or the previous owner is
    /// still alive. The reason is reported as `install_error` or `call_refused`.
    Failed,
    /// An earlier generation's owner thread, window threads, event forwarders or workers have not
    /// all finished within `Timings::shutdown_timeout_ms` (a window thread that is wedged, or a
    /// host callback that has not returned). Nothing was created. Retry on the next dictation
    /// start; it succeeds once they finished.
    RetirementPending,
}

impl Install {
    /// Whether a generation is up.
    pub fn is_running(self) -> bool {
        self == Install::Running
    }

    /// A stable tag for the host's log: `running`, `failed` or `retirement_pending`.
    pub fn code(self) -> &'static str {
        match self {
            Install::Running => "running",
            Install::Failed => "failed",
            Install::RetirementPending => "retirement_pending",
        }
    }
}

impl ExitSignal {
    fn signal(&self) {
        *lock(&self.done) = true;
        self.changed.notify_all();
    }

    /// Wait up to `bound` for the owner to finish; whether it did.
    fn wait(&self, bound: Duration) -> bool {
        let guard = lock(&self.done);
        let (guard, _) = self
            .changed
            .wait_timeout_while(guard, bound, |done| !*done)
            .unwrap_or_else(|e| e.into_inner());
        *guard
    }
}

pub(crate) struct Controller {
    generation: u64,
    tx: CmdTx,
    shared: Arc<Shared>,
    handles: [gfx::Handle; 3],
    exit: Arc<ExitSignal>,
}

static LIFE: Mutex<Lifecycle<Arc<Controller>>> = Mutex::new(Lifecycle::new());
/// Signalled whenever `LIFE` leaves `Starting` or `Cancelling`.
static CHANGED: Condvar = Condvar::new();
/// The exit signal of the most recently spawned owner thread, whatever became of its start.
static LAST_EXIT: Mutex<Option<Arc<ExitSignal>>> = Mutex::new(None);
/// A call was dropped because no generation is running, and it was reported (once per generation).
static DROP_REPORTED: AtomicBool = AtomicBool::new(false);
/// Worker threads running host callbacks right now, process-wide: a worker that outlives its
/// generation still holds its slot, so a new generation cannot exceed `Limits::max_worker_threads`
/// on top of it.
static WORKERS: AtomicUsize = AtomicUsize::new(0);
/// Retirement record, process-wide: the retained owner and window thread handles (until finished),
/// the event forwarders alive per generation and the workers running. Admission of a new generation
/// waits for it to empty (`retire.rs`).
static RETIRE: Retire = Retire::new();

thread_local! {
    /// Set on the owner thread: host callbacks that run there must not call [`shutdown`] or
    /// [`ensure_installed`], which would wait for the very thread they run on.
    static ON_OWNER: Cell<bool> = const { Cell::new(false) };
}

fn on_owner_thread() -> bool {
    ON_OWNER.with(Cell::get)
}

fn running() -> Option<Arc<Controller>> {
    lock(&LIFE).running()
}

/// Wait up to `bound` for the latest owner thread (if any) to finish. Whether it did.
fn wait_last_owner(bound: Duration) -> bool {
    let exit = lock(&LAST_EXIT).clone();
    match exit {
        Some(exit) => exit.wait(bound),
        None => true,
    }
}

// ---------------------------------------------------------------------------------------------
// Public surface (thread-safe, non-blocking after install)
// ---------------------------------------------------------------------------------------------

/// Create the overlays and start the driver with the installed [`config::Config`]. Idempotent
/// while running; after [`shutdown`] or a failed start it starts a fresh generation (first
/// waiting, bounded by `Timings::shutdown_timeout_ms`, for the previous owner thread to finish:
/// two generations never overlap). `false` if the overlays could not be created in
/// `Timings::startup_timeout_ms` (the owner then destroys anything it made and exits; implicit
/// calls do nothing until the next explicit call), if the previous owner is still alive, or when
/// called from a host callback on the owner thread. This is the only call that starts a
/// generation, and it blocks (bounded): call it off the UI thread or once at startup. Call
/// [`config::install`] first.
///
/// The answer is an [`Install`]. A new generation is admitted only when the previous owner has
/// exited and every retained thread (owner and windows) has finished and every event forwarder and
/// worker of every earlier generation has retired (see `RETIRE`). Waiting for that is part of the same bound (`Timings::shutdown_timeout_ms`,
/// shared with the wait for the previous owner); when it runs out nothing is created and the
/// answer is [`Install::RetirementPending`], so a retry never accumulates threads or windows.
pub fn ensure_installed() -> Install {
    ensure_installed_if(|| true)
}

/// Evaluate admission under LIFE before every begin, including after a concurrent start settles.
/// The predicate must be nonblocking, callback-free, and must not take a supervisor lock.
pub(crate) fn ensure_installed_if(admit: impl Fn() -> bool) -> Install {
    if on_owner_thread() {
        config::report(
            "call_refused",
            "call=ensure_installed reason=on_owner_thread",
        );
        return Install::Failed;
    }
    match explicit_controller(admit) {
        Ok(_) => Install::Running,
        Err(refusal) => refusal,
    }
}

/// Destroy the overlays and stop the owner thread, waiting at most `Timings::shutdown_timeout_ms`
/// in total. Returns `true` only when the latest owner has finished (whether it was running, still
/// starting, or had been given up on by a timed-out start) AND every
/// retained thread (owner and windows) has finished and every forwarder and worker retired within
/// that bound. `false` means one of two things, reported as `shutdown_incomplete`:
/// `owner_still_running` (the owner is still unwinding and closes the overlays when it gets
/// there) or `threads_not_exited` (a window or owner thread, a forwarder or a worker had not
/// finished when the bound ran out, with `threads=`, `forwarders=` and `workers=` counts; a window
/// may still exist, and `ensure_installed` refuses a new generation until they finished). Cancels a
/// start in progress.
/// Safe to call at any time, any number of times. A call from a host callback on the owner thread
/// is refused (`false`): it would wait for itself. After it, only [`ensure_installed`] starts the
/// driver again.
pub fn shutdown() -> bool {
    let Some(config) = config::try_get() else {
        return true;
    };
    if on_owner_thread() {
        config::report("call_refused", "call=shutdown reason=on_owner_thread");
        return false;
    }
    let bound = Duration::from_millis(config.timings.shutdown_timeout_ms);
    let deadline = after_duration(Instant::now(), bound);
    let take = lock(&LIFE).shutdown();
    CHANGED.notify_all();
    let settled = match take {
        ShutdownTake::Running(controller) => teardown(&controller, remaining(deadline)),
        ShutdownTake::CancelledStartup(_) | ShutdownTake::Nothing => {
            wait_until_settled(remaining(deadline))
        }
    };
    // The phase can settle before the owner has finished its teardown (a timed-out start leaves
    // its owner behind, and the starter reports before it destroys a superseded controller): the
    // owner's exit signal says it finished, and the retirement record says whether the owner and
    // every window thread finished (their retained handles) and every forwarder and worker ended.
    let last = lock(&LAST_EXIT).clone();
    let exited = last
        .as_ref()
        .is_none_or(|exit| exit.wait(remaining(deadline)));
    let windows_left = exited && RETIRE.wait_retired(remaining(deadline)) != Verdict::Retired;
    crate::frame::clear();
    DROP_REPORTED.store(false, Ordering::Relaxed);
    if !(settled && exited) {
        config::report("shutdown_incomplete", "reason=owner_still_running");
    } else if windows_left {
        let left = RETIRE.counts();
        config::report(
            "shutdown_incomplete",
            &format!(
                "reason=threads_not_exited threads={} forwarders={} workers={}",
                left.threads, left.forwarders, left.workers
            ),
        );
    }
    settled && exited && !windows_left
}

fn remaining(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

/// Ask a controller's owner to destroy its overlays and wait (bounded) for it.
fn teardown(controller: &Controller, bound: Duration) -> bool {
    controller.tx.post_shutdown();
    controller.exit.wait(bound)
}

/// Wait up to `bound` for a cancelled start to finish unwinding. Whether it did.
fn wait_until_settled(bound: Duration) -> bool {
    let guard = lock(&LIFE);
    let (guard, _) = CHANGED
        .wait_timeout_while(guard, bound, |life| {
            matches!(life.phase(), Phase::Cancelling(_))
        })
        .unwrap_or_else(|e| e.into_inner());
    !matches!(guard.phase(), Phase::Cancelling(_))
}

/// The running controller for an explicit `ensure_installed`: starts one when allowed and waits
/// (bounded) for another caller's start.
fn explicit_controller(admit: impl Fn() -> bool) -> Result<Arc<Controller>, Install> {
    // No look, no pill: nothing starts before `config::install`.
    let Some(config) = config::try_get() else {
        return Err(Install::Failed);
    };
    let wait_bound = Duration::from_millis(config.timings.startup_timeout_ms)
        .saturating_add(Duration::from_millis(config.timings.shutdown_timeout_ms))
        .saturating_add(Duration::from_millis(1500));
    let deadline = after_duration(Instant::now(), wait_bound);
    loop {
        let decision = {
            let mut life = lock(&LIFE);
            if !admit() {
                return Err(Install::Failed);
            }
            life.begin(true)
        };
        match decision {
            Begin::Run(controller) => return Ok(controller),
            Begin::Refuse => return Err(Install::Failed),
            Begin::Start(startup) => return start(&startup),
            Begin::Wait => {
                let left = remaining(deadline);
                if left.is_zero() {
                    return Err(Install::Failed);
                }
                let guard = lock(&LIFE);
                let (guard, _) = CHANGED
                    .wait_timeout_while(guard, left, |life| {
                        matches!(life.phase(), Phase::Starting(_) | Phase::Cancelling(_))
                    })
                    .unwrap_or_else(|e| e.into_inner());
                drop(guard);
            }
        }
    }
}

/// Hand a command to the running owner. Never starts a generation and never waits for one.
/// `true` only when the owner's queue accepted the command. With no running owner the call is
/// dropped and reported once per generation (`call_dropped reason=not_running`); a queue that is
/// full drops it and reports once per burst (`reason=queue_full`); an owner that ended reports
/// `reason=owner_gone`. A caller that must not act as if the command ran checks the answer.
fn send(cmd: Cmd) -> bool {
    send_deferred(cmd).report()
}

/// Queue outcome whose diagnostics must be emitted after the caller releases its own locks.
pub(crate) enum Dispatch {
    Accepted,
    SilentDrop,
    QueueFull,
    NotRunning,
    OwnerGone,
}

impl Dispatch {
    pub(crate) fn report(self) -> bool {
        match self {
            Self::Accepted => true,
            Self::SilentDrop => false,
            Self::QueueFull => {
                config::report("call_dropped", "reason=queue_full");
                false
            }
            Self::NotRunning => {
                report_dropped("not_running");
                false
            }
            Self::OwnerGone => {
                report_dropped("owner_gone");
                false
            }
        }
    }
}

fn send_deferred(cmd: Cmd) -> Dispatch {
    match running() {
        Some(controller) => match controller.tx.push(cmd) {
            Push::Queued => Dispatch::Accepted,
            Push::Dropped {
                first_of_burst: true,
            } => Dispatch::QueueFull,
            Push::Dropped {
                first_of_burst: false,
            } => Dispatch::SilentDrop,
            Push::Closed => Dispatch::OwnerGone,
        },
        None => Dispatch::NotRunning,
    }
}

fn report_dropped(reason: &'static str) {
    if config::try_get().is_some() && !DROP_REPORTED.swap(true, Ordering::Relaxed) {
        config::report("call_dropped", &format!("reason={reason}"));
    }
}

/// The command queue of the owner `launch` spawned, kept where `start` can reach it if `launch`
/// panics: an owner that already published its windows no longer listens to the cancellation, only
/// to its queue.
///
/// It also carries why `launch` refused before it spawned anything (`refusal`: the previous
/// generation's windows have not retired), so the caller gets a typed [`Install`].
#[derive(Default)]
struct LaunchSlot {
    queue: Mutex<Option<CmdTx>>,
    refusal: Mutex<Option<Install>>,
}

/// The starter: launch the owner, wait for it (bounded), report to the lifecycle. `finish` always
/// runs: a panic in `launch` is caught, the attempt is cancelled, the owner (if one exists, ready
/// or not) is told to shut down through its queue and the attempt counts as a failed start, so the
/// phase can never stay `Starting` and no owner is left without a reference.
fn start(startup: &Arc<Startup>) -> Result<Arc<Controller>, Install> {
    let slot = LaunchSlot::default();
    let launched = catch_unwind(AssertUnwindSafe(|| launch(startup, &slot)));
    let built = match launched {
        Ok(built) => built.map(Arc::new),
        Err(payload) => {
            startup.cancel();
            if let Some(queue) = lock(&slot.queue).take() {
                queue.post_shutdown();
            }
            config::report(
                "start_panic",
                &format!("type={}", panic_type(payload.as_ref())),
            );
            None
        }
    };
    let finish = lock(&LIFE).finish(startup, built.clone());
    CHANGED.notify_all();
    if matches!(finish, Finish::Installed) {
        DROP_REPORTED.store(false, Ordering::Relaxed);
    }
    let refusal = lock(&slot.refusal).take().unwrap_or(Install::Failed);
    match (finish, built) {
        (Finish::Installed, Some(controller)) => {
            // The owner can die between publishing its windows and here (a painter fault right
            // after `mark_ready`): its own `mark_dead` found the phase still `Starting` and did
            // nothing. `died` is set before that call, so it is visible now.
            if controller.exit.died.load(Ordering::SeqCst) {
                let marked = lock(&LIFE).mark_dead(controller.generation);
                CHANGED.notify_all();
                if marked {
                    config::report(
                        "driver_stopped",
                        &format!(
                            "generation={} overlays=destroyed restart=explicit",
                            controller.generation
                        ),
                    );
                }
                return Err(Install::Failed);
            }
            Ok(controller)
        }
        (Finish::Installed, None) => Err(refusal),
        (Finish::Superseded, Some(controller)) => {
            // `shutdown` overtook the start: the controller was never published, so destroy it.
            let bound = Duration::from_millis(config::get().timings.shutdown_timeout_ms);
            teardown(&controller, bound);
            Err(Install::Failed)
        }
        _ => Err(refusal),
    }
}

/// What the owner reports once the overlays exist: the shared state and the three handles.
type Ready = Result<(Arc<Shared>, [gfx::Handle; 3]), &'static str>;

/// Spawn the owner thread and wait (bounded) for it. Everything that can fail on the caller's
/// side happens before the spawn; the owner's queue is recorded in `slot` before the spawn, so
/// `start` can stop an owner that exists whatever happens afterwards.
fn launch(startup: &Arc<Startup>, slot: &LaunchSlot) -> Option<Controller> {
    let config = config::get();
    let startup_bound = Duration::from_millis(config.timings.startup_timeout_ms);
    let shutdown_bound = Duration::from_millis(config.timings.shutdown_timeout_ms);
    // One generation at a time: the previous owner must have exited and every retained thread
    // (owners and windows), forwarder and worker of any earlier generation must have finished
    // before the same window names are created again. One bound covers both waits, so a refused
    // attempt creates nothing and returns.
    let admit_deadline = after_duration(Instant::now(), shutdown_bound);
    if !wait_last_owner(remaining(admit_deadline)) {
        config::report("install_error", "reason=previous_owner_alive");
        return None;
    }
    match RETIRE.wait_retired(remaining(admit_deadline)) {
        Verdict::Retired => {}
        Verdict::Pending => {
            let left = RETIRE.counts();
            config::report(
                "install_error",
                &format!(
                    "reason=previous_threads_running threads={} forwarders={} workers={}",
                    left.threads, left.forwarders, left.workers
                ),
            );
            *lock(&slot.refusal) = Some(Install::RetirementPending);
            return None;
        }
    }
    // A `shutdown` may have cancelled this start while it waited for the previous owner: then no
    // owner is spawned at all (it would only build windows to destroy them).
    if startup.is_cancelled() {
        config::report("install_error", "reason=startup_cancelled");
        return None;
    }
    gfx::register_fonts();
    let tx: CmdTx = Arc::new(CmdQueue::new());
    let (ready_tx, ready_rx) = mpsc::sync_channel::<Ready>(1);
    let exit = Arc::new(ExitSignal::default());
    *lock(&LAST_EXIT) = Some(Arc::clone(&exit));
    *lock(&slot.queue) = Some(Arc::clone(&tx));
    let thread_tx = Arc::clone(&tx);
    let thread_startup = Arc::clone(startup);
    let thread_exit = Arc::clone(&exit);
    let spawned = std::thread::Builder::new()
        .name(config.thread_name.to_string())
        .spawn(move || run(thread_tx, thread_startup, ready_tx, thread_exit));
    match spawned {
        // The owner's own handle is kept until the thread has finished: the next admission and
        // `shutdown` reap it (only once `is_finished()`), and never wait on it past their bound.
        Ok(owner) => RETIRE.hold(startup.generation(), owner),
        Err(_) => {
            // No owner exists: there is nothing to wait for.
            exit.signal();
            config::report("install_error", "reason=spawn_failed");
            return None;
        }
    }
    let (shared, handles) = match ready_rx.recv_timeout(startup_bound) {
        Ok(Ok(ready)) => ready,
        Ok(Err(code)) => {
            config::report("install_error", &format!("reason={code}"));
            return None;
        }
        Err(RecvTimeoutError::Disconnected) => {
            config::report("install_error", "reason=owner_ended_during_startup");
            return None;
        }
        Err(RecvTimeoutError::Timeout) => match startup.cancel() {
            CancelOutcome::Cancelled | CancelOutcome::AlreadyCancelled => {
                // The owner sees the cancellation, destroys what it made and exits; its exit
                // signal stays in `LAST_EXIT`, so the next start and `shutdown` wait for it.
                config::report("install_error", "reason=startup_timeout cancelled=true");
                return None;
            }
            CancelOutcome::AlreadyReady => {
                // The owner won the race a moment before the timeout: take what it published.
                match ready_rx.recv_timeout(shutdown_bound) {
                    Ok(Ok(ready)) => ready,
                    _ => {
                        config::report("install_error", "reason=ready_lost shutdown=requested");
                        tx.post_shutdown();
                        return None;
                    }
                }
            }
        },
    };
    config::report("ready", "overlays=3");
    Some(Controller {
        generation: startup.generation(),
        tx,
        shared,
        handles,
        exit,
    })
}

/// Microphone level for the recording bars. Never installs and never wakes the driver. A
/// non-finite level counts as silence.
pub fn voice_level(level: f32) {
    if let Some(controller) = running() {
        let level = if level.is_finite() {
            level.clamp(0.0, 1.0)
        } else {
            0.0
        };
        controller
            .shared
            .level_bits
            .store(level.to_bits(), Ordering::Relaxed);
    }
}

/// Apply a pill event and optionally show or hide the window. A status or error label is cut to
/// `Limits::max_label_bytes` on the caller's thread, so the queue never carries an unbounded
/// string. Dropped (and reported once) when no generation is running.
pub fn apply_event(event: &PillEvent, vis: Visibility) {
    apply_event_deferred(event, vis).report();
}

/// Bound labels before retaining an event in either the driver or supervisor queue.
pub(crate) fn bounded_event(event: &PillEvent) -> PillEvent {
    let max = config::try_get().map_or(0, |config| config.limits.max_label_bytes);
    let cut = |label: &str| crate::text::cut_bytes(label, max).to_string();
    match event {
        PillEvent::Status { label } => PillEvent::Status { label: cut(label) },
        PillEvent::Error { label } => PillEvent::Error { label: cut(label) },
        other => other.clone(),
    }
}

/// Nonblocking and callback-free: callers may serialize enqueue with their own short state lock.
pub(crate) fn apply_event_deferred(event: &PillEvent, vis: Visibility) -> Dispatch {
    if let PillEvent::Voice { level } = event {
        voice_level(*level);
        return Dispatch::Accepted;
    }
    if config::try_get().is_none() {
        return Dispatch::SilentDrop;
    }
    send_deferred(Cmd::Apply {
        event: bounded_event(event),
        vis,
    })
}

pub fn set_visibility(vis: Visibility) {
    send(Cmd::SetVisibility(vis));
}

/// `PillModel.reveal()`: show with the spring entrance, and the one-time bloom when `intro`.
pub fn reveal(intro: bool) {
    send(Cmd::Reveal { intro });
}

fn shown_flag(read: impl FnOnce(&Published) -> &AtomicBool) -> bool {
    running().is_some_and(|controller| read(&controller.shared.published).load(Ordering::Relaxed))
}

pub fn is_visible() -> bool {
    shown_flag(|p| &p.pill_shown)
}

pub fn hint_visible() -> bool {
    shown_flag(|p| &p.hint_shown)
}

pub fn clipboard_visible() -> bool {
    shown_flag(|p| &p.clip_shown)
}

/// Re-assert TOPMOST on every visible overlay (guardian passes).
pub fn reassert_topmost() {
    send(Cmd::Reassert);
}

/// Recompute the placement (monitor, work area, DPI) and move the overlays.
pub fn reposition() {
    send(Cmd::Reposition);
}

/// Physical `[left, top, right, bottom]` of the monitor the pill sits on.
pub fn monitor_rect() -> Option<[f64; 4]> {
    running().and_then(|controller| *lock(&controller.shared.published.monitor))
}

/// Physical-pixel `[x, y, width, height]` of the capsule as last laid out (settled target, so it
/// is valid while the pill is hidden too). The splash and Hub transitions anchor to it.
pub fn capsule_rect() -> Option<[f64; 4]> {
    running().and_then(|controller| *lock(&controller.shared.published.capsule))
}

/// `[x, y, width, height, scale]` of the pill, hint and clip overlay windows (position in
/// physical px, size in logical px) plus whether each is shown. QA journeys read it to find where
/// a real pointer must go.
pub fn overlay_frames() -> [(Option<[f64; 5]>, bool); 3] {
    match running() {
        Some(controller) => {
            let published = &controller.shared.published;
            let frames = *lock(&published.frames);
            [
                (frames[0], published.pill_shown.load(Ordering::Relaxed)),
                (frames[1], published.hint_shown.load(Ordering::Relaxed)),
                (frames[2], published.clip_shown.load(Ordering::Relaxed)),
            ]
        }
        None => [(None, false); 3],
    }
}

/// HWNDs of the pill, hint and clip overlays that exist (delivery excludes them when it looks
/// for the dictation target). Empty before the driver is running and after `shutdown`; it never
/// starts the driver.
pub fn overlay_hwnds() -> Vec<isize> {
    match running() {
        Some(controller) => controller
            .handles
            .iter()
            .map(gfx::Handle::hwnd)
            .filter(|hwnd| *hwnd != 0)
            .collect(),
        None => Vec::new(),
    }
}

/// Whether a generation is running right now: its owner published the overlays and has not ended.
/// Never starts a generation and never blocks. `false` before `install`, before the first
/// `ensure_installed`, after `shutdown`, and after a failed or faulted generation (a painter
/// fault, an owner panic, an overlay window that ended by itself): a host that wants the pill back
/// calls [`ensure_installed`] again, off its UI thread.
pub fn is_running() -> bool {
    config::try_get().is_some() && running().is_some()
}

/// Show a hint card. Each text is cut to `Limits::max_hint_text_bytes` and the auto-hide delay is
/// clamped to one day, on the caller's thread. `true` only when the owner accepted the command
/// (see [`is_running`] and `call_dropped`).
pub fn show_hint(content: HintContent) -> bool {
    let Some(config) = config::try_get() else {
        return false;
    };
    send(Cmd::ShowHint {
        content: content.bounded(config.limits.max_hint_text_bytes),
    })
}

pub fn dismiss_hint() {
    send(Cmd::DismissHint);
}

/// Show the clip card. An empty list dismisses the card. Otherwise the items are validated on the
/// caller's thread against `Limits` (at most `max_clip_items` kept, ids valid and unique, every
/// payload within `max_clip_payload_bytes`, all payloads within `max_clip_total_bytes`); a refused
/// card is reported (`clip_refused`), not shown, and returned as the error. Nothing is cut: a
/// truncated payload would put a truncated transcript on the clipboard.
///
/// `Ok(())` means the owner's queue accepted the command, not that the card is on screen. A call
/// that was not accepted (no generation running, owner gone, queue full, nothing installed) is
/// `Err(ClipRefusal::NotRunning)`: the host must not set any state that assumes the card shows
/// (an override transcript, a "pill stays expanded" hold), or must undo it.
pub fn show_clipboard(items: Vec<ClipItem>, single: bool) -> Result<(), ClipRefusal> {
    let Some(config) = config::try_get() else {
        return Err(ClipRefusal::NotRunning);
    };
    let command = if items.is_empty() {
        Cmd::DismissClip
    } else {
        match clipboard::validate_items(items, &config.limits) {
            Ok(items) => Cmd::ShowClip { items, single },
            Err(refusal) => {
                config::report("clip_refused", &format!("reason={}", refusal.code()));
                return Err(refusal);
            }
        }
    };
    if send(command) {
        Ok(())
    } else {
        Err(ClipRefusal::NotRunning)
    }
}

pub fn dismiss_clipboard() {
    send(Cmd::DismissClip);
}

// ---------------------------------------------------------------------------------------------
// Driver thread
// ---------------------------------------------------------------------------------------------

/// Every window the owner made so far. Dropping it (on every exit path of the owner, including an
/// unwind, and after a forwarder failed to spawn) closes each window with
/// `OverlayWindow::close_into_join_handle`, which posts the close at once and returns the window
/// thread's `JoinHandle` without waiting, and moves each handle into `RETIRE` for this generation.
/// The owner therefore never joins a window thread (all windows close in parallel and the owner
/// ends at once); `RETIRE` joins a handle only after `is_finished()`. Every window has a handle,
/// so there is no window whose end cannot be observed.
struct Made {
    generation: u64,
    windows: Vec<gfx::Window>,
    handles: Vec<gfx::Handle>,
}

impl Drop for Made {
    fn drop(&mut self) {
        for window in self.windows.drain(..) {
            RETIRE.hold(self.generation, window.close_into_join_handle());
        }
    }
}

/// One live event forwarder in `RETIRE`: created on the owner thread before the forwarder is
/// spawned and moved into it, so it is released by the forwarder's own exit (a return or an
/// unwind), or at once when the spawn fails and the closure is dropped unrun.
struct ForwarderLive(u64);

impl ForwarderLive {
    fn enter(generation: u64) -> ForwarderLive {
        RETIRE.enter(generation);
        ForwarderLive(generation)
    }
}

impl Drop for ForwarderLive {
    fn drop(&mut self) {
        RETIRE.leave(self.0);
    }
}

/// Closes the command queue when the owner ends, however it ends: later pushes are refused and
/// what was waiting is forgotten.
struct QueueCloser(CmdTx);

impl Drop for QueueCloser {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// Display-affinity values to try, in order, so a transcript on the clip card or recovery popup
/// never shows in a screen share or recording. `WDA_EXCLUDEFROMCAPTURE` (Windows 10 2004+) leaves
/// the window out of the capture entirely; older builds reject it and get `WDA_MONITOR`, which
/// captures the window as a black box. Returns the value that was accepted.
pub fn apply_capture_affinity(
    mut set: impl FnMut(u32) -> bool,
    exclude: u32,
    fallback: u32,
) -> Option<u32> {
    [exclude, fallback]
        .into_iter()
        .find(|affinity| set(*affinity))
}

/// Keep one overlay out of screen capture (Teams, Zoom and OBS shares, screenshots). The host
/// turns it off (`HostInputs::exclude_from_capture`) for builds that must capture the pill.
fn exclude_from_capture(hwnd: isize, surface: Surf) {
    if !config::get().exclude_from_capture {
        return;
    }
    let accepted = apply_capture_affinity(
        |affinity| unsafe { SetWindowDisplayAffinity(hwnd as _, affinity) } != 0,
        WDA_EXCLUDEFROMCAPTURE,
        WDA_MONITOR,
    );
    match accepted {
        Some(WDA_EXCLUDEFROMCAPTURE) => {}
        Some(_) => config::report(
            "capture_affinity",
            &format!("surface={surface:?} affinity=monitor"),
        ),
        None => config::report(
            "capture_affinity_error",
            &format!("surface={surface:?} reason=none_accepted"),
        ),
    }
}

enum BuildError {
    /// The start was cancelled (the caller timed out or `shutdown` ran).
    Cancelled,
    /// A fixed code naming the step that failed (never the failing call's own text).
    Failed(&'static str),
}

/// Create the three overlays. The cancellation is checked before each window is created, after
/// each window is created, and after its painter and forwarder thread are in place; the final
/// `Startup::mark_ready` compare-and-swap in `owner_main` decides the rest. Every window made so
/// far sits in `made`, so any early return (and the unwind of a panic) drops, and therefore
/// closes, all of them.
fn build_surfaces(
    shared: &Arc<Shared>,
    tx: &CmdTx,
    startup: &Startup,
    made: &mut Made,
) -> Result<[gfx::Handle; 3], BuildError> {
    for (index, name) in config::get().names.into_iter().enumerate() {
        if startup.is_cancelled() {
            return Err(BuildError::Cancelled);
        }
        let window = gfx::create(name).map_err(BuildError::Failed)?;
        let handle = window.handle();
        made.windows.push(window);
        made.handles.push(handle.clone());
        let surf = match index {
            0 => Surf::Pill,
            1 => Surf::Hint,
            _ => Surf::Clip,
        };
        // The forwarder comes first, before anything can return early. (The window's thread handle
        // is retained through `made`; the forwarder is the second, independent signal that the
        // thread ran `finish`.)
        let events = handle.events();
        let live = ForwarderLive::enter(startup.generation());
        let forward = Arc::clone(tx);
        let spawned = std::thread::Builder::new()
            .name(format!("{}-events-{index}", config::get().thread_name))
            .spawn(move || forward_events(surf, events, &forward, live));
        if spawned.is_err() {
            // `made` closes this window (and the earlier ones) on the way out and retains their
            // thread handles in `RETIRE`.
            return Err(BuildError::Failed("event_forwarder"));
        }
        if startup.is_cancelled() {
            return Err(BuildError::Cancelled);
        }
        exclude_from_capture(handle.hwnd(), surf);
        let fault_tx = Arc::clone(tx);
        let on_fault = move |kind: &'static str| fault_tx.post_fault(surf, kind);
        let painter = match index {
            0 => gfx::boxed(
                PillPainter {
                    shared: Arc::clone(shared),
                    tx: Arc::clone(tx),
                },
                on_fault,
            ),
            1 => gfx::boxed(
                hint::painter::HintPainter {
                    shared: Arc::clone(shared),
                    tx: Arc::clone(tx),
                },
                on_fault,
            ),
            _ => gfx::boxed(
                clipboard::painter::ClipPainter {
                    shared: Arc::clone(shared),
                    tx: Arc::clone(tx),
                },
                on_fault,
            ),
        };
        handle.set_painter(painter);
        if startup.is_cancelled() {
            return Err(BuildError::Cancelled);
        }
    }
    let handles: Vec<gfx::Handle> = made.handles.clone();
    let [a, b, c]: [gfx::Handle; 3] = handles
        .try_into()
        .map_err(|_| BuildError::Failed("handle_count"))?;
    Ok([a, b, c])
}

/// The forwarder thread of one overlay: hands the events the owner uses (hover enter and leave,
/// click, a DPI or monitor change) to the owner's queue and ends when the window does. Pointer
/// moves, characters, keys and focus changes are never used by the owner, so they never enter the
/// queue. When the window is gone (`Closed`, or the receiver disconnecting without it: the
/// overlay's bounded channel can miss `Closed`) it tells the owner once, through the part of the
/// queue that is never dropped; the owner then treats the generation as dead.
///
/// It does not end when the owner's queue closes: it keeps reading (and discarding) until the
/// overlay drops its sender, because that is the only observable for the window thread's end, and
/// `_live` (released when this function returns or unwinds) is what keeps `RETIRE` from admitting
/// a new generation before then.
fn forward_events(
    surf: Surf,
    events: mpsc::Receiver<gfx::OverlayEvent>,
    tx: &CmdTx,
    _live: ForwarderLive,
) {
    let mut owner_open = true;
    for event in events.iter() {
        if matches!(event, gfx::OverlayEvent::Closed) {
            break;
        }
        let used = matches!(
            event,
            gfx::OverlayEvent::HoverEnter(_)
                | gfx::OverlayEvent::HoverLeave(_)
                | gfx::OverlayEvent::Click(_)
                | gfx::OverlayEvent::DpiChanged(_)
                | gfx::OverlayEvent::MonitorChanged
        );
        if used && owner_open {
            match tx.push(Cmd::Overlay(surf, event)) {
                Push::Queued => {}
                Push::Dropped { first_of_burst } => {
                    if first_of_burst {
                        config::report("call_dropped", "reason=queue_full");
                    }
                }
                Push::Closed => owner_open = false,
            }
        }
    }
    let _ = tx.push_reserved(Cmd::Overlay(surf, gfx::OverlayEvent::Closed));
}

/// Clears what a generation published when its owner ends, however it ends (the guard runs on a
/// normal return and on an unwind).
struct PublishedGuard(Arc<Shared>);

impl Drop for PublishedGuard {
    fn drop(&mut self) {
        self.0.published.clear();
    }
}

/// The owner thread body. Always ends by destroying every window it made, then signals `exit`.
fn run(tx: CmdTx, startup: Arc<Startup>, ready: SyncSender<Ready>, exit: Arc<ExitSignal>) {
    ON_OWNER.with(|flag| flag.set(true));
    let generation = startup.generation();
    let outcome = catch_unwind(AssertUnwindSafe(|| owner_main(&tx, &startup, ready)));
    let died = match outcome {
        Ok(None) => false,
        Ok(Some(code)) => {
            config::report(
                "driver_fault",
                &format!("generation={generation} code={code}"),
            );
            true
        }
        Err(payload) => {
            config::report(
                "driver_panic",
                &format!(
                    "generation={generation} type={}",
                    panic_type(payload.as_ref())
                ),
            );
            true
        }
    };
    // Every window of this generation was asked to close by now (owner_main's locals, or the
    // unwind) and its thread handle is in `RETIRE`, as is this owner's own (kept by `launch`).
    if died {
        // Before `mark_dead`: a starter installing this controller later reads it (`start`).
        exit.died.store(true, Ordering::SeqCst);
        let marked = lock(&LIFE).mark_dead(generation);
        CHANGED.notify_all();
        if marked {
            config::report(
                "driver_stopped",
                &format!("generation={generation} overlays=destroyed restart=explicit"),
            );
        }
    }
    exit.signal();
}

/// `Some(code)` when the generation ended on a fault (a painter panic); `None` for a normal end
/// (shutdown, cancelled start). A panic unwinds out of here; the windows are dropped on the way.
/// The shared state is built here, on the owner thread, so the host's look is never asked on a
/// caller's thread; a panic while building it ends the owner and the starter sees the channel
/// disconnect.
fn owner_main(tx: &CmdTx, startup: &Startup, ready: SyncSender<Ready>) -> Option<&'static str> {
    // Declared first, so it is dropped last: the queue closes once everything else is gone.
    let _closer = QueueCloser(Arc::clone(tx));
    let shared = Arc::new(Shared::new(Instant::now()));
    // Declared before `made` so it is dropped after it: the published flags clear only once the
    // windows are gone.
    let _published = PublishedGuard(Arc::clone(&shared));
    let mut made = Made {
        generation: startup.generation(),
        windows: Vec::with_capacity(3),
        handles: Vec::with_capacity(3),
    };
    let handles = match build_surfaces(&shared, tx, startup, &mut made) {
        Ok(handles) => handles,
        Err(BuildError::Cancelled) => {
            let _ = ready.send(Err("startup_cancelled"));
            return None;
        }
        Err(BuildError::Failed(code)) => {
            let _ = ready.send(Err(code));
            return None;
        }
    };
    let mut core = Core::new(Arc::clone(&shared), handles.clone(), Arc::clone(tx));
    // Publish only if the starter has not given up. Losing this race means the caller owns
    // nothing: destroy what was made (the drops below) and exit.
    if !startup.mark_ready() {
        let _ = ready.send(Err("startup_cancelled"));
        return None;
    }
    if ready
        .send(Ok((Arc::clone(&shared), handles.clone())))
        .is_err()
    {
        return None;
    }
    let fault = event_loop(&mut core, tx);
    drop(core);
    fault
}

enum Flow {
    Continue,
    Stop,
    /// A fixed code naming the fault (never the fault's own text).
    Fault(&'static str),
}

/// While draining a queue that never empties, timers and deadlines are serviced after at most this
/// many commands...
const SERVICE_EVERY_COMMANDS: usize = 16;
/// ...or after this much time since the last service, whichever comes first. The gap between two
/// service points is therefore at most `SERVICE_EVERY_COMMANDS` commands, or this slice plus the
/// duration of the one command being handled when it ends.
const SERVICE_EVERY: Duration = Duration::from_millis(10);

fn event_loop(core: &mut Core, queue: &CmdQueue<Cmd>) -> Option<&'static str> {
    loop {
        // `None` blocks until something arrives: an idle pill has no timer.
        let wait = core
            .next_deadline()
            .map(|deadline| deadline.saturating_duration_since(Instant::now()));
        let mut received = queue.next(wait);
        let mut since_service = 0usize;
        let mut serviced_at = Instant::now();
        loop {
            let flow = match received {
                Next::Item(cmd) => {
                    since_service = since_service.saturating_add(1);
                    core.handle(cmd, Instant::now())
                }
                Next::Control(control) => {
                    since_service = since_service.saturating_add(1);
                    core.handle_control(control, Instant::now())
                }
                Next::Timeout => break,
                Next::Closed => return None,
            };
            match flow {
                Flow::Continue => {}
                Flow::Stop => return None,
                Flow::Fault(code) => return Some(code),
            }
            // A queue that is never empty must not starve the timers (a copy or host-action
            // deadline, a hide timer, the pointer polls): service them every few commands or
            // milliseconds, on the monotonic clock.
            let now = Instant::now();
            if since_service >= SERVICE_EVERY_COMMANDS
                || now.saturating_duration_since(serviced_at) >= SERVICE_EVERY
            {
                core.on_timers(now);
                since_service = 0;
                serviced_at = now;
            }
            // Whatever is already waiting belongs to this batch.
            received = queue.next(Some(Duration::ZERO));
        }
        core.on_timers(Instant::now());
    }
}

struct Placement {
    /// Horizontal centre of the monitor, physical px.
    center_x: f64,
    /// Capsule bottom edge, physical px (work-area bottom minus the host's bottom margin).
    capsule_bottom: f64,
    scale: f32,
}

impl Placement {
    fn fallback() -> Placement {
        let fallback = config::get().placement;
        Placement {
            center_x: fallback.fallback_center_x,
            capsule_bottom: fallback.fallback_capsule_bottom,
            scale: 1.0,
        }
    }
}

fn compute_placement() -> (Placement, Option<[f64; 4]>) {
    let (x, y) = gfx::cursor_pos().unwrap_or((0, 0));
    match rightkit_overlay::windows::monitor_at(x, y) {
        Some(monitor) => {
            let s = monitor.scale();
            (
                Placement {
                    center_x: (monitor.bounds.left + monitor.bounds.right) * 0.5,
                    capsule_bottom: monitor.work.bottom - f64::from(metrics().bottom_margin) * s,
                    scale: s as f32,
                },
                Some([
                    monitor.bounds.left,
                    monitor.bounds.top,
                    monitor.bounds.right,
                    monitor.bounds.bottom,
                ]),
            )
        }
        None => (Placement::fallback(), None),
    }
}

/// Whether an applied pill event must re-assert the pill's TOPMOST band. Every event that changes
/// what a visible pill shows does; `Hide` and the resting `Idle` sliver do not, which keeps an
/// idle desktop free of window-position calls.
pub fn reassert_on(event: &PillEvent, pill_shown: bool) -> bool {
    pill_shown
        && matches!(
            event,
            PillEvent::Starting
                | PillEvent::Recording
                | PillEvent::Processing { .. }
                | PillEvent::Success
                | PillEvent::Status { .. }
                | PillEvent::Error { .. }
        )
}

/// Run a host callback on the calling thread, catching a panic. The panic is reported by the
/// callback's name and the payload's type (never its text) and never propagated. This guards the
/// `Callbacks` the crate calls; calls into the host's look (`Painters`) on the owner thread are not
/// individually guarded, and a panic there ends the generation through `run`.
fn host<R>(what: &'static str, f: impl FnOnce() -> R) -> Option<R> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => Some(value),
        Err(payload) => {
            config::report(
                "host_panic",
                &format!("callback={what} type={}", panic_type(payload.as_ref())),
            );
            None
        }
    }
}

/// `activate`, with a panic counted as a failure.
fn run_activate(activation: Activation) -> ActionOutcome {
    host("activate", || {
        (config::get().callbacks.activate)(activation)
    })
    .unwrap_or(ActionOutcome::Failed)
}

/// `copy_text`, with a panic counted as a failure.
fn run_copy(text: String) -> bool {
    host("copy_text", || (config::get().callbacks.copy_text)(text)).unwrap_or(false)
}

/// Window top-left in physical screen px, size in logical px (`set_frame`'s contract).
type Frame = (i32, i32, f32, f32);

/// The settled state a layout pass copies out of the shared lock.
struct LayoutSnapshot {
    scene: crate::look::PillScene,
    hint: Option<HintContent>,
    clip: Option<ClipState>,
    leaving: bool,
    scale: f32,
    extent: (f32, f32, f32),
}

/// What a clip copy in flight is about.
#[derive(Clone, Debug)]
struct ClipCopy {
    id: String,
    /// The region asked for the card to close after the action: it closes on success only.
    close_after: bool,
}

struct Core {
    shared: Arc<Shared>,
    tx: CmdTx,
    handles: [gfx::Handle; 3],
    hover: [HashSet<RegionId>; 3],
    shown: [bool; 3],
    frames: [Option<Frame>; 3],
    regions: [Vec<Region>; 3],
    /// Tag of the secondary action under the pointer on the pill, polled for the right button.
    rwatch_region: Option<Region>,
    placement: Placement,
    /// Capsule rectangle in physical screen px, from the last layout (hover reconciliation).
    capsule_phys: Option<(f64, f64, f64, f64)>,
    region_refresh_at: Option<Instant>,
    click_watch_at: Option<Instant>,
    click_primed: bool,
    prev_down: bool,
    last_hitbox: Option<(f32, f32, f32, f32)>,
    /// The overlay reports no right click, so while the pointer rests on a region with a
    /// secondary action the right mouse button is polled (every 50 ms).
    rwatch_at: Option<Instant>,
    rprev_down: bool,
    /// Capsule height the aux cards were last stacked on while none was leaving: a card that is
    /// fading out stays where it was while the capsule underneath already changes size.
    stack_anchor_h: f32,
    /// The clip copy in flight (its result decides "copied", the close timer and the failure).
    clip_copy: Tracker<ClipCopy>,
    /// The `HostFlash` action in flight (its outcome decides the check mark or the dimming).
    flash: Tracker<()>,
    /// A `Host` action that closes a card in flight; the key is the card to close on `Done`.
    host_close: Tracker<Surf>,
}

/// One slot of the process-wide worker cap (`WORKERS`); released when the worker thread ends or
/// fails to start.
struct WorkerSlot;

impl WorkerSlot {
    /// Counts the worker in `WORKERS` (the cap) and in `RETIRE` (admission of a new generation).
    fn take() -> WorkerSlot {
        RETIRE.enter_worker();
        WorkerSlot
    }
}

impl Drop for WorkerSlot {
    fn drop(&mut self) {
        WORKERS.fetch_sub(1, Ordering::SeqCst);
        RETIRE.leave_worker();
    }
}

impl Core {
    fn new(shared: Arc<Shared>, handles: [gfx::Handle; 3], tx: CmdTx) -> Core {
        let (placement, monitor) = compute_placement();
        *lock(&shared.published.monitor) = monitor;
        {
            let mut visual = shared.visual.lock().unwrap_or_else(|e| e.into_inner());
            visual.scale = placement.scale;
            visual.pill.animations = gfx::animations_enabled();
        }
        Core {
            shared,
            tx,
            handles,
            hover: [HashSet::new(), HashSet::new(), HashSet::new()],
            shown: [false; 3],
            frames: [None, None, None],
            regions: [Vec::new(), Vec::new(), Vec::new()],
            rwatch_region: None,
            placement,
            capsule_phys: None,
            region_refresh_at: None,
            click_watch_at: None,
            click_primed: false,
            prev_down: false,
            last_hitbox: None,
            rwatch_at: None,
            rprev_down: false,
            stack_anchor_h: config::capsule_size(Mode::Idle, false, false, "").1,
            clip_copy: Tracker::new(),
            flash: Tracker::new(),
            host_close: Tracker::new(),
        }
    }

    fn with_visual<R>(&self, f: impl FnOnce(&mut Visual) -> R) -> R {
        let mut guard = self.shared.visual.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard)
    }

    fn published(&self) -> &Published {
        &self.shared.published
    }

    /// Run `work` on a short-lived named worker thread. `Err` carries a fixed code: `worker_cap`
    /// when `Limits::max_worker_threads` workers are already running anywhere in the process (a
    /// host callback that never returns holds its slot, across generations), `spawn_failed` when
    /// the OS refused a thread.
    fn spawn(&self, name: &str, work: impl FnOnce() + Send + 'static) -> Result<(), &'static str> {
        let cap = config::limits().max_worker_threads;
        if WORKERS.fetch_add(1, Ordering::SeqCst) >= cap {
            WORKERS.fetch_sub(1, Ordering::SeqCst);
            return Err("worker_cap");
        }
        let slot = WorkerSlot::take();
        std::thread::Builder::new()
            .name(format!("{}-{name}", config::get().thread_name))
            .spawn(move || {
                let _slot = slot;
                work();
            })
            .map(|_| ())
            .map_err(|_| "spawn_failed")
    }

    // --- command dispatch ---------------------------------------------------------------------

    fn handle(&mut self, cmd: Cmd, now: Instant) -> Flow {
        if !matches!(cmd, Cmd::Overlay(..)) {
            // Re-read the "show animations" setting (a cheap call) so toggling it takes effect on
            // the next change, with no restart.
            let animations = gfx::animations_enabled();
            self.with_visual(|v| v.pill.animations = animations);
        }
        match cmd {
            Cmd::Apply { event, vis } => self.apply(event, vis, now),
            Cmd::SetVisibility(vis) => {
                self.apply_visibility(vis, now);
                self.after_change(now);
            }
            Cmd::Reveal { intro } => {
                self.apply_visibility(Visibility::Show, now);
                let effects = self.with_visual(|v| v.pill.reveal(now, intro));
                self.run_effects(effects, now);
                self.after_change(now);
            }
            Cmd::Overlay(surf, event) => return self.on_overlay(surf, event, now),
            Cmd::ShowHint { content } => self.show_hint(content, now),
            Cmd::DismissHint => self.dismiss_hint(now),
            Cmd::ShowClip { items, single } => self.show_clip(items, single, now),
            Cmd::DismissClip => self.dismiss_clip(now),
            Cmd::CopyDone { token, ok } => self.on_copy_done(token, ok, now),
            Cmd::FlashDone { token, outcome } => {
                // `now` is read when this command is handled, so a result that waited in the
                // queue past its deadline is overdue here whether or not a timer fired.
                match self.flash.resolve(token, now) {
                    Resolution::Resolved(()) => {
                        self.with_visual(|v| {
                            v.pill.flash_copy(outcome == ActionOutcome::Done, now)
                        });
                        self.after_change(now);
                    }
                    Resolution::Overdue(()) => {
                        self.flash_timed_out(now);
                        self.after_change(now);
                    }
                    // Settled already (the deadline, a newer attempt or a cancel): nothing.
                    Resolution::Stale => {}
                }
            }
            Cmd::ActionDone { token, outcome } => self.on_action_done(token, outcome, now),
            Cmd::Reassert => {
                for index in 0..3 {
                    if self.shown[index] {
                        self.handles[index].reassert_topmost();
                    }
                }
            }
            Cmd::Reposition => {
                self.place();
                self.after_change(now);
            }
        }
        Flow::Continue
    }

    /// What never goes through the bounded list: shutdown, a painter fault, a frame pump that can
    /// stop.
    fn handle_control(&mut self, control: Control, now: Instant) -> Flow {
        match control {
            Control::Shutdown => Flow::Stop,
            Control::Fault { surface, kind } => {
                config::report("painter_panic", &format!("surface={surface:?} type={kind}"));
                Flow::Fault("painter_panic")
            }
            Control::PumpIdle(surf) => {
                self.on_pump_idle(surf, now);
                Flow::Continue
            }
        }
    }

    fn apply(&mut self, event: PillEvent, vis: Visibility, now: Instant) {
        if !matches!(event, PillEvent::Hide) {
            let generation = if matches!(event, PillEvent::Error { .. }) {
                host("error_generation", || {
                    (config::get().callbacks.error_generation)()
                })
                .flatten()
            } else {
                None
            };
            let (effects, mode) = self.with_visual(|v| {
                let effects = v.pill.apply_event(&event, now, generation);
                (effects, v.pill.mode)
            });
            if mode != Mode::Recording {
                self.shared
                    .level_bits
                    .store(0f32.to_bits(), Ordering::Relaxed);
            }
            self.run_effects(effects, now);
        }
        self.apply_visibility(vis, now);
        // A hotkey start raises no foreground event, so nothing else would lift a pill that the
        // topmost band dropped: re-assert on every visible state change (one `SetWindowPos`).
        if reassert_on(&event, self.shown[Surf::Pill as usize]) {
            self.handles[Surf::Pill as usize].reassert_topmost();
        }
        self.after_change(now);
    }

    fn apply_visibility(&mut self, vis: Visibility, now: Instant) {
        match vis {
            Visibility::Show => self.show_pill(now),
            Visibility::Hide => self.with_visual(|v| v.pill.set_visible(false, now)),
            Visibility::Keep => {}
        }
    }

    fn show_pill(&mut self, now: Instant) {
        self.with_visual(|v| v.pill.set_visible(true, now));
        if !self.shown[Surf::Pill as usize] {
            self.place();
            self.layout_all(now);
            let handle = &self.handles[Surf::Pill as usize];
            handle.show();
            handle.reassert_topmost();
            self.shown[Surf::Pill as usize] = true;
            self.published().pill_shown.store(true, Ordering::Relaxed);
        }
    }

    fn hide_pill_window(&mut self, now: Instant) {
        let index = Surf::Pill as usize;
        if self.shown[index] {
            self.handles[index].hide();
            self.shown[index] = false;
            self.published().pill_shown.store(false, Ordering::Relaxed);
        }
        self.hover[index].clear();
        self.rwatch_at = None;
        self.rwatch_region = None;
        self.shared.pump[index].store(false, Ordering::SeqCst);
        self.with_visual(|v| {
            v.pill.set_pointer(false, now);
            v.pill.clear_hot(now);
        });
    }

    fn run_effects(&mut self, effects: Vec<Effect>, now: Instant) {
        for effect in effects {
            match effect {
                Effect::HideWindow => self.hide_pill_window(now),
                Effect::PlayIntroSound => gfx::play_intro_sound(),
                Effect::RevealPill => {
                    let started = self.spawn("reveal", || {
                        host("reveal_pill", || (config::get().callbacks.reveal_pill)());
                    });
                    if let Err(reason) = started {
                        config::report("worker_error", &format!("job=reveal reason={reason}"));
                    }
                }
                Effect::RecoverTerminalError(generation) => {
                    let started = self.spawn("recover", move || {
                        let result = host("recover_error", || {
                            (config::get().callbacks.recover_error)(generation)
                        });
                        if result == Some(false) {
                            config::report("terminal_reset_rejected", "result=false");
                        }
                    });
                    if let Err(reason) = started {
                        config::report("worker_error", &format!("job=recover reason={reason}"));
                    }
                }
            }
        }
    }

    // --- placement and layout -----------------------------------------------------------------

    fn place(&mut self) {
        let (placement, monitor) = compute_placement();
        *lock(&self.published().monitor) = monitor;
        let scale = placement.scale;
        self.placement = placement;
        self.with_visual(|v| v.scale = scale);
    }

    /// An aux card can appear while the pill window is hidden (`recording_only`); anchor it to
    /// the monitor the user is on then.
    fn place_if_idle(&mut self) {
        if !self.shown.iter().any(|shown| *shown) {
            self.place();
        }
    }

    fn set_frame(&mut self, surf: Surf, rect: Frame) {
        let index = surf as usize;
        if self.frames[index] == Some(rect) {
            return;
        }
        self.frames[index] = Some(rect);
        lock(&self.published().frames)[index] = Some([
            f64::from(rect.0),
            f64::from(rect.1),
            f64::from(rect.2),
            f64::from(rect.3),
            f64::from(self.placement.scale),
        ]);
        self.handles[index].set_frame(gfx::frame_rect(rect.0, rect.1, rect.2, rect.3));
    }

    fn set_regions(&mut self, surf: Surf, mut regions: Vec<Region>) {
        let index = surf as usize;
        // A look cannot make the hit-region list unbounded: later regions are dropped.
        regions.truncate(bound::MAX_REGIONS);
        if self.regions[index] == regions {
            return;
        }
        if gfx::cursor_is_inert()
            && regions
                .iter()
                .any(|region| region.cursor != Cursor::Default)
            && !self
                .shared
                .cursor_inert_reported
                .swap(true, Ordering::Relaxed)
        {
            config::report(
                "cursor_inert",
                &format!("surface={surf:?} reason=overlay_backend_has_no_region_cursor"),
            );
        }
        let converted: Vec<_> = regions.iter().map(gfx::region).collect();
        self.regions[index] = regions;
        self.handles[index].set_hit_regions(converted);
    }

    /// Recompute every frame and hit region from the settled state (no animation values). The
    /// settled state is copied under the lock and the look is asked after the lock is released,
    /// so a slow look never blocks the overlay threads' paints.
    fn layout_all(&mut self, now: Instant) {
        let m = *metrics();
        let window_h = m.pill_window_h + m.pill_window_bottom_pad;
        let snapshot = self.with_visual(|v| {
            let elapsed = v
                .pill
                .rec_start
                .map(|s| now.saturating_duration_since(s).as_secs() as u32)
                .unwrap_or(0);
            LayoutSnapshot {
                scene: v.pill.settled_scene(elapsed),
                hint: v.hint.as_ref().map(|h| h.content.clone()),
                clip: v.clip.clone(),
                leaving: v.hint.as_ref().is_some_and(HintState::is_leaving)
                    || v.clip.as_ref().is_some_and(ClipState::is_leaving),
                scale: v.scale,
                extent: v.pill.visual_extent(now),
            }
        });
        let painters = &config::get().painters;
        let target_h = snapshot.scene.cap_h;
        let pill_frame = painters.pill(
            &snapshot.scene,
            m.pill_window_w,
            window_h,
            m.pill_window_bottom_pad,
            Pass::Layout,
        );
        let hot = HotSet::empty();
        let hint = snapshot.hint.as_ref().map(|content| {
            painters.hint(&HintView {
                content,
                hot: &hot,
                pose: AuxPose::SETTLED,
            })
        });
        let clip = snapshot.clip.as_ref().map(|state| {
            painters.clip(&ClipView {
                state,
                hot: &hot,
                copied_id: None,
                copied_amount: 0.0,
                failed_id: None,
                failed_amount: 0.0,
                pose: AuxPose::SETTLED,
            })
        });
        let scale = snapshot.scale;
        let s = f64::from(scale);
        if !snapshot.leaving {
            self.stack_anchor_h = target_h;
        }
        let anchor_h = self.stack_anchor_h;

        // Pill window: fixed envelope, capsule bottom pinned above the work area. Size is
        // logical; only the top-left is physical.
        let win_w_phys = (f64::from(m.pill_window_w) * s).round();
        let win_h_phys = (f64::from(window_h) * s).round();
        let x = (self.placement.center_x - win_w_phys * 0.5).round() as i32;
        let y = (self.placement.capsule_bottom + f64::from(m.pill_window_bottom_pad) * s
            - win_h_phys)
            .round() as i32;
        self.set_frame(Surf::Pill, (x, y, m.pill_window_w, window_h));
        // Later regions sit on top of earlier ones: the animation extent (visual only), then the
        // settled capsule, then its controls.
        let (ext_w, ext_h, ext_extra) = snapshot.extent;
        let visual = VRect::new(
            (m.pill_window_w - ext_w) * 0.5,
            window_h - m.pill_window_bottom_pad - ext_h,
            ext_w,
            ext_h + ext_extra,
        );
        let capsule = pill_frame.capsule;
        let mut pill_regions = Vec::with_capacity(2 + pill_frame.regions.len());
        pill_regions.push(Region::new(RegionId::VISUAL, visual, HoverRole::Body));
        pill_regions.push(Region::new(RegionId::CAPSULE, capsule, HoverRole::Body));
        pill_regions.extend(pill_frame.regions);
        self.set_regions(Surf::Pill, pill_regions);
        let capsule_phys = (
            f64::from(x) + f64::from(capsule.x) * s,
            f64::from(y) + f64::from(capsule.y) * s,
            f64::from(capsule.w) * s,
            f64::from(capsule.h) * s,
        );
        self.capsule_phys = Some(capsule_phys);
        *lock(&self.published().capsule) = Some([
            capsule_phys.0,
            capsule_phys.1,
            capsule_phys.2,
            capsule_phys.3,
        ]);
        self.publish_hitbox(capsule, x, y, scale);

        // Aux cards stack above the capsule in the host's order (`PlacementPolicy::card_order`):
        // the first sits against the capsule, the second above it.
        let capsule_top = self.placement.capsule_bottom - f64::from(anchor_h) * s;
        let mut stack_top = capsule_top;
        let cards = match config::get().placement.card_order {
            CardOrder::ClipNearest => [(Surf::Clip, clip), (Surf::Hint, hint)],
            CardOrder::HintNearest => [(Surf::Hint, hint), (Surf::Clip, clip)],
        };
        for (surf, frame) in cards {
            if let Some(frame) = frame {
                let anchor_bottom = stack_top - f64::from(m.aux_spacing) * s;
                let card_h = frame.card.h;
                let rect = self.aux_frame(&frame, anchor_bottom, s);
                self.set_frame(surf, rect);
                self.set_regions(surf, frame.regions);
                stack_top = anchor_bottom - f64::from(card_h) * s;
            }
        }
    }

    /// Frame of an aux window whose card (`frame.card`, window-relative logical px) is centred on
    /// the pill and whose bottom edge sits at `anchor_bottom` (physical px). The window size is
    /// sanitised here (finite, `1..=bound::MAX_SURFACE_PX`) so the same frame always compares equal.
    fn aux_frame(&self, frame: &AuxFrame, anchor_bottom: f64, s: f64) -> Frame {
        let outer = frame.card;
        let x = (self.placement.center_x - f64::from(outer.cx()) * s).round() as i32;
        let y = (anchor_bottom - f64::from(outer.bottom()) * s).round() as i32;
        let edge = |v: f32| {
            if v.is_finite() {
                v.clamp(1.0, bound::MAX_SURFACE_PX)
            } else {
                1.0
            }
        };
        (x, y, edge(frame.window.0), edge(frame.window.1))
    }

    /// Keep the host's hitbox/frame snapshots current (the host's hub open/close transition
    /// anchors to them) through `Callbacks::publish_capsule_hitbox` and `publish_window_frame`.
    fn publish_hitbox(&mut self, capsule: VRect, win_x: i32, win_y: i32, scale: f32) {
        let hitbox = (capsule.x, capsule.y, capsule.w, capsule.h);
        if self.last_hitbox != Some(hitbox) {
            self.last_hitbox = Some(hitbox);
            host("publish_capsule_hitbox", || {
                (config::get().callbacks.publish_capsule_hitbox)(
                    f64::from(capsule.x),
                    f64::from(capsule.y),
                    f64::from(capsule.w),
                    f64::from(capsule.h),
                )
            });
        }
        host("publish_window_frame", || {
            (config::get().callbacks.publish_window_frame)(
                f64::from(win_x),
                f64::from(win_y),
                f64::from(scale),
            )
        });
    }

    // --- hover --------------------------------------------------------------------------------

    fn on_overlay(&mut self, surf: Surf, event: gfx::OverlayEvent, now: Instant) -> Flow {
        match event {
            gfx::OverlayEvent::HoverEnter(id) => self.hover_changed(surf, id, true, now),
            gfx::OverlayEvent::HoverLeave(id) => self.hover_changed(surf, id, false, now),
            gfx::OverlayEvent::Click(id) => self.click(surf, id, now),
            gfx::OverlayEvent::DpiChanged(_) | gfx::OverlayEvent::MonitorChanged => {
                if surf == Surf::Pill {
                    self.place();
                    self.after_change(now);
                }
            }
            gfx::OverlayEvent::Closed => {
                // A window that ends by itself (the OS or another process destroyed it, its thread
                // died) leaves a pill that looks alive and cannot show: the generation is dead and
                // only an explicit `ensure_installed` replaces it. The owner's own teardown never
                // gets here: it runs after this loop ended.
                config::report("overlay_closed", &format!("surface={surf:?}"));
                return Flow::Fault("overlay_closed");
            }
            _ => {}
        }
        Flow::Continue
    }

    fn region_of(&self, surf: Surf, id: RegionId) -> Option<Region> {
        self.regions[surf as usize]
            .iter()
            .rev()
            .find(|region| region.id == id)
            .cloned()
    }

    fn hover_changed(&mut self, surf: Surf, raw_id: u32, entered: bool, now: Instant) {
        let index = surf as usize;
        let id = RegionId(raw_id);
        if entered {
            self.hover[index].insert(id);
        } else {
            self.hover[index].remove(&id);
        }
        let ids: Vec<RegionId> = self.hover[index].iter().copied().collect();
        let controls: Vec<RegionId> = self.regions[index]
            .iter()
            .filter(|region| region.hover == HoverRole::Control)
            .map(|region| region.id)
            .collect();
        let role = self.region_of(surf, id).map(|region| region.hover);
        self.with_visual(|v| {
            let ms = v.pill.ms(now);
            match surf {
                Surf::Pill => {
                    v.pill
                        .set_pointer(ids.iter().any(|region| *region != RegionId::VISUAL), now);
                    for control in &controls {
                        v.pill.set_hot(*control, ids.contains(control), now);
                    }
                }
                Surf::Hint => {
                    if let Some(hint) = v.hint.as_mut() {
                        if role == Some(HoverRole::Control) {
                            hint.set_hot(id, entered, ms);
                        }
                    }
                }
                Surf::Clip => {
                    if let Some(clip) = v.clip.as_mut() {
                        if role == Some(HoverRole::Control) {
                            clip.set_hot(id, entered, ms);
                        }
                        if entered {
                            // The user is reading or using the card: stop the auto-dismiss.
                            clip.autodismiss_at = None;
                        }
                    }
                }
            }
        });
        if surf == Surf::Pill {
            let secondary = ids
                .iter()
                .filter_map(|region| self.region_of(surf, *region))
                .find(|region| region.secondary.is_some());
            match secondary {
                None => {
                    self.rwatch_at = None;
                    self.rwatch_region = None;
                }
                Some(region) => {
                    self.rwatch_region = Some(region);
                    if self.rwatch_at.is_none() {
                        self.rprev_down = unsafe { GetAsyncKeyState(0x02) } < 0;
                        self.rwatch_at = Some(after(now, config::timings().pointer_poll_ms));
                    }
                }
            }
        }
        self.after_change(now);
    }

    /// `reconcileHoverFromPointer`: after any state change the pointer's real position wins over
    /// events that may predate the new layout. Returns true when the model changed.
    fn reconcile_pointer(&mut self, now: Instant) -> bool {
        if !self.shown[Surf::Pill as usize] {
            return false;
        }
        let (Some((cx, cy)), Some((rx, ry, rw, rh))) = (gfx::cursor_pos(), self.capsule_phys)
        else {
            return false;
        };
        let (px, py) = (f64::from(cx), f64::from(cy));
        let inside = px >= rx && px <= rx + rw && py >= ry && py <= ry + rh;
        if !inside {
            self.hover[Surf::Pill as usize].clear();
        }
        self.with_visual(|v| {
            let before = (v.pill.pointer_inside, v.pill.hovered);
            v.pill.set_pointer(inside, now);
            if !inside {
                v.pill.clear_hot(now);
            }
            before != (v.pill.pointer_inside, v.pill.hovered)
        })
    }

    // --- clicks ---------------------------------------------------------------------------------

    fn click(&mut self, surf: Surf, raw_id: u32, now: Instant) {
        let Some(region) = self.region_of(surf, RegionId(raw_id)) else {
            return;
        };
        if surf != Surf::Pill {
            let leaving = self.with_visual(|v| match surf {
                Surf::Hint => v.hint.as_ref().is_some_and(HintState::is_leaving),
                _ => v.clip.as_ref().is_some_and(ClipState::is_leaving),
            });
            if leaving {
                return;
            }
            if surf == Surf::Clip {
                self.with_visual(|v| {
                    if let Some(clip) = v.clip.as_mut() {
                        clip.autodismiss_at = None;
                    }
                });
            }
        }
        if surf == Surf::Pill {
            // The region id is a number the host chose; no host text is logged.
            config::report(
                "action",
                &format!("surface=Pill region={} phase=clicked", region.id.0),
            );
        }
        let activation = |tag| Activation {
            surface: surf,
            region: region.id,
            tag,
        };
        match region.action {
            Action::None => {}
            Action::Dismiss => self.dismiss_card(surf, now),
            Action::Host(tag) => {
                // The card to close, and only when the host reports `Done`: the owning card for
                // a card's own region, the open clip card for a capsule region that asks for it.
                let close = if region.close_card_after && surf != Surf::Pill {
                    Some(surf)
                } else if region.close_clip_on_success && self.clip_open() {
                    Some(Surf::Clip)
                } else {
                    None
                };
                self.run_host(activation(tag), close, now)
            }
            Action::HostFlash(tag) => {
                // A disabled (empty) control, or one still waiting for the host, ignores the click.
                if !self.with_visual(|v| v.pill.copy_blocked(now)) && !self.flash.is_pending() {
                    self.run_flash(activation(tag), now);
                }
            }
            Action::CopyCurrent => self.copy_from_clip(None, region.close_card_after, now),
            Action::CopyItem(index) => {
                self.copy_from_clip(Some(index), region.close_card_after, now)
            }
            Action::ToggleAlternate => {
                self.with_visual(|v| {
                    if let Some(clip) = v.clip.as_mut() {
                        clip.toggle_alternate();
                    }
                });
            }
        }
        // `close_card_after` closes at once only after actions that finish synchronously here.
        // Asynchronous host work (`Host`, a copy) closes the card from its own result.
        if region.close_card_after
            && matches!(
                region.action,
                Action::None | Action::Dismiss | Action::ToggleAlternate
            )
        {
            self.dismiss_card(surf, now);
        }
        self.after_change(now);
    }

    fn dismiss_card(&mut self, surf: Surf, now: Instant) {
        match surf {
            Surf::Hint => self.dismiss_hint(now),
            Surf::Clip => self.dismiss_clip(now),
            Surf::Pill => {}
        }
    }

    fn clip_open(&self) -> bool {
        self.with_visual(|v| v.clip.is_some())
    }

    // --- host work: nothing is shown before the host reports ------------------------------------

    /// A `Host` region. With `close` set, that card closes only when the host returns `Done`
    /// within `Timings::host_action_deadline_ms`; without it the outcome is not read.
    fn run_host(&mut self, activation: Activation, close: Option<Surf>, now: Instant) {
        let Some(surf) = close else {
            if let Err(reason) = self.spawn("action", move || {
                let _ = run_activate(activation);
            }) {
                config::report("worker_error", &format!("job=action reason={reason}"));
            }
            return;
        };
        let deadline = config::timings().host_action_deadline_ms;
        let Some(token) = self.host_close.begin(surf, now, deadline) else {
            return;
        };
        let tx = self.tx.clone();
        if let Err(reason) = self.spawn("action", move || {
            let outcome = run_activate(activation);
            let _ = tx.push_reserved(Cmd::ActionDone { token, outcome });
        }) {
            self.host_close.cancel();
            config::report("worker_error", &format!("job=action reason={reason}"));
        }
    }

    fn on_action_done(&mut self, token: u64, outcome: ActionOutcome, now: Instant) {
        let surf = match self.host_close.resolve(token, now) {
            Resolution::Resolved(surf) => surf,
            Resolution::Overdue(surf) => {
                // Late: the card is not closed, whatever the host says.
                self.host_close_timed_out(surf);
                self.after_change(now);
                return;
            }
            // Stale: the deadline or the card's end already settled it.
            Resolution::Stale => return,
        };
        if outcome == ActionOutcome::Done {
            self.dismiss_card(surf, now);
        } else {
            config::report(
                "host_action_result",
                &format!("surface={surf:?} outcome={outcome:?} card=kept_open"),
            );
            self.after_change(now);
        }
    }

    /// A `HostFlash` region: the check mark or the dimming follows the host's outcome, or a
    /// failure at `Timings::host_action_deadline_ms`.
    fn run_flash(&mut self, activation: Activation, now: Instant) {
        let deadline = config::timings().host_action_deadline_ms;
        let Some(token) = self.flash.begin((), now, deadline) else {
            return;
        };
        let tx = self.tx.clone();
        if let Err(reason) = self.spawn("action", move || {
            let outcome = run_activate(activation);
            let _ = tx.push_reserved(Cmd::FlashDone { token, outcome });
        }) {
            self.flash.cancel();
            config::report("worker_error", &format!("job=action reason={reason}"));
        }
    }

    /// Copy the single card's current payload (`index` is `None`) or item `index`. "Copied", the
    /// close timer and the follow-up reset arm only when the host reports success; a failure or
    /// no answer within `Timings::copy_deadline_ms` shows the failure feedback and keeps the card.
    fn copy_from_clip(&mut self, index: Option<usize>, close_after: bool, now: Instant) {
        let picked = self.with_visual(|v| v.clip.as_ref().and_then(|clip| clip.copy_target(index)));
        let Some((id, text)) = picked else {
            return;
        };
        let key = ClipCopy {
            id: id.clone(),
            close_after,
        };
        let Some(token) = self
            .clip_copy
            .begin(key, now, config::timings().copy_deadline_ms)
        else {
            return; // a copy is already waiting for the host
        };
        let tx = self.tx.clone();
        if let Err(reason) = self.spawn("copy", move || {
            let ok = run_copy(text);
            let _ = tx.push_reserved(Cmd::CopyDone { token, ok });
        }) {
            self.clip_copy.cancel();
            config::report("copy_error", &format!("reason={reason}"));
            self.mark_copy_failed(&id, now);
        }
    }

    fn on_copy_done(&mut self, token: u64, ok: bool, now: Instant) {
        let key = match self.clip_copy.resolve(token, now) {
            Resolution::Resolved(key) => key,
            Resolution::Overdue(key) => {
                // Late: a copy that answered after its deadline is a failure, never "copied".
                self.copy_timed_out(&key, now);
                self.after_change(now);
                return;
            }
            // Stale: expired, or the card was replaced or closed meanwhile.
            Resolution::Stale => return,
        };
        if ok {
            self.mark_copied(&key, now);
        } else {
            config::report("copy_error", "reason=host_reported_failure");
            self.mark_copy_failed(&key.id, now);
        }
        self.after_change(now);
    }

    /// The host confirmed the copy: show "copied" and arm the close (single) or reset (recent)
    /// timer, as before, but from the confirmation.
    fn mark_copied(&mut self, key: &ClipCopy, now: Instant) {
        let close_now = self.with_visual(|v| {
            let ms = v.pill.ms(now);
            let clip = v.clip.as_mut()?;
            if clip.is_leaving() || !clip.items.iter().any(|item| item.id == key.id) {
                return None;
            }
            clip.set_copied(Some(key.id.clone()), ms);
            clip.failed_reset_at = None;
            if clip.single {
                clip.close_at = Some(after(now, motion().close_after_copy_ms));
            } else {
                clip.copied_reset_at = Some(after(now, motion().copied_reset_ms));
            }
            Some(key.close_after)
        });
        if close_now == Some(true) {
            self.dismiss_clip(now);
        }
    }

    /// A copy passed its deadline (the timer found it, or its result arrived late): the failure.
    fn copy_timed_out(&mut self, copy: &ClipCopy, now: Instant) {
        config::report(
            "copy_error",
            &format!(
                "reason=timeout deadline_ms={}",
                config::timings().copy_deadline_ms
            ),
        );
        self.mark_copy_failed(&copy.id, now);
    }

    /// A `HostFlash` action passed its deadline (timer or late result): the failure dimming.
    fn flash_timed_out(&mut self, now: Instant) {
        config::report(
            "host_action_timeout",
            &format!(
                "surface=Pill deadline_ms={}",
                config::timings().host_action_deadline_ms
            ),
        );
        self.with_visual(|v| v.pill.flash_copy(false, now));
    }

    /// A closing `Host` action passed its deadline (timer or late result): the card stays open.
    fn host_close_timed_out(&mut self, surf: Surf) {
        config::report(
            "host_action_timeout",
            &format!(
                "surface={surf:?} deadline_ms={} card=kept_open",
                config::timings().host_action_deadline_ms
            ),
        );
    }

    /// The copy failed or timed out: show the failure feedback for `Timings::copy_failed_hold_ms`
    /// and keep the card (and its timers) as they are.
    fn mark_copy_failed(&mut self, id: &str, now: Instant) {
        self.with_visual(|v| {
            let ms = v.pill.ms(now);
            if let Some(clip) = v.clip.as_mut() {
                if !clip.is_leaving() && clip.items.iter().any(|item| item.id == id) {
                    clip.set_copy_failed(Some(id.to_string()), ms);
                    clip.failed_reset_at = Some(after(now, config::timings().copy_failed_hold_ms));
                    clip.close_at = None;
                }
            }
        });
    }

    // --- hint / clip lifecycle --------------------------------------------------------------------

    fn show_hint(&mut self, content: HintContent, now: Instant) {
        let kind = content.kind.clone();
        let hide_after = content.hide_after_ms;
        self.place_if_idle();
        self.cancel_host_close(Surf::Hint);
        self.with_visual(|v| {
            let ms = v.pill.ms(now);
            let motion = v.pill.animations;
            let hide_at = hide_after.map(|d| after(now, d));
            let mut next = HintState::new(content, &kind, ms, hide_at, motion);
            if let Some(old) = v.hint.as_ref() {
                // A hint replacing a visible one keeps the card and fades the new text in.
                next.take_over(old, ms, motion);
            }
            v.hint = Some(next);
            v.pill.set_hint_visible(true, now);
        });
        self.hover[Surf::Hint as usize].clear();
        self.layout_all(now);
        let handle = &self.handles[Surf::Hint as usize];
        if !self.shown[Surf::Hint as usize] {
            handle.show();
            self.shown[Surf::Hint as usize] = true;
        }
        self.published().hint_shown.store(true, Ordering::Relaxed);
        handle.reassert_topmost();
        config::report("hint_show", &format!("auto_hide={}", hide_after.is_some()));
        self.after_change(now);
    }

    /// Hide the hint: the card plays its entrance spring in reverse, then `finish_hint` takes the
    /// window off screen. The app-visible state (`hint_visible`) clears at once.
    fn dismiss_hint(&mut self, now: Instant) {
        let began = self.with_visual(|v| {
            let ms = v.pill.ms(now);
            let began = match v.hint.as_mut() {
                Some(hint) if !hint.is_leaving() => {
                    hint.begin_exit(ms, now);
                    true
                }
                _ => false,
            };
            v.pill.set_hint_visible(false, now);
            began
        });
        self.published().hint_shown.store(false, Ordering::Relaxed);
        if began {
            config::report("hint_dismiss", "result=ok");
        }
        if self.with_visual(|v| v.hint.is_none()) {
            self.finish_hint(now);
        } else {
            self.after_change(now);
        }
    }

    /// Hide the hint with no exit animation (the clip card is taking its place).
    fn dismiss_hint_instant(&mut self, now: Instant) {
        self.with_visual(|v| {
            v.hint = None;
            v.pill.set_hint_visible(false, now);
        });
        self.finish_hint(now);
    }

    /// The exit animation ended (or there was nothing to animate): drop the hint and its window.
    fn finish_hint(&mut self, now: Instant) {
        self.with_visual(|v| v.hint = None);
        self.cancel_host_close(Surf::Hint);
        let index = Surf::Hint as usize;
        if self.shown[index] {
            self.handles[index].hide();
            self.shown[index] = false;
        }
        self.published().hint_shown.store(false, Ordering::Relaxed);
        self.hover[index].clear();
        self.regions[index].clear();
        self.shared.pump[index].store(false, Ordering::SeqCst);
        self.handles[index].stop_animation();
        self.after_change(now);
    }

    /// Forget a `close_card_after` action in flight for `surf`: its card is going or was
    /// replaced, so a late `Done` must not close whatever shows there next.
    fn cancel_host_close(&mut self, surf: Surf) {
        if self.host_close.key() == Some(&surf) {
            self.host_close.cancel();
        }
    }

    fn show_clip(&mut self, items: Vec<ClipItem>, single: bool, now: Instant) {
        if items.is_empty() {
            self.dismiss_clip(now);
            return;
        }
        // The host decides whether opening the clip card closes a visible hint.
        if config::get().placement.clip_replaces_hint && self.with_visual(|v| v.hint.is_some()) {
            self.dismiss_hint_instant(now);
        }
        self.place_if_idle();
        // A copy still waiting belongs to the card being replaced.
        self.clip_copy.cancel();
        self.cancel_host_close(Surf::Clip);
        self.with_visual(|v| {
            let ms = v.pill.ms(now);
            let mut next = ClipState::new(items, single, ms, now, v.pill.animations);
            if let Some(old) = v.clip.as_ref() {
                // A card replacing a visible one keeps its place and fades the new content in.
                next.take_over(old, ms);
            }
            v.clip = Some(next);
            v.pill.set_clip_visible(true, now);
        });
        self.hover[Surf::Clip as usize].clear();
        self.layout_all(now);
        let handle = &self.handles[Surf::Clip as usize];
        if !self.shown[Surf::Clip as usize] {
            handle.show();
            self.shown[Surf::Clip as usize] = true;
        }
        self.published().clip_shown.store(true, Ordering::Relaxed);
        handle.reassert_topmost();
        // Click-outside watcher: the first look comes `Timings::click_watch_first_ms` after the
        // card opens, so the click that opened it is never mistaken for an outside click.
        self.click_watch_at = Some(after(now, config::timings().click_watch_first_ms));
        self.click_primed = false;
        config::report("clip_show", &format!("single={single}"));
        self.after_change(now);
    }

    /// Hide the clip card: it plays its entrance spring in reverse, then `finish_clip` takes the
    /// window off screen. The app-side holds are released and `clipboard_visible` clears at once.
    fn dismiss_clip(&mut self, now: Instant) {
        let began = self.with_visual(|v| {
            let ms = v.pill.ms(now);
            let began = match v.clip.as_mut() {
                Some(clip) if !clip.is_leaving() => {
                    clip.begin_exit(ms, now);
                    true
                }
                _ => false,
            };
            v.pill.set_clip_visible(false, now);
            began
        });
        self.published().clip_shown.store(false, Ordering::Relaxed);
        self.click_watch_at = None;
        self.click_primed = false;
        // The card is going: a copy still waiting has nothing left to report to.
        self.clip_copy.cancel();
        // Release the app-side holds (override transcript, "pill stays expanded").
        host("clip_dismissed", || {
            (config::get().callbacks.clip_dismissed)()
        });
        if began {
            config::report("clip_dismiss", "reason=requested");
        }
        if self.with_visual(|v| v.clip.is_none()) {
            self.finish_clip(now);
        } else {
            self.after_change(now);
        }
    }

    /// The exit animation ended (or there was nothing to animate): drop the card and its window.
    fn finish_clip(&mut self, now: Instant) {
        self.with_visual(|v| v.clip = None);
        self.clip_copy.cancel();
        self.cancel_host_close(Surf::Clip);
        let index = Surf::Clip as usize;
        if self.shown[index] {
            self.handles[index].hide();
            self.shown[index] = false;
        }
        self.published().clip_shown.store(false, Ordering::Relaxed);
        self.hover[index].clear();
        self.regions[index].clear();
        self.shared.pump[index].store(false, Ordering::SeqCst);
        self.handles[index].stop_animation();
        self.after_change(now);
    }

    // --- frames, pumps and timers -------------------------------------------------------------------

    fn needs_frames(&self, surf: Surf, now: Instant) -> bool {
        self.with_visual(|v| match surf {
            Surf::Pill => v.pill.needs_frames(now),
            Surf::Hint => {
                let ms = v.pill.ms(now);
                v.hint.as_ref().is_some_and(|h| h.needs_frames(ms))
            }
            Surf::Clip => {
                let ms = v.pill.ms(now);
                v.clip.as_ref().is_some_and(|c| c.needs_frames(ms))
            }
        })
    }

    /// Repaint every shown surface and make sure animating ones have a frame pump.
    fn kick(&mut self, now: Instant) {
        for (index, surf) in [Surf::Pill, Surf::Hint, Surf::Clip].into_iter().enumerate() {
            if !self.shown[index] {
                continue;
            }
            if self.needs_frames(surf, now) && !self.shared.pump[index].swap(true, Ordering::SeqCst)
            {
                self.handles[index].animate(gfx::pump());
            }
            self.handles[index].invalidate();
        }
    }

    fn on_pump_idle(&mut self, surf: Surf, now: Instant) {
        let index = surf as usize;
        if self.needs_frames(surf, now) {
            // Something started animating after the painter went idle; keep the pump alive.
            self.shared.pump[index].store(true, Ordering::SeqCst);
            return;
        }
        self.handles[index].stop_animation();
        self.handles[index].invalidate();
    }

    fn after_change(&mut self, now: Instant) {
        self.layout_all(now);
        if self.reconcile_pointer(now) {
            self.layout_all(now);
        }
        self.kick(now);
        // The hovered recording dock re-lays out as the timer digits change width.
        let refresh = self
            .with_visual(|v| v.pill.mode == Mode::Recording && v.pill.hovered && v.pill.visible);
        self.region_refresh_at = if refresh {
            Some(after(now, config::timings().region_refresh_ms))
        } else {
            None
        };
    }

    fn next_deadline(&self) -> Option<Instant> {
        let model = self.with_visual(|v| {
            [
                v.pill.next_deadline(),
                v.hint.as_ref().and_then(|h| h.hide_at),
                v.hint.as_ref().and_then(|h| h.exit_at),
                v.clip.as_ref().and_then(ClipState::next_deadline),
            ]
        });
        model
            .into_iter()
            .flatten()
            .chain(self.region_refresh_at)
            .chain(self.click_watch_at)
            .chain(self.rwatch_at)
            .chain(self.clip_copy.deadline())
            .chain(self.flash.deadline())
            .chain(self.host_close.deadline())
            .min()
    }

    fn on_timers(&mut self, now: Instant) {
        match self.next_deadline() {
            Some(deadline) if now >= deadline => {}
            _ => return,
        }
        struct Due {
            effects: Vec<Effect>,
            hint: bool,
            hint_exit: bool,
            clip_close: bool,
            clip_exit: bool,
        }
        let due = self.with_visual(|v| {
            let effects = v.pill.on_timers(now);
            let hint = v
                .hint
                .as_ref()
                .and_then(|h| h.hide_at)
                .is_some_and(|t| now >= t);
            let hint_exit = v
                .hint
                .as_ref()
                .and_then(|h| h.exit_at)
                .is_some_and(|t| now >= t);
            let ms = v.pill.ms(now);
            let mut clip_close = false;
            let mut clip_exit = false;
            if let Some(clip) = v.clip.as_mut() {
                if clip.autodismiss_at.is_some_and(|t| now >= t) {
                    clip.autodismiss_at = None;
                    clip_close = true;
                }
                if clip.close_at.is_some_and(|t| now >= t) {
                    clip.close_at = None;
                    clip_close = true;
                }
                if clip.copied_reset_at.is_some_and(|t| now >= t) {
                    clip.copied_reset_at = None;
                    clip.set_copied(None, ms);
                }
                if clip.failed_reset_at.is_some_and(|t| now >= t) {
                    clip.failed_reset_at = None;
                    clip.set_copy_failed(None, ms);
                }
                if clip.exit_at.is_some_and(|t| now >= t) {
                    clip.exit_at = None;
                    clip_exit = true;
                }
            }
            Due {
                effects,
                hint,
                hint_exit,
                clip_close,
                clip_exit,
            }
        });
        self.run_effects(due.effects, now);
        // Host work that did not answer in time has failed; its late answer is stale.
        if let Some(copy) = self.clip_copy.expire(now) {
            self.copy_timed_out(&copy, now);
        }
        if self.flash.expire(now).is_some() {
            self.flash_timed_out(now);
        }
        if let Some(surf) = self.host_close.expire(now) {
            self.host_close_timed_out(surf);
        }
        if due.hint {
            self.dismiss_hint(now);
        }
        if due.hint_exit {
            self.finish_hint(now);
        }
        if due.clip_close {
            self.dismiss_clip(now);
        }
        if due.clip_exit {
            self.finish_clip(now);
        }
        if self.click_watch_at.is_some_and(|t| now >= t) {
            self.click_watch_tick(now);
        }
        if self.rwatch_at.is_some_and(|t| now >= t) {
            self.rwatch_tick(now);
        }
        if self.region_refresh_at.is_some_and(|t| now >= t) {
            self.region_refresh_at = None;
        }
        // Timer-driven model changes (hover exit, status expiry, fade end) move the capsule
        // target, so frames and regions are recomputed once per wake.
        self.after_change(now);
    }

    /// Run the secondary action on a fresh right-button press while the pointer rests on a
    /// region that has one (the overlay reports left clicks only).
    fn rwatch_tick(&mut self, now: Instant) {
        let down = unsafe { GetAsyncKeyState(0x02) } < 0;
        let fresh = down && !self.rprev_down;
        self.rprev_down = down;
        if fresh {
            if let Some(region) = self.rwatch_region.clone() {
                config::report(
                    "action",
                    &format!("surface=Pill region={} phase=right_click", region.id.0),
                );
                if let Some(tag) = region.secondary {
                    host("activate_secondary", || {
                        (config::get().callbacks.activate_secondary)(Activation {
                            surface: Surf::Pill,
                            region: region.id,
                            tag,
                        })
                    });
                }
            }
        }
        self.rwatch_at = Some(after(now, config::timings().pointer_poll_ms));
    }

    /// Dismiss the clip when the primary button goes down outside both the card and the pill
    /// (the Windows overlays never take focus, so there is no focus-loss event to use).
    fn click_watch_tick(&mut self, now: Instant) {
        let down = unsafe { GetAsyncKeyState(0x01) } < 0;
        if !self.click_primed {
            self.click_primed = true;
            self.prev_down = down;
            self.click_watch_at = Some(after(now, config::timings().pointer_poll_ms));
            return;
        }
        let fresh = down && !self.prev_down;
        self.prev_down = down;
        if fresh {
            if let Some((cx, cy)) = gfx::cursor_pos() {
                let scale = f64::from(self.placement.scale);
                let inside_clip = self.frames[Surf::Clip as usize].is_some_and(|(x, y, w, h)| {
                    let (px, py) = (f64::from(cx), f64::from(cy));
                    px >= f64::from(x)
                        && px <= f64::from(x) + f64::from(w) * scale
                        && py >= f64::from(y)
                        && py <= f64::from(y) + f64::from(h) * scale
                });
                let inside_pill = self.capsule_phys.is_some_and(|(x, y, w, h)| {
                    let (px, py) = (f64::from(cx), f64::from(cy));
                    px >= x && px <= x + w && py >= y && py <= y + h
                });
                if !inside_clip && !inside_pill {
                    config::report("clip_dismiss", "reason=outside_click");
                    self.dismiss_clip(now);
                    return;
                }
            }
        }
        self.click_watch_at = Some(after(now, config::timings().pointer_poll_ms));
    }
}
