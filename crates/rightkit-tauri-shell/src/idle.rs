//! Webview idle / resource management for tray-resident apps.
//!
//! Donor: MailRight `webview_idle.rs`. The state machine is kept verbatim (generation-stamped timer
//! so a stale timer never suspends a webview the person already brought back). When the main window
//! has been hidden for `suspend_after` the webview is suspended; showing it resumes it.
//!
//! Platform actions:
//! * Windows: WebView2 `SetIsVisible` plus `TrySuspend` / `Resume` (ported from the donor; compiled
//!   only on Windows and not exercised on this macOS host).
//! * macOS / Linux: WebKit already throttles a hidden view, there is no public suspend call, so the
//!   shell tracks the state and emits `rightkit-shell://idle` for the app to pause its own timers.
//!
//! Controller sequencing (Windows semantics, matching ScrapeRight's `webview_idle.rs`): suspend is
//! one operation on the WebView2 thread, `SetIsVisible(false)` then `TrySuspend`, and `TrySuspend`
//! is never called when hiding failed. Resume is one operation, `Resume` then `SetIsVisible(true)`,
//! and the show is skipped when `Resume` failed. A failed step stays pending in [`Applied`] and is
//! retried by the next resume request; a resume with nothing hidden or suspended by this shell
//! makes no controller call. The calls go through the internal [`Controller`] trait so the order
//! is tested with a fake.
//!
//! Hide/show paths owned by the shell (`rk_shell_window hide|show`, tray toggle, close-to-tray,
//! reveal) feed the machine automatically (a minimise too, with `IdleOptions::start_on_minimize`); an app that hides its window itself calls
//! [`note_hidden`] / [`note_visible`] (or the `rk_shell_idle_note` command).

use std::fmt::Display;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Wry};

use crate::{ShellState, EVENT_IDLE};

/// Tray / minimise to suspend delay (MailRight, locked 2026-07-06).
pub const DEFAULT_SUSPEND_AFTER: Duration = Duration::from_secs(30);

/// Longest suspend delay accepted (a larger host or per-hide value is clamped to it).
pub const MAX_SUSPEND_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// When the WebView2 controller is hidden after the main window hides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ControllerHide {
    /// At once, when the window hides (the MailRight donor behaviour and the default): rendering
    /// stops immediately, and `TrySuspend` later finds the controller already hidden.
    #[default]
    Immediate,
    /// When the idle timer fires, just before `TrySuspend` (WebView2 requires a hidden controller
    /// to suspend). The webview keeps rendering for `suspend_after` first, so a window that is
    /// minimised or hidden only briefly never loses its compositor surface.
    OnTimer,
}

/// Whether `rightkit-shell://idle` events are emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IdleEvents {
    /// Every transition on every platform (the default; macOS and Linux apps pause their own
    /// timers from these).
    #[default]
    Always,
    /// Only where the platform really suspends the webview (Windows). On macOS and Linux nothing
    /// is suspended, so nothing is emitted.
    NativeSuspendOnly,
}

impl IdleEvents {
    /// True when events are emitted on a platform with (`native_suspend`) or without a real suspend.
    pub fn emits(self, native_suspend: bool) -> bool {
        match self {
            IdleEvents::Always => true,
            IdleEvents::NativeSuspendOnly => native_suspend,
        }
    }
}

/// What starts the idle period of the main webview.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IdleTrigger {
    /// A window hide the shell or host reports (tray, close-to-tray, `note_hidden`), and a minimise
    /// when `IdleOptions::start_on_minimize` is set. The behaviour hosts already have.
    #[default]
    HiddenOrMinimised,
    /// A minimise only (whole-app: it acts on the webview of the window that was minimised). A
    /// tray hide, close-to-tray or `note_hidden` does not start idle, and a minimise always does,
    /// whatever `start_on_minimize` says. For an app with a single web view that only wants to stop
    /// work while minimised: on Windows `SetIsVisible(false)` then `TrySuspend`, nothing closed;
    /// the restore is `Resume` (when suspended) then `SetIsVisible(true)`.
    Minimised,
}

/// Where an idle request came from (see [`source_starts_idle`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleSource {
    /// A window hide (tray, close-to-tray, app-reported).
    Hide,
    /// The main window was minimised.
    Minimise,
}

/// Whether a request of `source` may start the idle period under `trigger`. Pure.
pub fn source_starts_idle(trigger: IdleTrigger, source: IdleSource) -> bool {
    !matches!((trigger, source), (IdleTrigger::Minimised, IdleSource::Hide))
}

/// Per-hide delay source held by [`IdleOptions`]; set with [`IdleOptions::suspend_after_with`].
#[derive(Clone)]
pub struct SuspendAfterProvider(Arc<dyn Fn() -> Option<Duration> + Send + Sync>);

impl std::fmt::Debug for SuspendAfterProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SuspendAfterProvider(..)")
    }
}

/// Two options are equal when they share the same provider instance (closures have no value
/// equality), so a clone is equal to its source.
impl PartialEq for SuspendAfterProvider {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for SuspendAfterProvider {}

/// Per-host idle options (see [`crate::ShellBuilder::webview_idle_options`]). The default is the
/// behaviour `webview_idle(DEFAULT_SUSPEND_AFTER)` has always had.
///
/// Not `Copy`: it may hold a delay provider (see [`IdleOptions::suspend_after_with`]). It cannot be
/// built with a struct literal outside this crate, not even with `..IdleOptions::default()`,
/// because the provider field is private. Build it with the setters:
/// `IdleOptions::default().suspend_after(d).start_on_minimize(true)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdleOptions {
    /// Hidden this long before the webview is suspended (clamped to [`MAX_SUSPEND_AFTER`]). With a
    /// provider set, this is the value used whenever the provider returns `None` or fails.
    pub suspend_after: Duration,
    /// Also start the idle timer when the main window is minimised, not only on a hide the shell or
    /// host reports (`note_hidden`). Restoring the window ends it. Default off.
    pub start_on_minimize: bool,
    pub controller_hide: ControllerHide,
    pub events: IdleEvents,
    /// What starts idle; `IdleTrigger::Minimised` makes it minimise-only.
    pub trigger: IdleTrigger,
    /// Per-hide delay source; `None` keeps the fixed `suspend_after`.
    suspend_after_fn: Option<SuspendAfterProvider>,
}

impl Default for IdleOptions {
    fn default() -> Self {
        Self {
            suspend_after: DEFAULT_SUSPEND_AFTER,
            start_on_minimize: false,
            controller_hide: ControllerHide::Immediate,
            events: IdleEvents::Always,
            trigger: IdleTrigger::HiddenOrMinimised,
            suspend_after_fn: None,
        }
    }
}

impl IdleOptions {
    /// Set the fixed suspend delay (see the field of the same name).
    pub fn suspend_after(mut self, suspend_after: Duration) -> Self {
        self.suspend_after = suspend_after;
        self
    }

    /// Also start the idle timer when the main window is minimised.
    pub fn start_on_minimize(mut self, start_on_minimize: bool) -> Self {
        self.start_on_minimize = start_on_minimize;
        self
    }

    /// Choose when the controller is hidden.
    pub fn controller_hide(mut self, controller_hide: ControllerHide) -> Self {
        self.controller_hide = controller_hide;
        self
    }

    /// Choose what starts idle (see [`IdleTrigger`]).
    pub fn trigger(mut self, trigger: IdleTrigger) -> Self {
        self.trigger = trigger;
        self
    }

    /// Minimise-only idle: `true` is `trigger(IdleTrigger::Minimised)` (a minimise starts idle, a
    /// tray hide does not), `false` restores the default trigger.
    pub fn on_minimise(mut self, on_minimise: bool) -> Self {
        self.trigger = if on_minimise {
            IdleTrigger::Minimised
        } else {
            IdleTrigger::HiddenOrMinimised
        };
        self
    }

    /// Whether a minimise of the main window feeds the idle machine.
    pub fn minimise_enabled(&self) -> bool {
        self.start_on_minimize || self.trigger == IdleTrigger::Minimised
    }

    /// Choose which idle events are emitted.
    pub fn events(mut self, events: IdleEvents) -> Self {
        self.events = events;
        self
    }

    /// Read the suspend delay at each hide instead of fixing it at build time.
    ///
    /// `provider` is called once per hide transition (a window hide, or the start of a minimise
    /// when `start_on_minimize` is set), never at build time. Its `Some(d)` is the delay for that
    /// hide, clamped to [`MAX_SUSPEND_AFTER`]; `None` uses the fixed `suspend_after`. A panic in
    /// `provider` is caught and also uses the fixed `suspend_after`. An explicit per-hide delay
    /// (`note_hidden_after`, `note_minimized_after`, `rk_shell_idle_note`) takes precedence and the
    /// provider is not called for that hide.
    ///
    /// Takes `self`, so it chains onto the default and the other setters:
    /// `IdleOptions::default().suspend_after_with(|| Some(read_env()))`.
    pub fn suspend_after_with(
        mut self,
        provider: impl Fn() -> Option<Duration> + Send + Sync + 'static,
    ) -> Self {
        self.suspend_after_fn = Some(SuspendAfterProvider(Arc::new(provider)));
        self
    }

    /// The suspend delay for one hide, evaluating the provider now (once per call). Precedence:
    /// `per_hide` if given, else the provider's `Some`, else `host_default` (the fixed
    /// `suspend_after`). The result is clamped to [`MAX_SUSPEND_AFTER`].
    pub fn hide_delay(&self, host_default: Duration, per_hide: Option<Duration>) -> Duration {
        effective_delay(host_default, per_hide.or_else(|| self.provided_delay()))
    }

    /// The provider's value for this hide. `None` when there is no provider, it returns `None`, or
    /// it panics (the panic is contained here so the idle machinery keeps running).
    fn provided_delay(&self) -> Option<Duration> {
        let provider = self.suspend_after_fn.as_ref()?;
        catch_unwind(AssertUnwindSafe(|| (*provider.0)()))
            .ok()
            .flatten()
    }
}

/// The delay for one hide: the per-hide override if given, else the host default, clamped.
pub fn effective_delay(host_default: Duration, per_hide: Option<Duration>) -> Duration {
    per_hide.unwrap_or(host_default).min(MAX_SUSPEND_AFTER)
}

/// When the controller is hidden for one window hide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HidePlan {
    /// Hide the controller as the window hides.
    pub now: bool,
    /// Hide the controller when the timer fires.
    pub on_timer: bool,
}

/// `controller_already_hidden`: the OS hid the window already (close-to-tray), so no call is needed.
pub fn hide_plan(mode: ControllerHide, controller_already_hidden: bool) -> HidePlan {
    HidePlan {
        now: mode == ControllerHide::Immediate && !controller_already_hidden,
        on_timer: mode == ControllerHide::OnTimer && !controller_already_hidden,
    }
}

/// What a minimise or restore of the main window asks of the idle machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MinimizeStep {
    Start,
    End,
    None,
}

/// Remembers that a minimise (not a tray hide) started the idle timer, so repeated resize events
/// do not restart it and a restore ends only what a minimise began.
#[derive(Debug, Default)]
pub struct MinimizeTracker {
    active: AtomicBool,
}

impl MinimizeTracker {
    pub fn step(&self, minimized: bool) -> MinimizeStep {
        let was = self.active.swap(minimized, Ordering::SeqCst);
        match (was, minimized) {
            (false, true) => MinimizeStep::Start,
            (true, false) => MinimizeStep::End,
            _ => MinimizeStep::None,
        }
    }

    /// The window is visible again by another route.
    pub fn clear(&self) {
        self.active.store(false, Ordering::SeqCst);
    }
}

/// Which platform webview action failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IdlePhase {
    Suspend,
    Resume,
}

/// A suspend or resume of the main webview that the platform reported as failed. Delivered to the
/// callback set with [`crate::ShellBuilder::webview_idle_on_error`] so the host can log it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IdleFailure {
    pub phase: IdlePhase,
    /// Label of the webview window the action targeted.
    pub window: String,
    /// The platform's message (for WebView2, the HRESULT text).
    pub error: String,
}

/// Receives [`IdleFailure`]s. May run on a WebView2 callback thread, so it must be `Send + Sync`.
pub type IdleErrorCallback = Arc<dyn Fn(&IdleFailure) + Send + Sync>;

/// Carries failures from a platform callback (possibly on another thread) to the host callback.
#[derive(Clone)]
pub(crate) struct Reporter {
    window: String,
    sink: Option<IdleErrorCallback>,
}

impl Reporter {
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn fail(&self, phase: IdlePhase, error: impl Display) {
        if let Some(sink) = &self.sink {
            sink(&IdleFailure {
                phase,
                window: self.window.clone(),
                error: error.to_string(),
            });
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleAction {
    HideControllerAndStartTimer,
    ShowController,
    SuspendNow,
    ResumeAndShowController,
    None,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IdleState {
    hidden: bool,
    suspended: bool,
    generation: u64,
}

impl IdleState {
    pub fn on_hidden(&mut self) -> (u64, IdleAction) {
        if self.suspended {
            self.hidden = true;
            return (self.generation, IdleAction::None);
        }
        self.hidden = true;
        self.generation = self.generation.wrapping_add(1);
        (self.generation, IdleAction::HideControllerAndStartTimer)
    }

    pub fn on_visible(&mut self) -> IdleAction {
        let was_hidden = self.hidden;
        self.hidden = false;
        self.generation = self.generation.wrapping_add(1);
        if std::mem::take(&mut self.suspended) {
            IdleAction::ResumeAndShowController
        } else if was_hidden {
            IdleAction::ShowController
        } else {
            IdleAction::None
        }
    }

    pub fn on_timer(&mut self, generation: u64) -> IdleAction {
        if self.hidden && !self.suspended && self.generation == generation {
            self.suspended = true;
            IdleAction::SuspendNow
        } else {
            IdleAction::None
        }
    }

    /// The platform suspend did not happen (hiding the controller or `TrySuspend` failed): the
    /// webview is still running, so forget the suspend. A later hide starts a fresh timer.
    pub fn on_suspend_failed(&mut self) {
        self.suspended = false;
    }

    /// `(hidden, suspended)`.
    pub fn snapshot(&self) -> (bool, bool) {
        (self.hidden, self.suspended)
    }
}

/// A real suspend or resume of the main webview (see [`IdleTransition`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IdleTransitionKind {
    Suspended,
    Resumed,
}

/// Delivered to the callback set with [`crate::ShellBuilder::webview_idle_on_transition`], once per
/// transition, after the platform calls succeeded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IdleTransition {
    pub kind: IdleTransitionKind,
    /// Label of the webview window.
    pub window: String,
    /// `Suspended`: how long the window had been hidden. `Resumed`: how long the webview was
    /// suspended. (For `Suspended` on Windows the `TrySuspend` request was accepted; its later
    /// completion error still goes to the error callback.)
    pub elapsed: Duration,
}

/// Receives [`IdleTransition`]s. Runs on the thread that applied the transition.
pub type IdleTransitionCallback = Arc<dyn Fn(&IdleTransition) + Send + Sync>;

/// The WebView2 controller calls the sequencing needs. Windows implements them on the real
/// controller; elsewhere they succeed without effect; tests record and fail them.
pub(crate) trait Controller {
    fn set_visible(&mut self, visible: bool) -> Result<(), String>;
    /// Request `TrySuspend` (accepted, not completed).
    fn try_suspend(&mut self) -> Result<(), String>;
    fn resume(&mut self) -> Result<(), String>;
}

/// What this shell has done to the controller and not yet undone.
#[derive(Debug, Default)]
pub(crate) struct Applied {
    /// This shell hid the controller.
    pub(crate) hidden: bool,
    /// This shell suspended the webview.
    pub(crate) suspended: bool,
    suspended_at: Option<Instant>,
    /// `Resume` succeeded but the show did not: the resumed hook fires when the retry succeeds.
    resumed_for: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StepFailure {
    pub(crate) phase: IdlePhase,
    pub(crate) error: String,
}

/// What a resume request did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResumeStep {
    /// Nothing was hidden or suspended by this shell: no controller call was made.
    Nothing,
    /// The controller was shown (it had not been suspended).
    Shown,
    /// The webview was resumed and shown after being suspended for the duration.
    Resumed(Duration),
}

impl Applied {
    /// Hide the controller (once). A failure leaves the webview visible and running.
    pub(crate) fn hide(&mut self, c: &mut dyn Controller) -> Result<(), StepFailure> {
        if self.hidden {
            return Ok(());
        }
        c.set_visible(false).map_err(|error| StepFailure {
            phase: IdlePhase::Suspend,
            error: format!("SetIsVisible(false): {error}"),
        })?;
        self.hidden = true;
        Ok(())
    }

    /// Hide, then suspend; the suspend is not attempted when the hide failed. When `TrySuspend`
    /// itself fails the controller stays hidden (the window is hidden) but nothing is suspended.
    pub(crate) fn hide_and_suspend(
        &mut self,
        c: &mut dyn Controller,
        now: Instant,
    ) -> Result<(), StepFailure> {
        self.hide(c)?;
        if self.suspended {
            return Ok(());
        }
        c.try_suspend().map_err(|error| StepFailure {
            phase: IdlePhase::Suspend,
            error: format!("TrySuspend: {error}"),
        })?;
        self.suspended = true;
        self.suspended_at = Some(now);
        Ok(())
    }

    /// Resume, then show; the show is not attempted when the resume failed. Whatever failed stays
    /// pending, so the next call retries exactly that step.
    pub(crate) fn resume_and_show(
        &mut self,
        c: &mut dyn Controller,
        now: Instant,
    ) -> Result<ResumeStep, StepFailure> {
        let mut resumed = None;
        if self.suspended {
            c.resume().map_err(|error| StepFailure {
                phase: IdlePhase::Resume,
                error: format!("Resume: {error}"),
            })?;
            self.suspended = false;
            let for_ = self
                .suspended_at
                .take()
                .map(|at| now.saturating_duration_since(at))
                .unwrap_or_default();
            self.resumed_for = Some(for_);
        }
        if self.hidden {
            c.set_visible(true).map_err(|error| StepFailure {
                phase: IdlePhase::Resume,
                error: format!("SetIsVisible(true): {error}"),
            })?;
            self.hidden = false;
        }
        if let Some(for_) = self.resumed_for.take() {
            resumed = Some(for_);
        }
        Ok(match resumed {
            Some(for_) => ResumeStep::Resumed(for_),
            None => ResumeStep::Nothing,
        })
    }
}

impl Applied {
    /// A resume request that found the controller hidden but not suspended reports `Shown`.
    fn classify(&self, before_hidden: bool, step: ResumeStep) -> ResumeStep {
        match step {
            ResumeStep::Nothing if before_hidden => ResumeStep::Shown,
            other => other,
        }
    }
}

/// Runtime half kept in [`ShellState`].
pub struct IdleHost {
    pub suspend_after: Duration,
    pub options: IdleOptions,
    pub minimize: MinimizeTracker,
    pub state: Mutex<IdleState>,
    /// How many times the platform suspend / resume action succeeded (diagnostics, QA).
    pub counters: Mutex<(u32, u32)>,
    /// Receives platform suspend / resume failures. `None` keeps them silent (the default).
    pub on_error: Option<IdleErrorCallback>,
    /// Receives suspended / resumed transitions. `None` (the default) changes nothing.
    pub on_transition: Option<IdleTransitionCallback>,
    pub(crate) applied: Mutex<Applied>,
}

impl IdleHost {
    pub fn new(suspend_after: Duration) -> Self {
        Self {
            suspend_after,
            options: IdleOptions {
                suspend_after,
                ..IdleOptions::default()
            },
            minimize: MinimizeTracker::default(),
            state: Mutex::new(IdleState::default()),
            counters: Mutex::new((0, 0)),
            on_error: None,
            on_transition: None,
            applied: Mutex::new(Applied::default()),
        }
    }

    /// Replace the options; the timeout follows `options.suspend_after`.
    pub fn with_options(mut self, options: IdleOptions) -> Self {
        self.suspend_after = options.suspend_after.min(MAX_SUSPEND_AFTER);
        self.options = IdleOptions {
            suspend_after: self.suspend_after,
            ..options
        };
        self
    }

    /// Install (or clear, with `None`) the failure callback.
    pub fn with_on_error(mut self, on_error: Option<IdleErrorCallback>) -> Self {
        self.on_error = on_error;
        self
    }

    /// Install (or clear, with `None`) the transition callback.
    pub fn with_on_transition(mut self, on_transition: Option<IdleTransitionCallback>) -> Self {
        self.on_transition = on_transition;
        self
    }

    /// Deliver a failure to the callback, if one is installed. Platform code and tests use this.
    pub fn report_failure(&self, phase: IdlePhase, window: &str, error: &str) {
        self.reporter(window).fail(phase, error);
    }

    pub(crate) fn reporter(&self, window: &str) -> Reporter {
        Reporter {
            window: window.to_owned(),
            sink: self.on_error.clone(),
        }
    }

    /// Deliver a transition to the callback, if one is installed.
    pub(crate) fn transition(&self, kind: IdleTransitionKind, window: &str, elapsed: Duration) {
        if let Some(sink) = &self.on_transition {
            sink(&IdleTransition {
                kind,
                window: window.to_owned(),
                elapsed,
            });
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IdleStatus {
    pub enabled: bool,
    pub hidden: bool,
    pub suspended: bool,
    pub suspend_after_ms: u64,
    pub suspends: u32,
    pub resumes: u32,
    /// Whether the platform has a real suspend call (Windows) or only the state and event.
    pub native_suspend: bool,
}

pub fn status(app: &AppHandle<Wry>) -> IdleStatus {
    let state = app.state::<ShellState>();
    match &state.idle {
        Some(h) => {
            let (hidden, suspended) = h.state.lock().unwrap_or_else(|e| e.into_inner()).snapshot();
            let (suspends, resumes) = *h.counters.lock().unwrap_or_else(|e| e.into_inner());
            IdleStatus {
                enabled: true,
                hidden,
                suspended,
                suspend_after_ms: h.suspend_after.as_millis() as u64,
                suspends,
                resumes,
                native_suspend: cfg!(windows),
            }
        }
        None => IdleStatus {
            enabled: false,
            hidden: false,
            suspended: false,
            suspend_after_ms: 0,
            suspends: 0,
            resumes: 0,
            native_suspend: cfg!(windows),
        },
    }
}

fn emit(app: &AppHandle<Wry>, host: &IdleHost, state: &str) {
    if host.options.events.emits(cfg!(windows)) {
        let _ = app.emit(EVENT_IDLE, serde_json::json!({"state": state}));
    }
}

/// The main window was hidden (minimise, tray, app-driven). `controller_already_hidden` skips the
/// redundant WebView2 visibility call on close-to-tray, where the OS hid the window already.
pub fn note_hidden(app: &AppHandle<Wry>, controller_already_hidden: bool) {
    note_hidden_after(app, controller_already_hidden, None);
}

/// [`note_hidden`] with a delay for this hide only (`None`: the host's `suspend_after`).
pub fn note_hidden_after(
    app: &AppHandle<Wry>,
    controller_already_hidden: bool,
    suspend_after: Option<Duration>,
) {
    let state = app.state::<ShellState>();
    let Some(host) = &state.idle else { return };
    if !source_starts_idle(host.options.trigger, IdleSource::Hide) {
        return;
    }
    start_hide(app, controller_already_hidden, suspend_after);
}

/// The shared hide path behind [`note_hidden_after`] and a minimise (the trigger gate has run).
fn start_hide(
    app: &AppHandle<Wry>,
    controller_already_hidden: bool,
    suspend_after: Option<Duration>,
) {
    let state = app.state::<ShellState>();
    let Some(host) = &state.idle else { return };
    let (generation, action) = host
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .on_hidden();
    if action != IdleAction::HideControllerAndStartTimer {
        return;
    }
    emit(app, host, "hidden");
    let hidden_at = Instant::now();
    let plan = hide_plan(host.options.controller_hide, controller_already_hidden);
    if plan.now {
        hide_controller(app);
    }
    // Evaluated once, here, after the transition check: a hide that did not start the timer does
    // not call the provider.
    let delay = host.options.hide_delay(host.suspend_after, suspend_after);
    let app = app.clone();
    let _ = std::thread::Builder::new()
        .name("rk-shell-idle".into())
        .spawn(move || {
            std::thread::sleep(delay);
            suspend_controller(&app, generation, hidden_at);
        });
}

/// Hide the controller as the window hides (`ControllerHide::Immediate`). One operation on the
/// WebView2 thread; skipped when the window was shown again before it ran.
fn hide_controller(app: &AppHandle<Wry>) {
    let state = app.state::<ShellState>();
    let Some(host) = &state.idle else { return };
    let reporter = host.reporter(&state.main_label);
    let app = app.clone();
    let run = platform::with_controller(&app.clone(), move |c| {
        let state = app.state::<ShellState>();
        let Some(host) = &state.idle else { return };
        let mut applied = host.applied.lock().unwrap_or_else(|e| e.into_inner());
        let still_hidden = host
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .snapshot()
            .0;
        if !still_hidden {
            return;
        }
        if let Err(f) = applied.hide(c) {
            host.reporter(&state.main_label).fail(f.phase, f.error);
        }
    });
    if let Err(e) = run {
        reporter.fail(IdlePhase::Suspend, e);
    }
}

/// The idle timer fired: decide on the WebView2 thread (so a show that raced the timer wins),
/// then hide and suspend as one operation.
fn suspend_controller(app: &AppHandle<Wry>, generation: u64, hidden_at: Instant) {
    let state = app.state::<ShellState>();
    let Some(host) = &state.idle else { return };
    let reporter = host.reporter(&state.main_label);
    let handle = app.clone();
    let run = platform::with_controller(app, move |c| {
        let state = handle.state::<ShellState>();
        let Some(host) = &state.idle else { return };
        let mut applied = host.applied.lock().unwrap_or_else(|e| e.into_inner());
        let action = host
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .on_timer(generation);
        if action != IdleAction::SuspendNow {
            return;
        }
        let now = Instant::now();
        match applied.hide_and_suspend(c, now) {
            Ok(()) => {
                host.counters.lock().unwrap_or_else(|e| e.into_inner()).0 += 1;
                emit(&handle, host, "suspended");
                host.transition(
                    IdleTransitionKind::Suspended,
                    &state.main_label,
                    now.saturating_duration_since(hidden_at),
                );
            }
            Err(f) => {
                host.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .on_suspend_failed();
                host.reporter(&state.main_label).fail(f.phase, f.error);
            }
        }
    });
    if let Err(e) = run {
        reporter.fail(IdlePhase::Suspend, e);
    }
}

/// What a minimise or restore asks of the idle machine, with the delay resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MinimizeAction {
    /// Start the idle period; the controller is hidden per the options; suspend after the delay.
    Hide {
        delay: Duration,
    },
    Visible,
    None,
}

/// The decision behind [`note_minimized_after`]: the per-hide override applies to a minimise
/// exactly as it does to [`note_hidden_after`].
pub fn minimize_action(
    host_default: Duration,
    step: MinimizeStep,
    per_hide: Option<Duration>,
) -> MinimizeAction {
    match step {
        MinimizeStep::Start => MinimizeAction::Hide {
            delay: effective_delay(host_default, per_hide),
        },
        MinimizeStep::End => MinimizeAction::Visible,
        MinimizeStep::None => MinimizeAction::None,
    }
}

/// The main window was minimised (`true`) or restored (`false`). Does nothing unless the host set
/// `IdleOptions::start_on_minimize` or `IdleTrigger::Minimised` (`on_minimise(true)`). On macOS and
/// Linux the machine and the `rightkit-shell://idle` event run but nothing is suspended (no
/// platform call exists there; the app pauses its own timers from the event). A restore ends only an idle period a minimise began.
pub fn note_minimized(app: &AppHandle<Wry>, minimized: bool) {
    note_minimized_after(app, minimized, None);
}

/// [`note_minimized`] with a delay for this minimise only (`None`: the host's `suspend_after`),
/// the same override [`note_hidden_after`] takes.
pub fn note_minimized_after(
    app: &AppHandle<Wry>,
    minimized: bool,
    suspend_after: Option<Duration>,
) {
    let state = app.state::<ShellState>();
    let Some(host) = &state.idle else { return };
    if !host.options.minimise_enabled() {
        return;
    }
    let step = host.minimize.step(minimized);
    // A minimise that starts the timer is a hide transition: resolve its delay once, here, with the
    // same precedence `note_hidden_after` uses, so the provider is not consulted twice.
    let per_hide = match step {
        MinimizeStep::Start => Some(host.options.hide_delay(host.suspend_after, suspend_after)),
        MinimizeStep::End | MinimizeStep::None => suspend_after,
    };
    match minimize_action(host.suspend_after, step, per_hide) {
        MinimizeAction::Hide { delay } => start_hide(app, false, Some(delay)),
        MinimizeAction::Visible => note_visible(app),
        MinimizeAction::None => {}
    }
}

/// The main window is visible again. Resume then show as one operation on the WebView2 thread;
/// a no-op for the controller unless this shell hid or suspended it. A step that failed earlier
/// is retried here.
pub fn note_visible(app: &AppHandle<Wry>) {
    let state = app.state::<ShellState>();
    let Some(host) = &state.idle else { return };
    host.minimize.clear();
    let action = host
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .on_visible();
    let reporter = host.reporter(&state.main_label);
    let handle = app.clone();
    let run = platform::with_controller(app, move |c| {
        let state = handle.state::<ShellState>();
        let Some(host) = &state.idle else { return };
        let mut applied = host.applied.lock().unwrap_or_else(|e| e.into_inner());
        let before_hidden = applied.hidden;
        let now = Instant::now();
        match applied.resume_and_show(c, now) {
            Ok(step) => match applied.classify(before_hidden, step) {
                ResumeStep::Resumed(for_) => {
                    host.counters.lock().unwrap_or_else(|e| e.into_inner()).1 += 1;
                    emit(&handle, host, "resumed");
                    host.transition(IdleTransitionKind::Resumed, &state.main_label, for_);
                }
                ResumeStep::Shown => emit(&handle, host, "visible"),
                ResumeStep::Nothing => {
                    if matches!(
                        action,
                        IdleAction::ShowController | IdleAction::ResumeAndShowController
                    ) {
                        emit(&handle, host, "visible");
                    }
                }
            },
            Err(f) => host.reporter(&state.main_label).fail(f.phase, f.error),
        }
    });
    if let Err(e) = run {
        reporter.fail(IdlePhase::Resume, e);
    }
}

#[cfg(windows)]
mod platform {
    use crate::winapi::webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_3;
    use crate::winapi::windows::core::Interface;
    use tauri::{AppHandle, Manager, Wry};

    use super::Controller;
    use crate::ShellState;

    /// The real controller, as three closures built inside the `with_webview` callback.
    struct Wv2<'a> {
        visible: &'a dyn Fn(bool) -> Result<(), String>,
        suspend: &'a dyn Fn() -> Result<(), String>,
        resume: &'a dyn Fn() -> Result<(), String>,
    }

    impl Controller for Wv2<'_> {
        fn set_visible(&mut self, visible: bool) -> Result<(), String> {
            (self.visible)(visible)
        }
        fn try_suspend(&mut self) -> Result<(), String> {
            (self.suspend)()
        }
        fn resume(&mut self) -> Result<(), String> {
            (self.resume)()
        }
    }

    /// Run `f` on the WebView2 thread against the main window's controller. `Err`: the closure
    /// could not be dispatched (window missing, webview gone); `f` did not run.
    pub fn with_controller(
        app: &AppHandle<Wry>,
        f: impl FnOnce(&mut dyn Controller) + Send + 'static,
    ) -> Result<(), String> {
        let label = app.state::<ShellState>().main_label.clone();
        let w = app
            .get_webview_window(&label)
            .ok_or_else(|| "main window not found".to_string())?;
        let report = app
            .state::<ShellState>()
            .idle
            .as_ref()
            .map(|h| h.reporter(&label));
        w.with_webview(move |webview| {
            let controller = webview.controller();
            let visible = |v: bool| -> Result<(), String> {
                // SAFETY: COM call on the live controller inside the webview closure.
                unsafe { controller.SetIsVisible(v) }.map_err(|e| e.to_string())
            };
            let core3 = || -> Result<ICoreWebView2_3, String> {
                // SAFETY: as above.
                let core = unsafe { controller.CoreWebView2() }.map_err(|e| e.to_string())?;
                core.cast::<ICoreWebView2_3>().map_err(|e| e.to_string())
            };
            let suspend = || -> Result<(), String> {
                let core3 = core3()?;
                let completion = report.clone();
                let handler = crate::winapi::webview2_com::TrySuspendCompletedHandler::create(
                    Box::new(move |error, _suspended| {
                        if let (Err(e), Some(r)) = (error, &completion) {
                            r.fail(super::IdlePhase::Suspend, e);
                        }
                        Ok(())
                    }),
                );
                // SAFETY: as above.
                unsafe { core3.TrySuspend(&handler) }.map_err(|e| e.to_string())
            };
            let resume = || -> Result<(), String> {
                let core3 = core3()?;
                // SAFETY: as above.
                unsafe { core3.Resume() }.map_err(|e| e.to_string())
            };
            let mut c = Wv2 {
                visible: &visible,
                suspend: &suspend,
                resume: &resume,
            };
            f(&mut c);
        })
        .map_err(|e| e.to_string())
    }
}

#[cfg(not(windows))]
mod platform {
    use tauri::{AppHandle, Wry};

    use super::Controller;

    /// No webview suspend call here: every step succeeds without effect, so the state, counters,
    /// events and hooks behave as on Windows.
    struct Noop;

    impl Controller for Noop {
        fn set_visible(&mut self, _: bool) -> Result<(), String> {
            Ok(())
        }
        fn try_suspend(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn resume(&mut self) -> Result<(), String> {
            Ok(())
        }
    }

    pub fn with_controller(
        _: &AppHandle<Wry>,
        f: impl FnOnce(&mut dyn Controller) + Send + 'static,
    ) -> Result<(), String> {
        f(&mut Noop);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::atomic::AtomicUsize;

    /// Records every call in order; any named call can be made to fail.
    #[derive(Default)]
    struct Fake {
        calls: Vec<&'static str>,
        failing: HashSet<&'static str>,
    }

    impl Fake {
        fn failing(names: &[&'static str]) -> Self {
            Self {
                failing: names.iter().copied().collect(),
                ..Self::default()
            }
        }
        fn call(&mut self, name: &'static str) -> Result<(), String> {
            self.calls.push(name);
            if self.failing.contains(name) {
                Err(format!("{name} failed"))
            } else {
                Ok(())
            }
        }
    }

    impl Controller for Fake {
        fn set_visible(&mut self, visible: bool) -> Result<(), String> {
            self.call(if visible { "show" } else { "hide" })
        }
        fn try_suspend(&mut self) -> Result<(), String> {
            self.call("suspend")
        }
        fn resume(&mut self) -> Result<(), String> {
            self.call("resume")
        }
    }

    #[test]
    fn suspend_is_hide_then_suspend() {
        let (mut a, mut c) = (Applied::default(), Fake::default());
        a.hide_and_suspend(&mut c, Instant::now()).unwrap();
        assert_eq!(c.calls, ["hide", "suspend"]);
        assert!(a.hidden && a.suspended);
    }

    #[test]
    fn a_failed_hide_means_no_suspend_and_a_running_webview() {
        let (mut a, mut c) = (Applied::default(), Fake::failing(&["hide"]));
        let f = a.hide_and_suspend(&mut c, Instant::now()).unwrap_err();
        assert_eq!(f.phase, IdlePhase::Suspend);
        assert_eq!(c.calls, ["hide"], "TrySuspend must not run");
        assert!(!a.hidden && !a.suspended);
    }

    #[test]
    fn an_immediate_hide_is_not_repeated_by_the_suspend() {
        let (mut a, mut c) = (Applied::default(), Fake::default());
        a.hide(&mut c).unwrap();
        a.hide_and_suspend(&mut c, Instant::now()).unwrap();
        assert_eq!(c.calls, ["hide", "suspend"]);
    }

    #[test]
    fn a_failed_immediate_hide_is_retried_by_the_suspend() {
        let (mut a, mut c) = (Applied::default(), Fake::failing(&["hide"]));
        assert!(a.hide(&mut c).is_err());
        c.failing.clear();
        a.hide_and_suspend(&mut c, Instant::now()).unwrap();
        assert_eq!(c.calls, ["hide", "hide", "suspend"]);
    }

    #[test]
    fn a_failed_try_suspend_leaves_the_controller_hidden_not_suspended() {
        let (mut a, mut c) = (Applied::default(), Fake::failing(&["suspend"]));
        assert!(a.hide_and_suspend(&mut c, Instant::now()).is_err());
        assert!(a.hidden && !a.suspended);
        // Resume then shows only.
        c.calls.clear();
        assert_eq!(
            a.resume_and_show(&mut c, Instant::now()).unwrap(),
            ResumeStep::Nothing
        );
        assert_eq!(c.calls, ["show"]);
    }

    #[test]
    fn resume_is_resume_then_show() {
        let (mut a, mut c) = (Applied::default(), Fake::default());
        let t0 = Instant::now();
        a.hide_and_suspend(&mut c, t0).unwrap();
        c.calls.clear();
        let step = a
            .resume_and_show(&mut c, t0 + Duration::from_secs(5))
            .unwrap();
        assert_eq!(c.calls, ["resume", "show"]);
        assert_eq!(step, ResumeStep::Resumed(Duration::from_secs(5)));
        assert!(!a.hidden && !a.suspended);
    }

    #[test]
    fn a_failed_resume_means_no_show_and_an_error_then_retries() {
        let (mut a, mut c) = (Applied::default(), Fake::default());
        a.hide_and_suspend(&mut c, Instant::now()).unwrap();
        c.calls.clear();
        c.failing.insert("resume");
        let f = a.resume_and_show(&mut c, Instant::now()).unwrap_err();
        assert_eq!(f.phase, IdlePhase::Resume);
        assert_eq!(c.calls, ["resume"], "no show after a failed resume");
        assert!(a.hidden && a.suspended, "still pending");
        c.failing.clear();
        c.calls.clear();
        assert!(matches!(
            a.resume_and_show(&mut c, Instant::now()),
            Ok(ResumeStep::Resumed(_))
        ));
        assert_eq!(c.calls, ["resume", "show"]);
    }

    #[test]
    fn a_failed_show_is_surfaced_and_retried_on_the_next_request_without_resuming_again() {
        let (mut a, mut c) = (Applied::default(), Fake::default());
        a.hide_and_suspend(&mut c, Instant::now()).unwrap();
        c.calls.clear();
        c.failing.insert("show");
        let f = a.resume_and_show(&mut c, Instant::now()).unwrap_err();
        assert_eq!(f.phase, IdlePhase::Resume);
        assert_eq!(c.calls, ["resume", "show"]);
        assert!(a.hidden && !a.suspended);
        c.failing.clear();
        c.calls.clear();
        let step = a.resume_and_show(&mut c, Instant::now()).unwrap();
        assert_eq!(c.calls, ["show"]);
        assert!(
            matches!(step, ResumeStep::Resumed(_)),
            "resumed hook still fires once"
        );
        // And a further request is a no-op.
        c.calls.clear();
        assert_eq!(
            a.resume_and_show(&mut c, Instant::now()).unwrap(),
            ResumeStep::Nothing
        );
        assert!(c.calls.is_empty());
    }

    #[test]
    fn resume_with_nothing_suspended_makes_no_call() {
        let (mut a, mut c) = (Applied::default(), Fake::default());
        assert_eq!(
            a.resume_and_show(&mut c, Instant::now()).unwrap(),
            ResumeStep::Nothing
        );
        assert!(c.calls.is_empty());
    }

    #[test]
    fn a_hide_alone_is_undone_by_a_show_alone() {
        let (mut a, mut c) = (Applied::default(), Fake::default());
        a.hide(&mut c).unwrap();
        c.calls.clear();
        let step = a.resume_and_show(&mut c, Instant::now()).unwrap();
        assert_eq!(c.calls, ["show"]);
        assert_eq!(a.classify(true, step), ResumeStep::Shown);
    }

    #[test]
    fn a_failed_suspend_is_forgotten_by_the_state_machine() {
        let mut s = IdleState::default();
        let (g, _) = s.on_hidden();
        assert_eq!(s.on_timer(g), IdleAction::SuspendNow);
        s.on_suspend_failed();
        assert_eq!(s.snapshot(), (true, false));
        let (_, action) = s.on_hidden();
        assert_eq!(action, IdleAction::HideControllerAndStartTimer);
    }

    #[test]
    fn the_transition_hook_fires_once_per_call_and_is_silent_when_unset() {
        let count = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (c2, s2) = (count.clone(), seen.clone());
        let host = IdleHost::new(Duration::from_secs(1)).with_on_transition(Some(Arc::new(
            move |t: &IdleTransition| {
                c2.fetch_add(1, Ordering::SeqCst);
                s2.lock().unwrap().push(t.clone());
            },
        )));
        host.transition(
            IdleTransitionKind::Suspended,
            "main",
            Duration::from_secs(30),
        );
        host.transition(IdleTransitionKind::Resumed, "main", Duration::from_secs(4));
        assert_eq!(count.load(Ordering::SeqCst), 2);
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].kind, IdleTransitionKind::Suspended);
        assert_eq!(seen[0].window, "main");
        assert_eq!(seen[0].elapsed, Duration::from_secs(30));
        assert_eq!(seen[1].kind, IdleTransitionKind::Resumed);
        // Unset: no panic, no effect.
        IdleHost::new(Duration::from_secs(1)).transition(
            IdleTransitionKind::Suspended,
            "main",
            Duration::ZERO,
        );
    }

    /// One minimise-only cycle as the shell drives it: tracker, state machine, then the controller.
    struct Cycle {
        tracker: MinimizeTracker,
        state: IdleState,
        applied: Applied,
        c: Fake,
    }

    impl Cycle {
        fn new() -> Self {
            Self {
                tracker: MinimizeTracker::default(),
                state: IdleState::default(),
                applied: Applied::default(),
                c: Fake::default(),
            }
        }
        /// A minimise (`true`) or restore (`false`); `timer` lets the idle timer fire after a minimise.
        fn minimised(&mut self, minimised: bool, timer: bool) {
            match self.tracker.step(minimised) {
                MinimizeStep::Start => {
                    let (g, _) = self.state.on_hidden();
                    self.applied.hide(&mut self.c).unwrap();
                    if timer && self.state.on_timer(g) == IdleAction::SuspendNow {
                        self.applied
                            .hide_and_suspend(&mut self.c, Instant::now())
                            .unwrap();
                    }
                }
                MinimizeStep::End => {
                    self.tracker.clear();
                    self.state.on_visible();
                    self.applied
                        .resume_and_show(&mut self.c, Instant::now())
                        .unwrap();
                }
                MinimizeStep::None => {}
            }
        }
    }

    #[test]
    fn minimise_only_trigger_ignores_hides_and_always_accepts_a_minimise() {
        for trigger in [IdleTrigger::HiddenOrMinimised, IdleTrigger::Minimised] {
            assert!(source_starts_idle(trigger, IdleSource::Minimise));
        }
        assert!(source_starts_idle(
            IdleTrigger::HiddenOrMinimised,
            IdleSource::Hide
        ));
        assert!(!source_starts_idle(IdleTrigger::Minimised, IdleSource::Hide));
        let o = IdleOptions::default().on_minimise(true);
        assert_eq!(o.trigger, IdleTrigger::Minimised);
        assert!(o.minimise_enabled() && !o.start_on_minimize);
        assert!(!IdleOptions::default().minimise_enabled());
        assert_eq!(o.on_minimise(false).trigger, IdleTrigger::HiddenOrMinimised);
    }

    #[test]
    fn minimise_then_restore_suspends_and_resumes_in_order() {
        let mut m = Cycle::new();
        m.minimised(true, true);
        assert_eq!(m.c.calls, ["hide", "suspend"]);
        m.c.calls.clear();
        m.minimised(false, false);
        assert_eq!(m.c.calls, ["resume", "show"]);
        assert_eq!(m.state.snapshot(), (false, false));
    }

    #[test]
    fn a_double_minimise_changes_nothing_the_second_time() {
        let mut m = Cycle::new();
        m.minimised(true, true);
        m.c.calls.clear();
        m.minimised(true, true);
        assert!(m.c.calls.is_empty());
        m.minimised(false, false);
        assert_eq!(m.c.calls, ["resume", "show"]);
    }

    #[test]
    fn a_restore_before_the_timer_only_shows_and_never_resumes() {
        let mut m = Cycle::new();
        m.minimised(true, false);
        m.minimised(false, false);
        assert_eq!(m.c.calls, ["hide", "show"]);
    }

    #[test]
    fn a_restore_with_no_minimise_makes_no_controller_call() {
        let mut m = Cycle::new();
        m.minimised(false, false);
        assert!(m.c.calls.is_empty());
    }

    #[test]
    fn the_minimise_trigger_honours_a_per_hide_delay() {
        let host = Duration::from_secs(30);
        assert_eq!(
            minimize_action(host, MinimizeStep::Start, None),
            MinimizeAction::Hide { delay: host }
        );
        assert_eq!(
            minimize_action(host, MinimizeStep::Start, Some(Duration::from_secs(2))),
            MinimizeAction::Hide {
                delay: Duration::from_secs(2)
            }
        );
        assert_eq!(
            minimize_action(
                host,
                MinimizeStep::Start,
                Some(Duration::from_secs(10 * 24 * 3600))
            ),
            MinimizeAction::Hide {
                delay: MAX_SUSPEND_AFTER
            }
        );
        assert_eq!(
            minimize_action(host, MinimizeStep::End, Some(Duration::ZERO)),
            MinimizeAction::Visible
        );
        assert_eq!(
            minimize_action(host, MinimizeStep::None, None),
            MinimizeAction::None
        );
    }
}
