#![cfg(target_os = "macos")]

//! Process-local focus continuity used by both sides of the desktop IPC.
//!
//! The value carried over IPC is deliberately an OS monotonic timestamp and
//! the process launch identity.  A generation stored in one process cannot
//! prove anything to another process.  This observer therefore keeps a
//! conservative native/AX sample before recording begins and rejects any
//! capture made before coverage started, after a sample was lost, or after a
//! later focus/window/app change.
//!
//! The observer is on demand. It runs only between `begin_focus_coverage` and
//! `end_focus_coverage` (a recording's start through delivery or cancel), and
//! only in the process that captures and validates tokens: the shell. The
//! engine sidecar's own stop snapshot is advisory wire evidence the shell
//! never adopts, so the engine starts no observer. With no recording, no
//! the app thread wakes for focus continuity.

use core_foundation::base::{CFType, TCFType};
use core_foundation::runloop::{kCFRunLoopDefaultMode, CFRunLoop, CFRunLoopSource};
use core_foundation::string::CFString;
use objc2::rc::Retained;
use objc2::{define_class, msg_send, sel, AnyThread};
use objc2_app_kit::{NSWorkspace, NSWorkspaceDidActivateApplicationNotification};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObject};
use std::ffi::c_void;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

type DiagnosticHook = Arc<dyn Fn(&str, u64) + Send + Sync + 'static>;
static DIAGNOSTIC_HOOK: OnceLock<DiagnosticHook> = OnceLock::new();

/// Installs content-free timing telemetry for slow focus-observer work.
/// Callback runs on observer thread, never AppKit main thread.
pub fn set_focus_observer_diagnostic_hook(hook: DiagnosticHook) {
    let _ = DIAGNOSTIC_HOOK.set(hook);
}

fn report_if_slow(phase: &str, started: Instant) {
    let elapsed_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    if elapsed_ms >= 16 {
        if let Some(hook) = DIAGNOSTIC_HOOK.get() {
            hook(phase, elapsed_ms);
        }
    }
}

#[link(name = "System", kind = "dylib")]
extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXObserverCreate(
        pid: i32,
        callback: extern "C" fn(*const c_void, *const c_void, *const c_void, *mut c_void),
        observer: *mut *const c_void,
    ) -> i32;
    fn AXObserverAddNotification(
        observer: *const c_void,
        element: *const c_void,
        notification: *const c_void,
        refcon: *mut c_void,
    ) -> i32;
    fn AXObserverGetRunLoopSource(observer: *const c_void) -> *const c_void;
    fn CFRelease(value: *const c_void);
}

#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FocusSample {
    pid: i32,
    window: isize,
    editor: isize,
    process_instance: u64,
}

/// Immutable observer evidence captured at hotkey acceptance. This is a
/// content-free native anchor; enrichment happens later on a worker thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HotkeyFocusAnchor {
    pub captured_at: u64,
    pub process_id: i32,
    pub window_identity: isize,
    pub process_instance: u64,
    pub native_control: Option<isize>,
}

#[derive(Debug, Clone, Copy)]
struct ObservationState {
    started_at: u64,
    coverage_started_at: Option<u64>,
    last_change_at: u64,
    sample: Option<FocusSample>,
    coverage_lost: bool,
    last_poll_at: u64,
    /// An AX notification arrived while `sample` was held: the sample it had
    /// and when. The next observation decides whether focus REALLY changed.
    recheck_from: Option<(FocusSample, u64)>,
}

fn state() -> &'static Mutex<ObservationState> {
    static STATE: OnceLock<Mutex<ObservationState>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(ObservationState {
            started_at: 0,
            coverage_started_at: None,
            last_change_at: 0,
            sample: None,
            coverage_lost: false,
            last_poll_at: 0,
            recheck_from: None,
        })
    })
}

#[inline]
fn os_ticks() -> u64 {
    // mach_absolute_time is monotonic across processes, which is the only
    // timestamp that can safely cross the engine/shell IPC boundary.
    unsafe { mach_absolute_time() }
}

fn nanos_to_ticks(nanos: u64) -> u64 {
    static TIMEBASE: OnceLock<(u64, u64)> = OnceLock::new();
    let (numer, denom) = *TIMEBASE.get_or_init(|| {
        let mut info = MachTimebaseInfo { numer: 0, denom: 0 };
        let ok = unsafe { mach_timebase_info(&mut info) } == 0;
        if ok && info.numer != 0 {
            (u64::from(info.numer), u64::from(info.denom))
        } else {
            (1, 1)
        }
    });
    ((nanos as u128 * denom as u128) / numer as u128).min(u64::MAX as u128) as u64
}

/// Return a launch identity for `pid`. PID alone is deliberately rejected:
/// the kernel may recycle it after the original application exits. Uses the
/// same kernel start-time identity as `FocusPin`; `NSRunningApplication`
/// reports no `launchDate` for some apps (measured: Devin), which
/// left them without any continuity sample.
fn process_launch_identity(pid: i32) -> Option<u64> {
    crate::desktop::macos::running_process_instance(pid)
}

/// Sample frontmost native destination plus AX focused editor.  All AX work
/// is confined to the observer thread; stop capture and validation only read
/// the retained sample below.
fn sample() -> Option<FocusSample> {
    let total_started = Instant::now();
    let app = crate::desktop::macos::frontmost_fast()?;
    let window_started = Instant::now();
    let window = crate::desktop::macos::frontmost_window_identity(app.pid)?;
    report_if_slow("window_list", window_started);
    // Opaque editors may not expose an AX focused element. Native continuity
    // remains useful there; AX notifications below invalidate when an
    // accessible editor/window actually changes.
    let ax_started = Instant::now();
    let editor = crate::desktop::macos::focused_target_identity_for_pid(app.pid).unwrap_or(0);
    report_if_slow("ax_focused_target", ax_started);
    let process_instance = process_launch_identity(app.pid)?;
    let result = Some(FocusSample {
        pid: app.pid,
        window,
        editor,
        process_instance,
    });
    report_if_slow("sample_total", total_started);
    result
}

fn observe_once() {
    let next = sample();
    // Record progress after AX/native work completes. A stalled observer then
    // fails freshness validation instead of looking alive on entry.
    let completed_at = os_ticks();
    let Ok(mut current) = state().lock() else {
        return;
    };
    apply_sample(&mut current, next, completed_at);
}

fn apply_sample(current: &mut ObservationState, next: Option<FocusSample>, completed_at: u64) {
    current.last_poll_at = completed_at;

    // Resolve a pending AX-notification recheck. Apps fire
    // AXUIElementDestroyed constantly (ChatGPT's composer re-lays out as text
    // grows); treating each one as a focus change voided valid paste targets
    // (measured: 2 of 50 live dictations). Same process, window AND editor
    // after the notification means focus did not move.
    if let Some((prior, notified_at)) = current.recheck_from.take() {
        if next == Some(prior) {
            current.sample = Some(prior);
        } else {
            current.last_change_at = current.last_change_at.max(notified_at);
        }
    }

    let Some(next) = next else {
        // A failed sample invalidates the current interval. A later observer
        // install starts a fresh interval, so unsupported apps do not poison
        // future native-window fallback captures.
        if current.coverage_started_at.is_some() {
            current.coverage_started_at = None;
            current.last_change_at = completed_at;
        }
        current.sample = None;
        return;
    };

    if current.sample != Some(next) {
        // Learning the editor of an otherwise identical target (0 -> known) is
        // new information, not a focus move: Chromium-class apps expose no
        // focused element until their AX tree wakes, which can be seconds
        // after the first sample.
        let editor_learned = current.sample.is_some_and(|held| {
            held.editor == 0 && next.editor != 0 && FocusSample { editor: 0, ..next } == held
        });
        if current.sample.is_some() && !editor_learned {
            current.last_change_at = completed_at;
        }
        current.sample = Some(next);
    }
    if current.coverage_started_at.is_none() && !current.coverage_lost {
        current.coverage_started_at = Some(completed_at);
    }
}

const SAMPLE_RETRY_INTERVAL: Duration = Duration::from_millis(150);

fn sample_due(
    last_sampled: Instant,
    now: Instant,
    pid_changed: bool,
    sample_missing: bool,
    sample_refresh_pending: bool,
    editor_unknown: bool,
    focus_notifications_complete: bool,
) -> bool {
    if pid_changed {
        return true;
    }

    let elapsed = now.saturating_duration_since(last_sampled);
    // A failed or notification-invalidated sample used to trigger another
    // AX/CGWindowList walk on every 40 ms run-loop cycle. Keep the state
    // conservative while giving the provider time to settle; this also keeps
    // one slow AX request from creating a retry burst.
    if sample_missing && sample_refresh_pending {
        return true;
    }
    if sample_missing {
        return elapsed >= SAMPLE_RETRY_INTERVAL;
    }

    (!focus_notifications_complete && elapsed >= Duration::from_millis(150))
        || (editor_unknown && elapsed >= Duration::from_millis(250))
        // A retained editor identity still needs periodic verification because
        // some apps omit focus notifications for an actual editor replacement.
        || elapsed >= Duration::from_secs(1)
}

/// Liveness without resampling. A retained sample stays authoritative while
/// workspace/AX notifications (which clear it) are being drained, so record
/// observer progress here instead of re-running CGWindowList every cycle.
/// Without this, `last_poll_at` froze at the last resample and every token
/// captured more than 250 ms later was refused (0.1.175 start capture).
fn heartbeat(current: &mut ObservationState, now: u64) {
    if current.sample.is_some() && !current.coverage_lost {
        current.last_poll_at = now;
    }
}

static AX_NOTIFICATIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Content-free observer state for diagnostics: ages in ms, flags, counters.
pub fn focus_observer_debug_snapshot() -> String {
    let now = os_ticks();
    // Inverse of nanos_to_ticks via its 1 ms tick width; diagnostics only.
    let tick_ms = nanos_to_ticks(1_000_000).max(1);
    let ms = |ticks: u64| now.saturating_sub(ticks) / tick_ms;
    // Read before the observation lock: coverage-then-state is the only lock order.
    let covering = focus_coverage_wanted();
    let lazy_armed = LAZY_PATH_ARMED.load(std::sync::atomic::Ordering::Relaxed);
    match state().try_lock() {
        Ok(current) => format!(
            "covering={} lazy_armed={} sample={} editor_known={} coverage={} coverage_lost={} last_change_ms_ago={} last_poll_ms_ago={} ax_notifications={}",
            covering,
            lazy_armed,
            current.sample.is_some(),
            current.sample.is_some_and(|sample| sample.editor != 0),
            current.coverage_started_at.is_some(),
            current.coverage_lost,
            ms(current.last_change_at),
            ms(current.last_poll_at),
            AX_NOTIFICATIONS.load(std::sync::atomic::Ordering::Relaxed),
        ),
        Err(_) => "locked".to_string(),
    }
}

fn hotkey_anchor_from_state(
    current: &ObservationState,
    captured_at: u64,
) -> Option<HotkeyFocusAnchor> {
    let coverage_started_at = current.coverage_started_at?;
    let sample = current.sample?;
    (current.started_at <= coverage_started_at
        && coverage_started_at <= captured_at
        && !current.coverage_lost
        && current.recheck_from.is_none()
        && sample.pid > 0
        && sample.window > 0
        && sample.process_instance != 0
        && current.last_poll_at <= captured_at
        && nanos_to_ticks(250_000_000) >= captured_at.saturating_sub(current.last_poll_at)
        && current.last_change_at < captured_at)
        .then_some(HotkeyFocusAnchor {
            captured_at,
            process_id: sample.pid,
            window_identity: sample.window,
            process_instance: sample.process_instance,
            native_control: (sample.editor != 0).then_some(sample.editor),
        })
}

/// Return the latest observer-backed focus anchor without waiting for its
/// mutex, AppKit, AX, or window-server metadata. `None` is intentionally
/// conservative: start delivery then uses stop-time destination policy.
pub fn capture_hotkey_focus_anchor() -> Option<HotkeyFocusAnchor> {
    let captured_at = os_ticks();
    let current = state().try_lock().ok()?;
    hotkey_anchor_from_state(&current, captured_at)
}

/// A definite change (another application activated).
fn mark_ax_change() {
    AX_NOTIFICATIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let now = os_ticks();
    if let Ok(mut current) = state().lock() {
        current.last_change_at = now;
        current.sample = None;
        current.recheck_from = None;
    }
}

/// AXUIElementDestroyed from the observed app: SOMETHING went away, not
/// necessarily the focused editor. Drop the sample so the observer re-reads focus on its
/// next cycle; `apply_sample` records a change only if the re-read differs.
/// Until then continuity is Pending (`sample` is None), never Valid.
fn mark_ax_notification() {
    AX_NOTIFICATIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let now = os_ticks();
    if let Ok(mut current) = state().lock() {
        note_ax_notification(&mut current, now);
    }
}

fn note_ax_notification(current: &mut ObservationState, now: u64) {
    match (current.sample.take(), current.recheck_from) {
        (Some(sample), None) => current.recheck_from = Some((sample, now)),
        // Earliest notification time is kept; a missing sample with nothing
        // to compare against is a change, as before.
        (_, Some(_)) => {}
        (None, None) => current.last_change_at = now,
    }
}

extern "C" fn ax_notification(
    _observer: *const c_void,
    _element: *const c_void,
    notification: *const c_void,
    _refcon: *mut c_void,
) {
    // AXFocusedUIElementChanged / AXFocusedWindowChanged ARE focus events: a
    // definite change, even if focus returns to the same editor. Only
    // AXUIElementDestroyed is ambiguous (apps destroy unrelated elements all
    // the time), so only that one is resolved by re-reading focus.
    let destroyed = !notification.is_null()
        && unsafe {
            CFString::wrap_under_get_rule(notification as core_foundation::string::CFStringRef)
        } == "AXUIElementDestroyed";
    if destroyed {
        mark_ax_notification();
    } else {
        mark_ax_change();
    }
}

struct WorkspaceFocusObserverIvars;

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "RightKitDesktopFocusWorkspaceObserver"]
    #[ivars = WorkspaceFocusObserverIvars]
    struct WorkspaceFocusObserver;

    impl WorkspaceFocusObserver {
        #[unsafe(method(workspaceDidActivateApplication:))]
        fn workspace_did_activate_application(&self, _notification: &NSNotification) {
            mark_ax_change();
        }
    }
);

impl WorkspaceFocusObserver {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(WorkspaceFocusObserverIvars);
        unsafe { msg_send![super(this), init] }
    }
}

struct WorkspaceActivationObserver {
    center: Retained<NSNotificationCenter>,
    observer: Retained<WorkspaceFocusObserver>,
}

impl Drop for WorkspaceActivationObserver {
    fn drop(&mut self) {
        unsafe { self.center.removeObserver(&self.observer) };
    }
}

fn install_workspace_observer() -> Option<WorkspaceActivationObserver> {
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    let observer = WorkspaceFocusObserver::new();
    unsafe {
        center.addObserver_selector_name_object(
            &observer,
            sel!(workspaceDidActivateApplication:),
            Some(NSWorkspaceDidActivateApplicationNotification),
            None,
        );
    }
    Some(WorkspaceActivationObserver { center, observer })
}

struct ActiveAxObserver {
    /// Both focus notifications registered. Only then may retained samples be
    /// trusted without polling; a partial registration (e.g. destroy-only)
    /// would let editor/window changes go unseen while samples looked fresh.
    focus_coverage_complete: bool,
    observer: *const c_void,
    _app: CFType,
    source: CFRunLoopSource,
}

impl Drop for ActiveAxObserver {
    fn drop(&mut self) {
        let run_loop = CFRunLoop::get_current();
        run_loop.remove_source(&self.source, unsafe { kCFRunLoopDefaultMode });
        unsafe { CFRelease(self.observer) };
    }
}

unsafe fn install_ax_observer(pid: i32) -> Option<ActiveAxObserver> {
    let app = crate::macos::ax::El::app_with(
        pid,
        &crate::macos::ax::AxOptions::with_timeout(Duration::from_millis(100)),
    )
    .ok()?;
    let app_type = CFType::wrap_under_get_rule(app.as_raw());
    let mut observer = std::ptr::null();
    if AXObserverCreate(pid, ax_notification, &mut observer) != 0 || observer.is_null() {
        return None;
    }
    let notifications = [
        "AXFocusedUIElementChanged",
        "AXFocusedWindowChanged",
        "AXUIElementDestroyed",
    ];
    let mut registered = false;
    let mut focus_registered = 0u8;
    for name in notifications {
        let notification = CFString::from_static_string(name);
        if AXObserverAddNotification(
            observer,
            app_type.as_CFTypeRef(),
            notification.as_CFTypeRef(),
            std::ptr::null_mut(),
        ) == 0
        {
            registered = true;
            if name != "AXUIElementDestroyed" {
                focus_registered += 1;
            }
        }
    }
    if !registered {
        CFRelease(observer);
        return None;
    }
    let source_ref = AXObserverGetRunLoopSource(observer);
    if source_ref.is_null() {
        CFRelease(observer);
        return None;
    }
    Some(ActiveAxObserver {
        focus_coverage_complete: focus_registered == 2,
        observer,
        _app: app_type,
        source: TCFType::wrap_under_get_rule(
            source_ref as core_foundation::runloop::CFRunLoopSourceRef,
        ),
    })
}

// ---------------------------------------------------------------------------
// On-demand coverage lifecycle
//
// The observer used to run for the whole process lifetime: a 40 ms loop that
// sampled the frontmost app, installed NSWorkspace/AX observers and polled
// opaque editors, in the shell AND in the engine sidecar, even with no
// recording anywhere (idle CPU, plus the 1.1 GB autorelease growth fixed in
// 35e7bb16). A capture only needs coverage that started before it and
// continued unbroken until delivery, so the observer now runs only between
// `begin_focus_coverage` and `end_focus_coverage`. Parked, the thread waits on
// a condition variable with no timeout: nothing wakes periodically.
// ---------------------------------------------------------------------------

/// How long `begin_focus_coverage` waits for the first sample and observers.
/// On a miss the call returns anyway; the capture then reports its own
/// content-free reason (`coverage_missing`, `sample_missing`) and callers use
/// their documented stop-time fallback.
pub const COVERAGE_READY_WAIT: Duration = Duration::from_millis(150);

/// Pure state machine for the observer thread's lifecycle. Idempotent
/// `arm`/`disarm` (not a counter): the application owns pairing and decides
/// when no session needs coverage any more.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CoverageLifecycle {
    /// Coverage requested and not yet ended.
    wanted: bool,
    /// Incremented on every idle -> wanted transition. A session belongs to
    /// one epoch, so an end-then-begin pair restarts the observation interval
    /// even when the thread never noticed the gap.
    epoch: u64,
    /// Epoch whose first observer cycle (observers installed, first sample
    /// attempted) has completed.
    ready_epoch: u64,
    /// The observer thread exists, running a session or parked.
    thread_spawned: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArmOutcome {
    /// Coverage was already requested; nothing restarts.
    AlreadyWanted,
    /// Idle -> wanted. `spawn` is true when no thread exists yet.
    Armed { spawn: bool },
}

impl CoverageLifecycle {
    const IDLE: Self = Self {
        wanted: false,
        epoch: 0,
        ready_epoch: 0,
        thread_spawned: false,
    };

    fn arm(&mut self) -> ArmOutcome {
        if self.wanted {
            return ArmOutcome::AlreadyWanted;
        }
        self.wanted = true;
        self.epoch += 1;
        ArmOutcome::Armed {
            spawn: !self.thread_spawned,
        }
    }

    /// Returns whether coverage was actually active.
    fn disarm(&mut self) -> bool {
        std::mem::replace(&mut self.wanted, false)
    }

    fn mark_ready(&mut self, epoch: u64) {
        if self.session_live(epoch) {
            self.ready_epoch = epoch;
        }
    }

    fn is_ready(&self) -> bool {
        self.wanted && self.ready_epoch == self.epoch
    }

    fn session_live(&self, epoch: u64) -> bool {
        self.wanted && self.epoch == epoch
    }
}

static COVERAGE: (Mutex<CoverageLifecycle>, Condvar) =
    (Mutex::new(CoverageLifecycle::IDLE), Condvar::new());
static LAZY_PATH_ARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn lock_coverage() -> std::sync::MutexGuard<'static, CoverageLifecycle> {
    COVERAGE
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Observation state at the start of a coverage session: a fresh interval
/// that no earlier token can fall inside.
fn reset_observation_for_session(current: &mut ObservationState, now: u64) {
    current.started_at = now;
    current.coverage_started_at = None;
    current.last_change_at = now;
    current.sample = None;
    current.coverage_lost = false;
    current.last_poll_at = now;
    current.recheck_from = None;
}

/// Observation state once coverage ends: nothing retained may validate or
/// capture as live. `started_at` and `coverage_lost` are left to the next
/// session's reset.
fn clear_observation_after_session(current: &mut ObservationState, now: u64) {
    current.coverage_started_at = None;
    current.sample = None;
    current.recheck_from = None;
    current.last_change_at = now;
}

/// Compatibility shim for the former process-lifetime observer. It starts no
/// thread and performs no sampling: the observer is armed lazily by
/// `begin_focus_coverage`. Safe to call any number of times, from any thread.
pub fn start_focus_observer() {
    LAZY_PATH_ARMED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Request focus coverage without waiting: starts the observer thread on first
/// use or wakes the parked one. Idempotent while coverage is already wanted.
/// Pair with `wait_for_focus_coverage` (or use `begin_focus_coverage`) before
/// capturing, and with `end_focus_coverage` after delivery or cancel.
pub fn arm_focus_coverage() {
    let mut lifecycle = lock_coverage();
    match lifecycle.arm() {
        ArmOutcome::AlreadyWanted => {}
        ArmOutcome::Armed { spawn } => {
            if spawn {
                let spawned = std::thread::Builder::new()
                    .name("macos-focus-continuity".into())
                    .spawn(observer_thread_main);
                match spawned {
                    Ok(_) => lifecycle.thread_spawned = true,
                    Err(error) => {
                        lifecycle.disarm();
                        if let Ok(mut current) = state().lock() {
                            current.coverage_lost = true;
                            current.last_change_at = os_ticks();
                        }
                        eprintln!("macos_focus_observer_spawn_error {error}");
                    }
                }
            }
            // Wake the parked thread (no-op for a freshly spawned one).
            COVERAGE.1.notify_all();
        }
    }
}

/// Wait up to `max_wait` for the current coverage session to finish its first
/// observer cycle. `false` means not ready (timeout, never armed, or ended).
pub fn wait_for_focus_coverage(max_wait: Duration) -> bool {
    let lifecycle = lock_coverage();
    let (lifecycle, _) = COVERAGE
        .1
        .wait_timeout_while(lifecycle, max_wait, |l| l.wanted && !l.is_ready())
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    lifecycle.is_ready()
}

/// Start coverage and wait (bounded by `COVERAGE_READY_WAIT`) until the first
/// sample and observers are installed. Call before capturing continuity for a
/// recording. Blocks the caller for at most `COVERAGE_READY_WAIT`; never call
/// from the AppKit main thread.
pub fn begin_focus_coverage() -> bool {
    arm_focus_coverage();
    wait_for_focus_coverage(COVERAGE_READY_WAIT)
}

/// Stop coverage. The observer thread tears down its AX and workspace
/// observers and parks on its next wake (at most one 40 ms cycle), and nothing
/// retained from the interval can capture or validate as live afterwards.
/// Call only after delivery has finished validating, or after a cancel.
pub fn end_focus_coverage() {
    let was_covering = lock_coverage().disarm();
    COVERAGE.1.notify_all();
    if was_covering {
        if let Ok(mut current) = state().lock() {
            clear_observation_after_session(&mut current, os_ticks());
        }
    }
}

/// Whether coverage is currently requested (diagnostics and tests).
pub fn focus_coverage_wanted() -> bool {
    lock_coverage().wanted
}

fn announce_coverage_ready(epoch: u64) {
    lock_coverage().mark_ready(epoch);
    COVERAGE.1.notify_all();
}

fn coverage_session_live(epoch: u64) -> bool {
    lock_coverage().session_live(epoch)
}

/// Block (no timeout, no periodic wake) until coverage is wanted; return its epoch.
fn park_until_coverage_wanted() -> u64 {
    let mut lifecycle = lock_coverage();
    while !lifecycle.wanted {
        lifecycle = COVERAGE
            .1
            .wait(lifecycle)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }
    lifecycle.epoch
}

fn observer_thread_main() {
    loop {
        let epoch = park_until_coverage_wanted();
        run_coverage_session(epoch);
    }
}

/// One coverage session: install observers, run the notification-driven loop
/// while `epoch` stays live, then drop every observer on this same thread (the
/// AX run-loop sources were added to this thread's run loop).
fn run_coverage_session(epoch: u64) {
    if let Ok(mut current) = state().lock() {
        reset_observation_for_session(&mut current, os_ticks());
    }
    objc2::rc::autoreleasepool(|_| {
        let mut active_pid = None;
        let mut active_observer: Option<ActiveAxObserver> = None;
        let mut ax_retry_at = os_ticks();
        let mut workspace_observer = install_workspace_observer();
        let mut workspace_retry_at = os_ticks();
        let mut last_sampled = Instant::now();
        let mut announced = false;
        while coverage_session_live(epoch) {
            // A plain std::thread has no autorelease pool: every autoreleased AppKit/Foundation
            // result in a cycle (frontmostApplication, localizedName, bundleIdentifier, bundleURL,
            // the window and AX probes) would stay in the thread's outermost pool for the life of
            // the process, about 12 KB per 40 ms cycle in the engine. Drain one pool per cycle.
            objc2::rc::autoreleasepool(|_| {
                let cycle_started = Instant::now();
                let now = os_ticks();
                if workspace_observer.is_none() && now >= workspace_retry_at {
                    workspace_observer = install_workspace_observer();
                    workspace_retry_at = now.saturating_add(nanos_to_ticks(1_000_000_000));
                    if workspace_observer.is_some() {
                        if let Ok(mut current) = state().lock() {
                            current.coverage_started_at = None;
                            current.coverage_lost = false;
                            current.sample = None;
                            current.last_change_at = now;
                        }
                    } else if let Ok(mut current) = state().lock() {
                        current.coverage_lost = true;
                        current.coverage_started_at = None;
                        current.sample = None;
                        current.last_change_at = now;
                    }
                }
                let pid = crate::desktop::macos::frontmost_fast().map(|app| app.pid);
                // Contention says nothing about whether focus is missing.
                // Keep draining notifications; retry the state read next cycle.
                let (sample_missing, sample_refresh_pending) = state()
                    .try_lock()
                    .map(|current| {
                        let missing = current.sample.is_none();
                        // A notification invalidation must be re-read on
                        // the next cycle. Failed reads are paced below.
                        let refresh_pending = missing
                            && (current.recheck_from.is_some()
                                || current.last_change_at > current.last_poll_at);
                        (missing, refresh_pending)
                    })
                    .unwrap_or((false, false));
                let ax_retry_due = active_observer.is_none() && now >= ax_retry_at;
                let pid_changed = pid != active_pid;
                if pid_changed || (pid.is_some() && ax_retry_due) {
                    active_observer = None;
                    active_pid = pid;
                    if let Some(pid) = pid {
                        active_observer = unsafe { install_ax_observer(pid) };
                        if let Some(ref observer) = active_observer {
                            // Observer recovery begins a new interval;
                            // prior stop tokens remain invalid.
                            if let Ok(mut current) = state().lock() {
                                current.coverage_started_at = None;
                                current.coverage_lost = false;
                                current.sample = None;
                                current.last_change_at = os_ticks();
                            }
                            let run_loop = CFRunLoop::get_current();
                            run_loop.add_source(&observer.source, unsafe { kCFRunLoopDefaultMode });
                            ax_retry_at = 0;
                        } else if let Ok(mut current) = state().lock() {
                            // AX registration is optional for native
                            // continuity; polling still covers opaque
                            // editors. Retry it with bounded backoff.
                            if pid_changed && current.coverage_started_at.is_some() {
                                current.coverage_started_at = None;
                                current.sample = None;
                                current.last_change_at = os_ticks();
                            }
                            ax_retry_at = now.saturating_add(nanos_to_ticks(1_000_000_000));
                        }
                    }
                }
                // Window identity uses CGWindowListCopyWindowInfo, which
                // shares SkyLight connection locking with CoreAnimation.
                // Sampling it every 40 ms can stall pill frame commits.
                // Workspace/AX notifications clear `sample`; resample once
                // after that change (or process activation), then retain it.
                // Without COMPLETE focus-notification coverage nothing
                // reliably invalidates a retained sample inside the app, so
                // those targets keep polling, at 150 ms rather than every
                // cycle, and polling (not the heartbeat) proves liveness.
                let notified = active_observer
                    .as_ref()
                    .is_some_and(|observer| observer.focus_coverage_complete);
                // A sample WITHOUT an editor identity is incomplete: keep
                // re-reading until the app exposes its focused element.
                // Without this the one-shot sample stayed editor-less until
                // the next app switch, and start capture rejected almost
                // every dictation.
                let editor_unknown = state()
                    .try_lock()
                    .map(|current| current.sample.is_some_and(|sample| sample.editor == 0))
                    .unwrap_or(false);
                // A retained editor identity can be WRONG, not just missing:
                // in Claude, the sample taken at a focus change can
                // name a different element than the composer that later holds
                // focus, with no notification correcting it, sending a long
                // dictation to the clipboard. Verify the retained sample
                // once a second; a differing re-read is a normal focus change.
                if sample_due(
                    last_sampled,
                    Instant::now(),
                    pid_changed,
                    sample_missing,
                    sample_refresh_pending,
                    editor_unknown,
                    notified,
                ) {
                    observe_once();
                    last_sampled = Instant::now();
                    if !announced {
                        // Observers are installed and the first sample is
                        // applied: release `begin_focus_coverage` now rather
                        // than after this cycle's 40 ms run-loop slice.
                        announced = true;
                        announce_coverage_ready(epoch);
                    }
                }
                let _ = CFRunLoop::run_in_mode(
                    unsafe { kCFRunLoopDefaultMode },
                    Duration::from_millis(40),
                    false,
                );
                // Notifications for this cycle are drained; one that fired
                // cleared `sample`, which makes this a no-op.
                if notified {
                    if let Ok(mut current) = state().lock() {
                        heartbeat(&mut current, os_ticks());
                    }
                }
                let remaining = Duration::from_millis(40).saturating_sub(cycle_started.elapsed());
                if !remaining.is_zero() {
                    std::thread::sleep(remaining);
                }
            });
            if !announced {
                // First cycle done without a sample (no frontmost app yet):
                // still release the begin wait; captures report the miss.
                announced = true;
                announce_coverage_ready(epoch);
            }
        }
        drop(active_observer.take());
        drop(workspace_observer.take());
    });
    if let Ok(mut current) = state().lock() {
        clear_observation_after_session(&mut current, os_ticks());
    }
}

/// Capture continuity at a stop boundary. This is a lock-only read of the
/// last background sample: it never queries AX, waits, or activates a target.
/// `None` means exact continuity is unavailable; callers may apply the
/// documented unchanged native-window fallback for opaque editors.
pub fn capture_focus_continuity_with_editor(
    pid: u32,
    window: isize,
) -> Option<(u64, u64, Option<isize>)> {
    let captured_at = os_ticks();
    let current = state().try_lock().ok()?;
    let coverage_started_at = current.coverage_started_at?;
    let sample = current.sample?;
    (current.started_at <= coverage_started_at
        && coverage_started_at <= captured_at
        && !current.coverage_lost
        && sample.pid == pid as i32
        && sample.window == window
        && sample.process_instance != 0
        && current.last_poll_at <= captured_at
        && nanos_to_ticks(250_000_000) >= captured_at.saturating_sub(current.last_poll_at)
        && current.last_change_at < captured_at)
        .then_some((
            captured_at,
            sample.process_instance,
            (sample.editor != 0).then_some(sample.editor),
        ))
}

/// Content-free reason why `capture_focus_continuity_with_editor` would refuse
/// right now. Diagnostics only; the capture itself stays the authority.
pub fn focus_continuity_capture_miss_reason(pid: u32, window: isize) -> &'static str {
    let now = os_ticks();
    let Ok(current) = state().try_lock() else {
        return "state_busy";
    };
    let Some(coverage) = current.coverage_started_at else {
        return "coverage_missing";
    };
    let Some(sample) = current.sample else {
        return "sample_missing";
    };
    if current.coverage_lost {
        return "coverage_lost";
    }
    if coverage > now || current.started_at > coverage {
        return "coverage_restarted";
    }
    if sample.pid != pid as i32 || sample.window != window {
        return "target_differs";
    }
    if sample.process_instance == 0 {
        return "process_unknown";
    }
    if current.last_change_at >= now {
        return "focus_changing";
    }
    if now.saturating_sub(current.last_poll_at) > nanos_to_ticks(250_000_000) {
        return "observer_progress_stale";
    }
    "capturable"
}

pub fn capture_focus_continuity(pid: u32, window: isize) -> Option<(u64, u64)> {
    capture_focus_continuity_with_editor(pid, window).map(|(at, process, _)| (at, process))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusContinuityStatus {
    Valid,
    Pending(&'static str),
    Invalid(&'static str),
}

impl FocusContinuityStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::Pending(reason) | Self::Invalid(reason) => reason,
        }
    }
    pub fn is_valid(self) -> bool {
        self == Self::Valid
    }
    pub fn is_pending(self) -> bool {
        matches!(self, Self::Pending(_))
    }
}

fn classify_continuity(
    current: &ObservationState,
    pid: u32,
    window: isize,
    captured_at: u64,
    process_instance: u64,
    now: u64,
    max_staleness: u64,
) -> FocusContinuityStatus {
    use FocusContinuityStatus::*;
    if captured_at == 0 || captured_at > now {
        return Invalid("invalid_capture_time");
    }
    if current.coverage_lost {
        return Invalid("coverage_lost");
    }
    if current.last_change_at >= captured_at {
        return Invalid("focus_changed");
    }
    let Some(coverage) = current.coverage_started_at else {
        return Invalid("coverage_missing");
    };
    if coverage == 0 || coverage > captured_at {
        return Invalid("coverage_restarted");
    }
    let Some(sample) = current.sample else {
        return if current.recheck_from.is_some() {
            Pending("observation_updated")
        } else {
            Invalid("sample_missing")
        };
    };
    if sample.pid != pid as i32 || sample.window != window {
        return Invalid("target_changed");
    }
    if sample.process_instance != process_instance {
        return Invalid("process_changed");
    }
    if current.last_poll_at < captured_at {
        return Pending("awaiting_observer_progress");
    }
    if current.last_poll_at > now {
        return Invalid("invalid_progress_time");
    }
    if now - current.last_poll_at > max_staleness {
        return Pending("observer_progress_stale");
    }
    if crate::desktop::continuity_window_valid(
        captured_at,
        now,
        coverage,
        current.last_change_at,
        current.last_poll_at,
        max_staleness,
    ) {
        Valid
    } else {
        Invalid("invalid_continuity_interval")
    }
}

pub fn focus_continuity_status(
    pid: u32,
    window: isize,
    captured_at: u64,
    process_instance: u64,
    native_control: Option<isize>,
) -> FocusContinuityStatus {
    use FocusContinuityStatus::*;
    let before = match state().try_lock() {
        Ok(current) => *current,
        Err(std::sync::TryLockError::WouldBlock) => return Pending("state_busy"),
        Err(_) => return Invalid("state_poisoned"),
    };
    if process_launch_identity(pid as i32) != Some(process_instance) {
        return Invalid("process_changed");
    }
    let current = match state().try_lock() {
        Ok(current) => current,
        Err(std::sync::TryLockError::WouldBlock) => return Pending("state_busy"),
        Err(_) => return Invalid("state_poisoned"),
    };
    let status = classify_continuity(
        &current,
        pid,
        window,
        captured_at,
        process_instance,
        os_ticks(),
        nanos_to_ticks(250_000_000),
    );
    if matches!(status, Invalid(_)) {
        return status;
    }
    if native_control
        .is_some_and(|editor| current.sample.is_none_or(|sample| sample.editor != editor))
    {
        return Invalid("editor_changed");
    }
    if current.coverage_started_at != before.coverage_started_at
        || current.last_change_at != before.last_change_at
        || current.coverage_lost != before.coverage_lost
        || current.sample != before.sample
    {
        return Pending("observation_updated");
    }
    status
}

/// Validate a previously captured stop token. Any later observed change,
/// including changing away from an editor and returning to it, invalidates
/// the token because `last_change_at` remains later than the stop timestamp.
/// This function performs no AX calls.
pub fn validate_focus_continuity(
    pid: u32,
    window: isize,
    captured_at: u64,
    process_instance: u64,
    native_control: Option<isize>,
) -> bool {
    focus_continuity_status(pid, window, captured_at, process_instance, native_control).is_valid()
}

fn settle_continuity_status(
    max_wait: Duration,
    mut check: impl FnMut() -> FocusContinuityStatus,
) -> FocusContinuityStatus {
    let deadline = Instant::now() + max_wait;
    loop {
        let status = check();
        if !status.is_pending() || Instant::now() >= deadline {
            return status;
        }
        std::thread::sleep(
            Duration::from_millis(10).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

/// Pending progress must settle before candidate acquisition. Publish only
/// after another positive check of the same original continuity interval.
pub fn capture_continuous_candidate<T>(
    before: FocusContinuityStatus,
    identity_stable: bool,
    capture: impl FnOnce() -> T,
    mut revalidate: impl FnMut() -> FocusContinuityStatus,
) -> (Option<T>, FocusContinuityStatus) {
    if !identity_stable || matches!(before, FocusContinuityStatus::Invalid(_)) {
        return (
            None,
            if identity_stable {
                before
            } else {
                FocusContinuityStatus::Invalid("target_changed")
            },
        );
    }
    let ready = if before.is_pending() {
        revalidate()
    } else {
        before
    };
    if !ready.is_valid() {
        return (None, ready);
    }
    let candidate = capture();
    let after = revalidate();
    (after.is_valid().then_some(candidate), after)
}

/// Wait only for transient observer progress/contention, never through a
/// known focus change. Original timestamp & process identity are immutable.
pub fn wait_for_focus_continuity(
    pid: u32,
    window: isize,
    captured_at: u64,
    process_instance: u64,
    native_control: Option<isize>,
    max_wait: Duration,
) -> FocusContinuityStatus {
    settle_continuity_status(max_wait, || {
        focus_continuity_status(pid, window, captured_at, process_instance, native_control)
    })
}

pub fn validate_focus_continuity_settled(
    pid: u32,
    window: isize,
    captured_at: u64,
    process_instance: u64,
    native_control: Option<isize>,
    max_wait: Duration,
) -> bool {
    wait_for_focus_continuity(
        pid,
        window,
        captured_at,
        process_instance,
        native_control,
        max_wait,
    )
    .is_valid()
}

#[cfg(test)]
#[path = "macos_focus_continuity_tests.rs"]
mod tests;
