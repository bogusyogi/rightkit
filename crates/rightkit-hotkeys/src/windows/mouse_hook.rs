//! Mouse half of the process-wide low-level input hooks: middle-click toggle that leaves
//! hyperlinks alone.
//!
//! A `WH_MOUSE_LL` handler watches the middle-button press and HOLDS it (swallows it) so Windows
//! never starts autoscroll, while a worker decides what is under the pointer: a hyperlink gets the
//! click back, a confirmed non-link runs the host's toggle callback. A Windows low-level hook proc
//! must return synchronously (a hook that does not is removed by the system without notice), so
//! everything that can wait on another process (the window hit test, UI Automation) or on host code
//! (the toggle callback, the host's event sink) runs off the hook thread, behind a deadline, a cap
//! or its own thread.
//!
//! Inside the host's own process, pointer-styled controls must not be mistaken for links, so the
//! hand-cursor fallback is used only for other processes; UI Automation hyperlink evidence stays
//! authoritative everywhere.
//!
//! # Held click outcomes
//!
//! A click the hook holds ends in exactly ONE of three outcomes, on every path that reaches the
//! worker (a panic is settled by `PendingClick::drop`, reported as `settle_failed` if settling itself
//! panics); the decision table and the exact effect sequence are the pure `held_click`
//! module:
//!
//! 1. DELIVERED: replayed at the ORIGINAL press point as ONE `SendInput` batch (absolute move to the
//!    press point, middle down, middle up, absolute move back), so the pointer cannot drift between
//!    the position and the click; the window under that point was re-resolved and matched the one
//!    identified at pickup immediately before the batch.
//! 2. CONSUMED: the host toggle was handed to its own lane (confirmed non-link only).
//! 3. DROPPED: neither. The click is never sent to a window other than the one identified at pickup.
//!    The drop is queued for the host's sink as `middle_mouse_win_dropped` with the reason
//!    (`target_changed`, `target_unverifiable`, `pickup_too_late`, `position_unmappable`,
//!    `toggle_busy`, `send_rejected`, `button_stuck`, `settle_failed`). The report lane holds
//!    [`REPORT_DEPTH`] lines; past that a report is lost, but the loss is counted and reported once
//!    (`middle_mouse_report_dropped`), so a drop is never silent while the report thread runs.
//!
//! A press the hook cannot hold is not held at all and passes untouched (nothing is eaten): queue
//! full, worker gone or retired, lock busy, a held click still in flight, the install ended, or the
//! identity facility saturated by calls that are past their deadline and never returned.
//!
//! Decisions:
//! - Target identity is captured by the WORKER at pickup. Only `WindowFromPoint` can wait on another
//!   process (the window's own thread answers the hit test), so only it runs on a detached thread
//!   behind the identity deadline (`MiddleClickTiming::identity_deadline_ms`); a call counts against
//!   [`MAX_IDENTITY_CALLS_IN_FLIGHT`] only once it is past that deadline. `GetAncestor`,
//!   `GetWindowThreadProcessId`, `OpenProcess` and `GetProcessTimes` read the window tree or query a
//!   process object and do not depend on the target's UI thread; they run on the worker (HANDOFF.md,
//!   "Identity capture"). A capture that cannot complete, finds no window or cannot read the process
//!   creation time (typically an elevated or protected target) is `target_unverifiable`: the click is
//!   dropped, because it cannot be shown to go to the window under the press. A first capture that
//!   ends later than `MiddleClickTiming::max_pickup_age_ms` after the press is not attributable to
//!   the press: `pickup_too_late`. Before ANY injection or toggle the window at the original point
//!   is re-resolved (same deadline) and compared (hwnd, root, pid, creation time); the toggle is
//!   gated too.
//! - The pointer is NOT moved and read back. The replay batch carries its own absolute move, so
//!   position and click are atomic to the input queue. The press point must lie on the virtual
//!   desktop (`position_unmappable` otherwise); the metrics and the absolute coordinates are read
//!   BEFORE the target check (`ClickEnv::prepare`), so between the check and `SendInput` only the
//!   `POINTER_EFFECTS` lock, one `GetCursorPos` and the library's conversion of four events remain.
//!   The pointer is put back where it was read just before the batch; motion the user made between
//!   that read and the batch is not preserved.
//! - Lookup is tri-state (`Link`, `NotLink`, `Unknown`) and comes from
//!   `rightkit_automation::win::uia::point_link_state` (`rightkit-automation` 0.7.1 or later):
//!   `NotLink` only when every control up to the desktop root was readable. Only a confirmed
//!   `NotLink` toggles. `Unknown` (also: timeout after [`RESOLVE_TIMEOUT`], resolver refused,
//!   exhausted or died, shutdown) REPLAYS. The hand cursor, read on the hook thread at press time,
//!   is a fallback for UI Automation under-reporting in OTHER processes.
//! - The toggle also requires the feature to still be enabled when the click is settled: a click
//!   held before `set_enabled(false)` is delivered instead of toggling.
//! - Host callback: runs on its own lane thread (`rk-middle-click-toggle`), never on the queue
//!   worker. At most one callback runs, across installs. A confirmed non-link that arrives while one
//!   is running (under [`TOGGLE_STALL_AFTER`]) is DROPPED and reported (`toggle_busy`); past the
//!   stall limit the host toggle is treated as unavailable and non-link presses are replayed.
//! - Reports to the host's sink go through their own bounded lane and thread
//!   (`rk-middle-click-report`), so a slow sink cannot delay a queued press.
//! - button-up before resolution: the real up is swallowed with its down (the replay carries its own
//!   up). The debt is scoped to the install that swallowed the down (`lifecycle::UpOwed`).
//!
//! # Hook thread (HANDOFF.md, "Hook-thread calls")
//!
//! [`MiddleClickState::handle_mouse`] runs inside the `WH_MOUSE_LL` procedure. For a middle press of
//! a live install it does: atomic loads, one `GetCursorInfo` (a read of the global cursor state; its
//! result is compared with an `IDC_HAND` handle loaded when the worker started), `Instant::now`, one
//! `try_lock` of the worker slot and one bounded `try_send`. It records only the point and the
//! cursor shape. It calls no `WindowFromPoint`, no `GetAncestor`, no `GetWindowThreadProcessId`, no
//! `OpenProcess`, no UI Automation, no `SendInput`, no host code, and takes no blocking lock. For
//! every other message: an enum compare. After the install ended it only settles an owed button-up.
//!
//! # Lifecycle (HANDOFF.md, "Lifecycle")
//!
//! One `MiddleClickState::start_worker` begins one generation (`lifecycle::Liveness`): the queue
//! worker, the toggle lane thread, the report thread and every resolver it starts belong to it.
//! `MiddleClickState::quiesce` (first step of `InputHooks`' teardown) stops the hook from holding
//! new presses; `MiddleClickState::stop_worker` ends the generation and closes the queue; both are
//! non-blocking, and nothing is joined. `MiddleClickState::on_hook_removed` drops the owed
//! button-up once the hook thread is gone.
//! - Held clicks still queued, or in flight, are settled without resolving: replayed if the target
//!   still verifies, otherwise dropped and reported. A settle that is blocked inside `SendInput`,
//!   `GetCursorPos`, `GetSystemMetrics` or the `POINTER_EFFECTS` lock is not interrupted: the click
//!   then ends when that call returns, and no further press is held meanwhile (`outstanding`).
//! - A resolver blocked in a UI Automation call is detached (at most [`MAX_RESOLVERS_IN_FLIGHT`] in
//!   the process); its late answer is dropped by the thread itself.
//! - The toggle lane ends when the worker ends, after the callback it is running, if any, returns.
//! - A second install of the same state while one is live is refused.
//! - Worker panic or exit: the worker takes itself out of the state under the same lock the hook's
//!   `try_send` runs under, THEN drains the queue, so a click queued by the hook is either seen by
//!   the drain or never accepted. A drop guard exists from the moment a click is received, so a
//!   panic anywhere after that still settles it.
//!
//! Replays are tagged (`dwExtraInfo == injected_tag` plus the injected flag) and recognised by
//! [`MiddleClickState::handle_mouse`], which passes them through untouched.
//!
//! Known limits (not fixable inside the hook): the window can still change between the last
//! identity check and `SendInput` (the lock, one `GetCursorPos`, the batch conversion); a replay is
//! injected, so Windows (UIPI) rejects it for an elevated or secure-desktop target (reported as
//! `send_rejected` or `button_stuck`); the host process must be per-monitor DPI aware for the
//! physical-pixel press point to map onto the virtual desktop; if the hook is removed while the user
//! holds the button, the real up reaches the application after the replayed click (reported once as
//! `middle_mouse_win_error`).

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use rightkit_automation::win::uia::{point_link_state, PointLinkState};
use rightkit_automation::win::{
    send_input_batch_with, RawInput, RawMouse, RawMouseButton, SendOptions,
};
use rightkit_automation::Error as AutomationError;
use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, POINT};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetCursorInfo, GetCursorPos, GetSystemMetrics, GetWindowThreadProcessId,
    LoadCursorW, WindowFromPoint, CURSORINFO, GA_ROOT, IDC_HAND, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use super::ptt_hook::EventSink;
use super::HookDecision;
use crate::held_click::{
    absolute_coord, classify, compare_target, gate_step, recovery_for, settle, ClickEnv,
    ClickOutcome, ClickSend, DesktopRect, DropReason, GateAction, LaneState, Lookup, Resolution,
    TargetCheck, TargetIdentity, UnknownCause,
};
use crate::hook_detail::{MouseEvent, MouseKind};
use crate::lifecycle::{
    bounded_call, lock, lookup_from_wait, run_toggle_lane, start_resolver, wait_interruptible,
    HungCallCap, Lane, Liveness, ResolverCap, ToggleGuard, UpOwed, WAIT_SLICE,
};

/// The host's toggle callback; runs on the toggle lane thread, never on the hook thread or the
/// queue worker.
pub type Action = Arc<dyn Fn() + Send + Sync + 'static>;

/// How long the worker waits for the hyperlink answer before the lookup counts as unknown.
pub const RESOLVE_TIMEOUT: Duration = Duration::from_millis(400);
/// A host toggle callback that has run this long is "stalled": the lane is treated as
/// unavailable and non-link presses are replayed instead of dropped as busy.
pub const TOGGLE_STALL_AFTER: Duration = Duration::from_millis(1500);
/// Ancestors the hyperlink probe climbs while looking for a link AND for the desktop root. A
/// `NotLink` needs the walk to reach the root inside this budget, so it is generous; a deeper tree
/// answers `Unknown` (the click is replayed). The walk is bounded in time by [`RESOLVE_TIMEOUT`].
pub const LINK_WALK_ANCESTORS: usize = 32;
/// Resolver threads that may be alive in the PROCESS at once (a blocked UI Automation call keeps
/// its thread, so install/uninstall cycles must not accumulate them).
pub const MAX_RESOLVERS_IN_FLIGHT: usize = 2;
/// Window hit tests (`WindowFromPoint`) that may be PAST their deadline and not yet returned, in the
/// PROCESS. A hit test still inside its deadline does not count. While this many are hung the hook
/// holds no click (it passes), so nothing is eaten and no further hit test is started.
pub const MAX_IDENTITY_CALLS_IN_FLIGHT: usize = 2;

/// Default for [`MiddleClickTiming::identity_deadline_ms`]. Unmeasured; chosen to equal
/// [`RESOLVE_TIMEOUT`]: the hold the code already grants the hyperlink lookup, so a slow target
/// costs at most the same wait whichever of the two is slow. Before the hit test moved off the hook
/// thread it was bounded only by the system's low-level-hook timeout (a registry value this code
/// does not read); a target that answered inside that bound and outside this one is now a loss.
pub const DEFAULT_IDENTITY_DEADLINE_MS: u64 = 400;
/// Default for [`MiddleClickTiming::max_pickup_age_ms`]. Unmeasured; the identity deadline plus
/// 100 ms for the worker to pick the press up.
pub const DEFAULT_MAX_PICKUP_AGE_MS: u64 = 500;
/// Accepted range of [`MiddleClickTiming::identity_deadline_ms`]; values outside are clamped.
pub const IDENTITY_DEADLINE_RANGE_MS: (u64, u64) = (20, 2_000);
/// Accepted range of [`MiddleClickTiming::max_pickup_age_ms`]; values outside are clamped, and a
/// value below the identity deadline is raised to it (a capture that used its whole deadline would
/// otherwise always be late).
pub const PICKUP_AGE_RANGE_MS: (u64, u64) = (50, 5_000);

/// Timing the host chooses for held clicks. Every value is clamped to its documented range when the
/// state is built ([`MiddleClickState::with_timing`]); nothing here can be unbounded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MiddleClickTiming {
    /// The longest one window hit test (`WindowFromPoint`, the only identity call that can wait on
    /// the target's own thread) may take before the identity is unverifiable
    /// (`target_unverifiable`). Range [`IDENTITY_DEADLINE_RANGE_MS`].
    pub identity_deadline_ms: u64,
    /// The longest between the press and the end of the first identity capture. Later, the window
    /// found is not attributable to the press (`pickup_too_late`). Range [`PICKUP_AGE_RANGE_MS`].
    pub max_pickup_age_ms: u64,
}

impl MiddleClickTiming {
    pub const DEFAULT: Self = Self {
        identity_deadline_ms: DEFAULT_IDENTITY_DEADLINE_MS,
        max_pickup_age_ms: DEFAULT_MAX_PICKUP_AGE_MS,
    };

    const fn clamp(v: u64, range: (u64, u64)) -> u64 {
        if v < range.0 {
            range.0
        } else if v > range.1 {
            range.1
        } else {
            v
        }
    }

    /// The same timing with every value inside its range.
    pub const fn clamped(self) -> Self {
        let identity = Self::clamp(self.identity_deadline_ms, IDENTITY_DEADLINE_RANGE_MS);
        let age = Self::clamp(self.max_pickup_age_ms, PICKUP_AGE_RANGE_MS);
        Self {
            identity_deadline_ms: identity,
            max_pickup_age_ms: if age < identity { identity } else { age },
        }
    }
}

/// Held clicks that may wait for the worker. The hook holds a new press only while none is
/// outstanding, so at most one is ever queued; the slack is a safety margin.
const QUEUE_DEPTH: usize = 2;
/// Reports that may wait for the host's sink; further ones are dropped (counted, reported once).
pub const REPORT_DEPTH: usize = 32;

static RESOLVERS: ResolverCap = ResolverCap::new(MAX_RESOLVERS_IN_FLIGHT);
static IDENTITY: HungCallCap = HungCallCap::new(MAX_IDENTITY_CALLS_IN_FLIGHT);
/// Pointer effects of every generation take turns: an old worker draining and a new one never
/// interleave their batches or their pointer reads.
static POINTER_EFFECTS: Mutex<()> = Mutex::new(());
/// The shared `IDC_HAND` cursor handle, loaded off the hook thread when a worker starts.
static HAND_CURSOR: OnceLock<usize> = OnceLock::new();

/// Why `MiddleClickState::start_worker` did not start.
#[derive(Debug)]
pub(crate) enum StartError {
    /// A worker of this state is still live (one install per state object).
    AlreadyRunning,
    /// A thread could not be created.
    Spawn(String),
}

/// One held press: where the user pressed, the cursor shape at that instant (the only thing read on
/// the hook thread besides the point), when, and the window identified for it (by the worker).
#[derive(Clone, Copy, Debug)]
struct HeldClick {
    x: i32,
    y: i32,
    hand_cursor: bool,
    pressed_at: Instant,
    target: Option<TargetIdentity>,
}

/// A line for the host's sink, queued off the worker.
struct Report {
    event: &'static str,
    message: String,
}

/// What every thread of one generation shares.
struct Generation {
    live: Arc<Liveness>,
    /// Held clicks accepted by the hook and not yet settled: the hook holds a new press only at 0.
    outstanding: AtomicUsize,
    reports: Lane<Report>,
}

impl Generation {
    fn report(&self, outcome: ClickOutcome) {
        if let Some(report) = report_for(outcome) {
            self.reports.send(report);
        }
    }
}

/// The live generation's queue. Dropping it (with `live` ended) lets the worker drain and exit.
struct Worker {
    tx: SyncSender<HeldClick>,
    epoch: Arc<Generation>,
}

/// State of the middle-click hook. `const`-constructible for a host `static`.
pub struct MiddleClickState {
    enabled: AtomicBool,
    /// The install's authority ended (`quiesce`, `stop_worker`, a retired worker): the hook holds no
    /// new press and only settles an owed button-up. Cleared by `start_worker`.
    ended: AtomicBool,
    /// Id of the last started generation (0 = none): scopes `up_owed`, read on the hook thread. It
    /// is kept after the generation ended, until the hook thread is gone (`on_hook_removed`), so a
    /// button-up still owed to an ended generation is still recognised.
    session: AtomicU64,
    /// A held press has not yet had its button-up swallowed (scoped to `session`).
    up_owed: UpOwed,
    toggle: OnceLock<Action>,
    /// At most one host toggle callback at a time, across installs.
    toggle_guard: ToggleGuard,
    worker: Mutex<Option<Worker>>,
    /// Host telemetry sink for dropped clicks; attached by `InputHooks::install`. A plain function
    /// pointer (configuration, not install state), first one wins.
    event_sink: OnceLock<EventSink>,
    /// Signature stamped into `dwExtraInfo` of the click replayed to the application, so the
    /// hook recognizes its own synthetic event and passes it through instead of re-holding it
    /// (which would eat the click and loop forever). Chosen by the host.
    injected_tag: usize,
    /// Longest one window hit test may take (`MiddleClickTiming::identity_deadline_ms`, clamped).
    identity_deadline: Duration,
    /// Longest between the press and the end of the first identity capture (clamped).
    max_pickup_age: Duration,
}

impl MiddleClickState {
    /// A state with the default timing ([`MiddleClickTiming::DEFAULT`]).
    pub const fn new(injected_tag: usize) -> Self {
        Self::with_timing(injected_tag, MiddleClickTiming::DEFAULT)
    }

    /// A state with the host's timing, every value clamped to its documented range.
    pub const fn with_timing(injected_tag: usize, timing: MiddleClickTiming) -> Self {
        let timing = timing.clamped();
        Self {
            enabled: AtomicBool::new(false),
            ended: AtomicBool::new(true),
            session: AtomicU64::new(0),
            up_owed: UpOwed::new(),
            toggle: OnceLock::new(),
            toggle_guard: ToggleGuard::new(),
            worker: Mutex::new(None),
            event_sink: OnceLock::new(),
            injected_tag,
            identity_deadline: Duration::from_millis(timing.identity_deadline_ms),
            max_pickup_age: Duration::from_millis(timing.max_pickup_age_ms),
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
    }

    /// First callback set wins, as before.
    pub fn set_toggle_callback(&self, cb: Action) {
        let _ = self.toggle.set(cb);
    }

    /// Called by `InputHooks::install` with the host's sink; first one wins.
    pub(crate) fn attach_event_sink(&self, sink: EventSink) {
        let _ = self.event_sink.set(sink);
    }

    /// Begin a generation: the report thread, the toggle lane and the resolver worker. Called by
    /// `InputHooks::install` before the hook is installed. Without a worker no click is ever held:
    /// the hook passes every click. Refused while a worker of this state is live.
    pub(crate) fn start_worker(&'static self) -> Result<(), StartError> {
        let mut slot = lock(&self.worker);
        if slot.is_some() {
            return Err(StartError::AlreadyRunning);
        }
        // Loaded here, off the hook thread, so the hook only compares handles.
        HAND_CURSOR.get_or_init(|| unsafe { LoadCursorW(std::ptr::null_mut(), IDC_HAND) as usize });

        let (reports, report_rx) = Lane::<Report>::with_depth(REPORT_DEPTH);
        let epoch = Arc::new(Generation {
            live: Liveness::begin(),
            outstanding: AtomicUsize::new(0),
            reports,
        });
        // The report thread reads the lane's drop count, so it holds the generation: the lane ends
        // by its close marker (`DrainOnDrop::drop`, or the failure paths below), not by the
        // generation being dropped.
        let report_gen = Arc::clone(&epoch);
        thread::Builder::new()
            .name("rk-middle-click-report".to_string())
            .spawn(move || {
                // A panicking sink must not end the thread.
                let deliver = |event: &str, message: &str| {
                    if let Some(sink) = self.event_sink.get() {
                        let _ = catch_unwind(AssertUnwindSafe(|| sink(event, message)));
                    }
                };
                let overflow = || {
                    let dropped = report_gen.reports.take_dropped();
                    if dropped > 0 {
                        deliver("middle_mouse_report_dropped", &format!("dropped={dropped}"));
                    }
                };
                while let Ok(report) = report_rx.recv() {
                    deliver(report.event, &report.message);
                    overflow();
                }
                // Anything dropped after the last delivery.
                overflow();
            })
            .map_err(|_| {
                StartError::Spawn("middle-click report thread spawn failed".to_string())
            })?;

        let (toggle_tx, toggle_rx) = mpsc::channel::<()>();
        if thread::Builder::new()
            .name("rk-middle-click-toggle".to_string())
            .spawn(move || {
                run_toggle_lane(toggle_rx, &self.toggle_guard, || {
                    if let Some(cb) = self.toggle.get() {
                        // A panicking host callback must not kill the lane.
                        let _ = catch_unwind(AssertUnwindSafe(|| cb()));
                    }
                })
            })
            .is_err()
        {
            epoch.reports.close();
            return Err(StartError::Spawn(
                "middle-click toggle lane spawn failed".to_string(),
            ));
        }
        let lane = ToggleLane {
            tx: toggle_tx,
            live: Arc::clone(&epoch.live),
            state: self,
        };

        let (tx, rx) = mpsc::sync_channel::<HeldClick>(QUEUE_DEPTH);
        let worker_gen = Arc::clone(&epoch);
        // If this spawn fails the closure (and with it the lane sender) is dropped, which ends
        // the toggle lane thread; the report lane is closed here.
        if thread::Builder::new()
            .name("rk-middle-click".to_string())
            .spawn(move || run_worker(self, rx, worker_gen, lane))
            .is_err()
        {
            epoch.reports.close();
            return Err(StartError::Spawn(
                "middle-click worker spawn failed".to_string(),
            ));
        }

        self.up_owed.clear();
        self.session.store(epoch.live.id(), Ordering::SeqCst);
        self.ended.store(false, Ordering::SeqCst);
        *slot = Some(Worker { tx, epoch });
        Ok(())
    }

    /// End the install's authority (first step of `InputHooks`' teardown, before the hook is
    /// removed): the hook holds no new press and passes every event except an owed button-up. A
    /// click already held is still settled by the worker. Non-blocking. Idempotent.
    pub(crate) fn quiesce(&self) {
        self.ended.store(true, Ordering::SeqCst);
    }

    /// End the generation. Call AFTER the hook is removed (or after `quiesce`): no new click can
    /// be held, and the worker settles every click it still holds without resolving it, then exits
    /// (which ends the toggle lane once its running callback, if any, returns). Returns at once; no
    /// thread is joined. Idempotent. The owed button-up is NOT dropped here: while the hook may
    /// still be installed it is still swallowed (`on_hook_removed` drops it).
    pub(crate) fn stop_worker(&self) {
        self.ended.store(true, Ordering::SeqCst);
        let worker = lock(&self.worker).take();
        if let Some(worker) = worker {
            // End first so everything in flight sees it, then drop the only sender.
            worker.epoch.live.end();
            drop(worker);
        }
    }

    /// The hook thread is gone: nothing can see the release of a middle press any more, so the owed
    /// button-up is dropped; `true` when one was owed (it reaches the application without the press
    /// the hook swallowed, after the replayed click). Changes state only and calls no host code:
    /// the caller reports it (`input_hooks::Removal::drop`, off the drop path). Called by
    /// `InputHooks` after the hook thread ended, from a non-hook thread.
    pub(crate) fn on_hook_removed(&self) -> bool {
        let session = self.session.swap(0, Ordering::SeqCst);
        let owed = session != 0 && self.up_owed.is_owed(session);
        self.up_owed.clear();
        owed
    }

    /// The worker leaves the state (it is exiting, normally or by panic). Done under the same lock
    /// the hook's `try_send` runs under, so every click the hook queued is already in the channel
    /// and no further one is accepted: the drain that follows sees them all. A no-op when
    /// `stop_worker` or a newer generation already replaced the slot. The owed button-up stays (the
    /// hook is still installed).
    fn retire_worker(&self, id: u64) {
        let worker = {
            let mut slot = lock(&self.worker);
            if slot.as_ref().is_some_and(|w| w.epoch.live.id() == id) {
                slot.take()
            } else {
                None
            }
        };
        if let Some(worker) = worker {
            self.ended.store(true, Ordering::SeqCst);
            worker.epoch.live.end();
        }
    }

    /// Queue a press for the worker without waiting. `false` means it was not queued (and the
    /// press passes untouched). Hook-thread path: no call into another process, no blocking lock.
    fn try_hold(&self, event: &MouseEvent) -> bool {
        // Fail open while every identity capture slot is held by a call that never returned: the
        // worker could not identify anything, so holding would only eat the click.
        if !IDENTITY.has_free_slot() {
            return false;
        }
        // Capture the cursor SHAPE at click time, on the hook thread (GetCursorInfo is a read of
        // global cursor state). Browsers show the shared system hand cursor (IDC_HAND) over links,
        // and this works even when the browser's UIA accessibility tree is dormant: Chromium/Edge
        // keep it OFF until an assistive client attaches, which made ElementFromPoint return the
        // document pane instead of the Hyperlink (the "middle-click on a link started dictation
        // and ate the click" bug).
        let hand_cursor = hook_hand_cursor();
        let job = HeldClick {
            x: event.x,
            y: event.y,
            hand_cursor,
            pressed_at: Instant::now(),
            target: None,
        };
        // try_lock: the lock is taken only by start/stop/retire; if it is busy, do not wait, pass.
        match self.worker.try_lock() {
            Ok(slot) => match slot.as_ref() {
                // One held click at a time: a press while one is outstanding passes (the
                // application gets a normal middle click) instead of waiting behind it.
                Some(worker) if worker.epoch.outstanding.load(Ordering::SeqCst) == 0 => {
                    worker.epoch.outstanding.fetch_add(1, Ordering::SeqCst);
                    if worker.tx.try_send(job).is_ok() {
                        true
                    } else {
                        worker.epoch.outstanding.fetch_sub(1, Ordering::SeqCst);
                        false
                    }
                }
                _ => false,
            },
            Err(_) => false,
        }
    }

    /// `WH_MOUSE_LL` handler body. Called by the hook procedure for every mouse event; returns at
    /// once for everything but the middle button.
    pub fn handle_mouse(&'static self, event: &MouseEvent) -> HookDecision {
        if !matches!(event.kind, MouseKind::MiddleDown | MouseKind::MiddleUp) {
            return HookDecision::Pass;
        }
        // Our own replay (injected AND carrying the host's tag): pass it straight through so the
        // application gets the click; never re-hold it or it loops.
        let ours = event.injected && event.extra_info == self.injected_tag;
        // The install ended (checked first): hold nothing new. The one thing still done is to
        // swallow the release of a press this hook swallowed while the install was live.
        if !ours && self.ended.load(Ordering::SeqCst) {
            let session = self.session.load(Ordering::SeqCst);
            if event.kind == MouseKind::MiddleUp && self.up_owed.is_owed(session) {
                self.up_owed.apply(session, false);
                return HookDecision::Swallow;
            }
            return HookDecision::Pass;
        }
        let enabled = self.enabled.load(Ordering::Relaxed);
        let session = self.session.load(Ordering::Relaxed);
        let owed = self.up_owed.is_owed(session);
        let (action, next_owed) =
            gate_step(event.kind, ours, enabled, owed, || self.try_hold(event));
        if next_owed != owed {
            self.up_owed.apply(session, next_owed);
        }
        match action {
            GateAction::Pass => HookDecision::Pass,
            GateAction::Hold | GateAction::SwallowUp => HookDecision::Swallow,
        }
    }

    /// Settle one held click with the real effects (see [`crate::held_click::settle`]). Runs on the
    /// worker or a drain path. The click's window is identified here if the worker has not already
    /// (drain paths); an identity that cannot be attributed to the press drops the click.
    fn settle_job(
        &'static self,
        job: &HeldClick,
        resolution: Resolution,
        shutting_down: bool,
        lane: Option<&ToggleLane>,
    ) -> ClickOutcome {
        let mut job = *job;
        if let Err(reason) = attribute_target(&mut job, self.identity_deadline, self.max_pickup_age)
        {
            return ClickOutcome::Dropped(reason);
        }
        let Some(target) = job.target else {
            return ClickOutcome::Dropped(DropReason::TargetUnverifiable);
        };
        let mut env = WinEnv {
            target,
            x: job.x,
            y: job.y,
            tag: self.injected_tag,
            deadline: self.identity_deadline,
            plan: None,
            lane,
        };
        // The toggle needs the feature to be on NOW, not only when the press was held.
        let has_toggle = self.toggle.get().is_some() && self.enabled.load(Ordering::SeqCst);
        settle(
            &mut env,
            (job.x, job.y),
            resolution,
            shutting_down,
            has_toggle,
        )
    }
}

fn report_for(outcome: ClickOutcome) -> Option<Report> {
    match outcome {
        ClickOutcome::Dropped(reason) => Some(Report {
            event: "middle_mouse_win_dropped",
            message: format!("reason={}", reason.as_str()),
        }),
        ClickOutcome::Delivered {
            pointer_restored: false,
            ..
        } => Some(Report {
            event: "middle_mouse_win_error",
            message: "click delivered; pointer could not be restored".to_string(),
        }),
        ClickOutcome::Delivered {
            host_toggle_unavailable: true,
            ..
        } => Some(Report {
            event: "middle_mouse_win_error",
            message: "host toggle callback stalled or unavailable; click delivered instead"
                .to_string(),
        }),
        ClickOutcome::Delivered { .. } | ClickOutcome::Toggled => None,
    }
}

/// `true` when the cursor at press time is the shared system hand cursor. Hook-thread path.
fn hook_hand_cursor() -> bool {
    let Some(hand) = HAND_CURSOR.get().copied() else {
        return false;
    };
    unsafe {
        let mut ci = CURSORINFO {
            cbSize: std::mem::size_of::<CURSORINFO>() as u32,
            flags: 0,
            hCursor: std::ptr::null_mut(),
            ptScreenPos: POINT { x: 0, y: 0 },
        };
        GetCursorInfo(&mut ci) != 0 && !ci.hCursor.is_null() && ci.hCursor as usize == hand
    }
}

// ---------------------------------------------------------------------------------------------
// Toggle lane: the host callback runs here, never on the queue worker
// ---------------------------------------------------------------------------------------------

/// The worker's handle to the toggle lane thread of ITS generation. Dropping it (the worker
/// exiting) ends the lane thread once its running callback returns.
struct ToggleLane {
    tx: Sender<()>,
    live: Arc<Liveness>,
    state: &'static MiddleClickState,
}

impl ToggleLane {
    fn state(&self) -> LaneState {
        self.state
            .toggle_guard
            .state_at(Instant::now(), TOGGLE_STALL_AFTER)
    }

    /// Hand one toggle to the lane without waiting. `false` when the generation has ended (no
    /// toggle is accepted after uninstall), when a callback is still marked running, or when the
    /// lane thread is gone; the caller then replays.
    fn submit(&self) -> bool {
        if !self.live.is_live() {
            return false;
        }
        let guard = &self.state.toggle_guard;
        if !guard.begin_if_idle(Instant::now()) {
            return false;
        }
        if self.tx.send(()).is_ok() {
            true
        } else {
            guard.end();
            false
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------------------------

/// Owns one received click until it is settled. From the moment the worker has the click (before
/// any call that can fail or panic) to the moment its outcome is known, dropping this settles it
/// without resolving (replay if the target still verifies, else a reported drop); either way it
/// releases the generation's outstanding slot so the hook can hold the next press.
struct PendingClick {
    job: HeldClick,
    state: &'static MiddleClickState,
    epoch: Arc<Generation>,
    settled: bool,
}

impl PendingClick {
    fn new(job: HeldClick, state: &'static MiddleClickState, epoch: &Arc<Generation>) -> Self {
        Self {
            job,
            state,
            epoch: Arc::clone(epoch),
            settled: false,
        }
    }
}

impl Drop for PendingClick {
    fn drop(&mut self) {
        if !self.settled {
            // A panic here would abort while unwinding; the click is lost only if settling itself
            // panics, and it is reported as dropped then.
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                self.state.settle_job(
                    &self.job,
                    Resolution::Unknown(UnknownCause::Skipped),
                    true,
                    None,
                )
            }))
            .unwrap_or(ClickOutcome::Dropped(DropReason::SettleFailed));
            self.epoch.report(outcome);
        }
        self.epoch.outstanding.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Owns the receiver; on any exit (shutdown or panic) the worker first leaves the state, then
/// settles every click still queued.
struct DrainOnDrop {
    rx: Receiver<HeldClick>,
    state: &'static MiddleClickState,
    epoch: Arc<Generation>,
}

impl Drop for DrainOnDrop {
    fn drop(&mut self) {
        // Closes the window in which the hook could queue a click after the final drain.
        self.state.retire_worker(self.epoch.live.id());
        while let Ok(job) = self.rx.try_recv() {
            drop(PendingClick::new(job, self.state, &self.epoch));
        }
        // Everything reportable has been queued; the report thread ends after delivering it.
        self.epoch.reports.close();
    }
}

fn run_worker(
    state: &'static MiddleClickState,
    rx: Receiver<HeldClick>,
    epoch: Arc<Generation>,
    lane: ToggleLane,
) {
    let queue = DrainOnDrop {
        rx,
        state,
        epoch: Arc::clone(&epoch),
    };
    while let Ok(job) = queue.rx.recv() {
        // Owned from here: a panic below still settles the click.
        let mut pending = PendingClick::new(job, state, &epoch);
        // Identify the window under the press NOW (bounded); without it nothing can be verified.
        let outcome = match attribute_target(
            &mut pending.job,
            state.identity_deadline,
            state.max_pickup_age,
        ) {
            Err(reason) => ClickOutcome::Dropped(reason),
            Ok(()) => {
                // After uninstall the click is settled at once; no resolution is attempted.
                let resolution = if !epoch.live.is_live() {
                    Resolution::Unknown(UnknownCause::Skipped)
                } else {
                    let own = pending
                        .job
                        .target
                        .map(|t| t.pid == unsafe { GetCurrentProcessId() });
                    classify(
                        resolve_with_deadline(pending.job, &epoch.live),
                        pending.job.hand_cursor,
                        own,
                    )
                };
                // Read after the (possibly long) lookup: an uninstall that happened meanwhile
                // turns the settle into a shutdown settle (replay, never a toggle).
                let shutting_down = !epoch.live.is_live();
                state.settle_job(&pending.job, resolution, shutting_down, Some(&lane))
            }
        };
        pending.settled = true;
        epoch.report(outcome);
    }
}

fn lookup_from_state(state: PointLinkState) -> Lookup {
    match state {
        PointLinkState::Link => Lookup::Link,
        PointLinkState::NotLink => Lookup::NotLink,
        PointLinkState::Unknown => Lookup::Unknown,
    }
}

/// Ask a detached resolver thread what is under the pointer, waiting at most [`RESOLVE_TIMEOUT`]
/// and giving up within one slice of the generation ending. UI Automation runs on its own thread
/// (never joined), so a hung call cannot stall the worker past the deadline, and its answer is
/// dropped by that thread if the generation has ended. Every failure is `Err(cause)`: it is never
/// reported as "not a link".
fn resolve_with_deadline(job: HeldClick, live: &Arc<Liveness>) -> Result<Lookup, UnknownCause> {
    let (x, y) = (job.x, job.y);
    let Some(rx) = start_resolver(
        &RESOLVERS,
        Arc::clone(live),
        "rk-middle-click-resolve",
        move || lookup_from_state(point_link_state(x, y, LINK_WALK_ANCESTORS)),
    ) else {
        return Err(UnknownCause::ResolverUnavailable);
    };
    lookup_from_wait(wait_interruptible(&rx, RESOLVE_TIMEOUT, WAIT_SLICE, || {
        !live.is_live()
    }))
}

// ---------------------------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------------------------

/// Make sure `job` carries the identity of the window under its press point, captured no later than
/// `max_pickup_age` after the press and complete (process creation time included). An identity
/// captured later cannot be attributed to the press; one that cannot be completed cannot be
/// verified: both drop the click.
fn attribute_target(
    job: &mut HeldClick,
    deadline: Duration,
    max_pickup_age: Duration,
) -> Result<(), DropReason> {
    if job.target.is_some() {
        return Ok(());
    }
    if job.pressed_at.elapsed() > max_pickup_age {
        return Err(DropReason::PickupTooLate);
    }
    let found = identify_at(deadline, job.x, job.y);
    if job.pressed_at.elapsed() > max_pickup_age {
        return Err(DropReason::PickupTooLate);
    }
    match found {
        Some(target) if target.started.is_some() => {
            job.target = Some(target);
            Ok(())
        }
        _ => Err(DropReason::TargetUnverifiable),
    }
}

/// The window under `(x, y)`, its root, its owning pid and that process's creation time, or `None`
/// when any of it cannot be had.
///
/// Only the hit test (`WindowFromPoint`) is bounded: it is the one identity call that can wait on
/// the target's own thread (HANDOFF.md, "Identity capture"), so it runs on a detached thread behind
/// `deadline` and counts against [`MAX_IDENTITY_CALLS_IN_FLIGHT`] only once past it. `None` when it
/// misses the deadline, or while that many hit tests are already hung. The other four calls run on
/// the calling (worker) thread.
fn identify_at(deadline: Duration, x: i32, y: i32) -> Option<TargetIdentity> {
    let hwnd = bounded_call(&IDENTITY, "rk-middle-click-hit-test", deadline, move || {
        window_at(x, y)
    })
    .flatten()?;
    let mut target = identity_of_window(hwnd)?;
    target.started = process_start(target.pid);
    Some(target)
}

/// `WindowFromPoint`, as a plain value so it can cross a thread. May wait on the window's thread;
/// only [`identify_at`] calls it, behind a deadline.
fn window_at(x: i32, y: i32) -> Option<isize> {
    let hwnd = unsafe { WindowFromPoint(POINT { x, y }) };
    (!hwnd.is_null()).then_some(hwnd as isize)
}

/// The root and owning pid of `hwnd`; `started` is left empty. `None` when the window is gone or no
/// root or pid can be had. Never call this on a hook thread.
fn identity_of_window(hwnd: isize) -> Option<TargetIdentity> {
    let hwnd = hwnd as windows_sys::Win32::Foundation::HWND;
    unsafe {
        let root = GetAncestor(hwnd, GA_ROOT);
        if root.is_null() {
            return None;
        }
        let mut pid = 0u32;
        let thread_id = GetWindowThreadProcessId(hwnd, &mut pid);
        if thread_id == 0 || pid == 0 {
            return None;
        }
        Some(TargetIdentity {
            hwnd: hwnd as isize,
            root: root as isize,
            pid,
            started: None,
        })
    }
}

/// Creation time of process `pid` (FILETIME ticks), or `None` when it cannot be opened for
/// limited query (then the target is unverifiable and nothing is injected).
fn process_start(pid: u32) -> Option<u64> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
        let ok = GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) != 0;
        CloseHandle(handle);
        ok.then(|| (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
    }
}

// ---------------------------------------------------------------------------------------------
// Effects
// ---------------------------------------------------------------------------------------------

/// What the replay needs that is read BEFORE the target check (`ClickEnv::prepare`): the virtual
/// desktop and the press point in absolute coordinates.
#[derive(Clone, Copy)]
struct ClickPlan {
    rect: DesktopRect,
    to: (i32, i32),
}

/// The real effects of one settle (see [`ClickEnv`]).
struct WinEnv<'a> {
    target: TargetIdentity,
    x: i32,
    y: i32,
    tag: usize,
    /// The hit-test deadline for the target check (`MiddleClickState::identity_deadline`).
    deadline: Duration,
    /// `None` until `prepare` ran; `Some(None)`: the press point cannot be mapped.
    plan: Option<Option<ClickPlan>>,
    lane: Option<&'a ToggleLane>,
}

impl ClickEnv for WinEnv<'_> {
    fn prepare(&mut self, origin: (i32, i32)) -> bool {
        let plan = virtual_desktop().and_then(|rect| {
            Some(ClickPlan {
                rect,
                to: absolute_point(&rect, origin)?,
            })
        });
        self.plan = Some(plan);
        plan.is_some()
    }

    fn target(&mut self) -> TargetCheck {
        compare_target(
            Some(self.target),
            identify_at(self.deadline, self.x, self.y),
        )
    }

    fn lane(&mut self) -> LaneState {
        match self.lane {
            Some(lane) => lane.state(),
            // Drain paths have no lane: the host toggle is unavailable (they replay anyway).
            None => LaneState::Stalled,
        }
    }

    fn submit_toggle(&mut self) -> bool {
        self.lane.is_some_and(ToggleLane::submit)
    }

    fn click_at(&mut self, origin: (i32, i32)) -> ClickSend {
        match self.plan {
            Some(Some(plan)) => send_click_batch(plan, origin, self.tag),
            // Not prepared, or not mappable: nothing is sent.
            _ => ClickSend::Unmappable,
        }
    }
}

/// `absolute_coord` on both axes.
fn absolute_point(rect: &DesktopRect, p: (i32, i32)) -> Option<(i32, i32)> {
    Some((
        absolute_coord(p.0, rect.left, rect.width)?,
        absolute_coord(p.1, rect.top, rect.height)?,
    ))
}

fn virtual_desktop() -> Option<DesktopRect> {
    let rect = unsafe {
        DesktopRect {
            left: GetSystemMetrics(SM_XVIRTUALSCREEN),
            top: GetSystemMetrics(SM_YVIRTUALSCREEN),
            width: GetSystemMetrics(SM_CXVIRTUALSCREEN),
            height: GetSystemMetrics(SM_CYVIRTUALSCREEN),
        }
    };
    (rect.width > 0 && rect.height > 0).then_some(rect)
}

fn cursor_pos() -> Option<(i32, i32)> {
    let mut p = POINT { x: 0, y: 0 };
    (unsafe { GetCursorPos(&mut p) } != 0).then_some((p.x, p.y))
}

/// Deliver the middle click at `origin` as ONE `SendInput` batch, every event tagged in
/// `dwExtraInfo` so the hook passes them through:
/// `[absolute move to origin, middle down, middle up, absolute move back]`. `SendInput` inserts a
/// call's events serially, so no hardware motion falls between the move and the click. The move back
/// goes to where the pointer was read just before the call (omitted when it was already at
/// `origin`, or unreadable). When the OS inserts only a prefix (`SendInput` reports the count), the
/// matching button-up and the move back are sent in follow-up calls, so a button is never left down.
/// Pointer effects of every generation take turns (`POINTER_EFFECTS`).
///
/// The metrics and the press point's absolute coordinates are already in `plan` (read before the
/// target check). What runs between the target check and `SendInput` is: the `POINTER_EFFECTS` lock
/// (waits only for another generation's own batch), one `GetCursorPos`, integer arithmetic for the
/// move-back point, filling a four-item array, and the library's conversion of those items to
/// `INPUT` records (a small allocation).
fn send_click_batch(plan: ClickPlan, origin: (i32, i32), tag: usize) -> ClickSend {
    let ClickPlan { rect, to } = plan;
    let move_to = |(x, y): (i32, i32)| {
        RawInput::Mouse(RawMouse::MoveTo {
            x,
            y,
            virtual_desktop: true,
        })
    };
    let middle = |up: bool| {
        RawInput::Mouse(RawMouse::Button {
            button: RawMouseButton::Middle,
            up,
        })
    };
    let opts = SendOptions::tagged(tag);

    let _turn = lock(&POINTER_EFFECTS);
    let before = cursor_pos();
    let back_px = before.filter(|b| *b != origin);
    let back = back_px.and_then(|p| absolute_point(&rect, p));
    let mut items = [move_to(to), middle(false), middle(true), move_to(to)];
    let len = match back {
        Some(back) => {
            items[3] = move_to(back);
            4
        }
        None => 3,
    };
    let batch = &items[..len];
    // The pointer ends where the user had it when it was readable and mappable (or never moved).
    let put_back = before.is_some() && (back_px.is_none() || back.is_some());
    match send_input_batch_with(batch, &opts) {
        Ok(()) => ClickSend::Sent {
            pointer_restored: put_back,
        },
        Err(AutomationError::Platform(inserted, _)) => {
            let recovery = recovery_for(usize::try_from(inserted).unwrap_or(0), back.is_some());
            let released =
                !recovery.release_needed || send_input_batch_with(&[middle(true)], &opts).is_ok();
            let restored = !recovery.restore_needed
                || back.is_some_and(|back| send_input_batch_with(&[move_to(back)], &opts).is_ok());
            if recovery.click_complete || (recovery.release_needed && released) {
                ClickSend::Sent {
                    pointer_restored: put_back && restored,
                }
            } else if recovery.release_needed {
                ClickSend::ButtonStuck
            } else {
                ClickSend::Rejected
            }
        }
        Err(_) => ClickSend::Rejected,
    }
}
