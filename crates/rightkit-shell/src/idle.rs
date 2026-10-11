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
//! Hide/show paths owned by the shell (`rk_shell_window hide|show`, tray toggle, close-to-tray,
//! reveal) feed the machine automatically (a minimise too, with `IdleOptions::start_on_minimize`); an app that hides its window itself calls
//! [`note_hidden`] / [`note_visible`] (or the `rk_shell_idle_note` command).

use std::fmt::Display;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

/// Per-host idle options (see [`crate::ShellBuilder::webview_idle_options`]). The default is the
/// behaviour `webview_idle(DEFAULT_SUSPEND_AFTER)` has always had.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdleOptions {
    /// Hidden this long before the webview is suspended (clamped to [`MAX_SUSPEND_AFTER`]).
    pub suspend_after: Duration,
    /// Also start the idle timer when the main window is minimised, not only on a hide the shell or
    /// host reports (`note_hidden`). Restoring the window ends it. Default off.
    pub start_on_minimize: bool,
    pub controller_hide: ControllerHide,
    pub events: IdleEvents,
}

impl Default for IdleOptions {
    fn default() -> Self {
        Self {
            suspend_after: DEFAULT_SUSPEND_AFTER,
            start_on_minimize: false,
            controller_hide: ControllerHide::Immediate,
            events: IdleEvents::Always,
        }
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

    /// `(hidden, suspended)`.
    pub fn snapshot(&self) -> (bool, bool) {
        (self.hidden, self.suspended)
    }
}

/// Runtime half kept in [`ShellState`].
pub struct IdleHost {
    pub suspend_after: Duration,
    pub options: IdleOptions,
    pub minimize: MinimizeTracker,
    pub state: Mutex<IdleState>,
    /// How many times the platform suspend / resume action ran (diagnostics, QA).
    pub counters: Mutex<(u32, u32)>,
    /// Receives platform suspend / resume failures. `None` keeps them silent (the default).
    pub on_error: Option<IdleErrorCallback>,
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
    let (generation, action) = host
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .on_hidden();
    if action != IdleAction::HideControllerAndStartTimer {
        return;
    }
    emit(app, host, "hidden");
    let plan = hide_plan(host.options.controller_hide, controller_already_hidden);
    if plan.now {
        platform::set_visible(app, false);
    }
    let delay = effective_delay(host.suspend_after, suspend_after);
    let app = app.clone();
    let reporter = host.reporter(&state.main_label);
    let _ = std::thread::Builder::new()
        .name("rk-shell-idle".into())
        .spawn(move || {
            std::thread::sleep(delay);
            let state = app.state::<ShellState>();
            let Some(host) = &state.idle else { return };
            let action = host
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .on_timer(generation);
            if action == IdleAction::SuspendNow {
                host.counters.lock().unwrap_or_else(|e| e.into_inner()).0 += 1;
                if plan.on_timer {
                    platform::set_visible(&app, false);
                }
                platform::suspend(&app, &reporter);
                emit(&app, host, "suspended");
            }
        });
}

/// The main window was minimised (`true`) or restored (`false`). Does nothing unless the host set
/// `IdleOptions::start_on_minimize`. A restore ends only an idle period a minimise began.
pub fn note_minimized(app: &AppHandle<Wry>, minimized: bool) {
    let state = app.state::<ShellState>();
    let Some(host) = &state.idle else { return };
    if !host.options.start_on_minimize {
        return;
    }
    match host.minimize.step(minimized) {
        MinimizeStep::Start => note_hidden_after(app, false, None),
        MinimizeStep::End => note_visible(app),
        MinimizeStep::None => {}
    }
}

/// The main window is visible again.
pub fn note_visible(app: &AppHandle<Wry>) {
    let state = app.state::<ShellState>();
    let Some(host) = &state.idle else { return };
    host.minimize.clear();
    let action = host
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .on_visible();
    match action {
        IdleAction::ShowController => {
            platform::set_visible(app, true);
            emit(app, host, "visible");
        }
        IdleAction::ResumeAndShowController => {
            host.counters.lock().unwrap_or_else(|e| e.into_inner()).1 += 1;
            platform::resume(app, &host.reporter(&state.main_label));
            platform::set_visible(app, true);
            emit(app, host, "resumed");
        }
        _ => {}
    }
}

#[cfg(windows)]
mod platform {
    use crate::winapi::webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_3;
    use crate::winapi::windows::core::Interface;
    use tauri::{AppHandle, Manager, Wry};

    use super::{IdlePhase, Reporter};
    use crate::ShellState;

    fn main_window(app: &AppHandle<Wry>) -> Option<tauri::WebviewWindow<Wry>> {
        let label = app.state::<ShellState>().main_label.clone();
        app.get_webview_window(&label)
    }

    pub fn set_visible(app: &AppHandle<Wry>, visible: bool) {
        if let Some(w) = main_window(app) {
            let _ = w.with_webview(move |webview| {
                // SAFETY: the controller is live for the duration of the closure.
                let _ = unsafe { webview.controller().SetIsVisible(visible) };
            });
        }
    }

    pub fn suspend(app: &AppHandle<Wry>, reporter: &Reporter) {
        let Some(w) = main_window(app) else {
            reporter.fail(IdlePhase::Suspend, "main window not found");
            return;
        };
        let report = reporter.clone();
        let dispatched = w.with_webview(move |webview| {
            // SAFETY: COM calls on the live controller inside the webview closure.
            unsafe {
                let core = match webview.controller().CoreWebView2() {
                    Ok(core) => core,
                    Err(e) => return report.fail(IdlePhase::Suspend, e),
                };
                let core3 = match core.cast::<ICoreWebView2_3>() {
                    Ok(core3) => core3,
                    Err(e) => return report.fail(IdlePhase::Suspend, e),
                };
                let completion = report.clone();
                let handler = crate::winapi::webview2_com::TrySuspendCompletedHandler::create(
                    Box::new(move |error, _suspended| {
                        if let Err(e) = error {
                            completion.fail(IdlePhase::Suspend, e);
                        }
                        Ok(())
                    }),
                );
                if let Err(e) = core3.TrySuspend(&handler) {
                    report.fail(IdlePhase::Suspend, e);
                }
            }
        });
        if let Err(e) = dispatched {
            reporter.fail(IdlePhase::Suspend, e);
        }
    }

    pub fn resume(app: &AppHandle<Wry>, reporter: &Reporter) {
        let Some(w) = main_window(app) else {
            reporter.fail(IdlePhase::Resume, "main window not found");
            return;
        };
        let report = reporter.clone();
        let dispatched = w.with_webview(move |webview| {
            // SAFETY: COM calls on the live controller inside the webview closure.
            unsafe {
                let core = match webview.controller().CoreWebView2() {
                    Ok(core) => core,
                    Err(e) => return report.fail(IdlePhase::Resume, e),
                };
                let core3 = match core.cast::<ICoreWebView2_3>() {
                    Ok(core3) => core3,
                    Err(e) => return report.fail(IdlePhase::Resume, e),
                };
                if let Err(e) = core3.Resume() {
                    report.fail(IdlePhase::Resume, e);
                }
            }
        });
        if let Err(e) = dispatched {
            reporter.fail(IdlePhase::Resume, e);
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use tauri::{AppHandle, Wry};

    use super::Reporter;
    pub fn set_visible(_: &AppHandle<Wry>, _: bool) {}
    pub fn suspend(_: &AppHandle<Wry>, _: &Reporter) {}
    pub fn resume(_: &AppHandle<Wry>, _: &Reporter) {}
}
