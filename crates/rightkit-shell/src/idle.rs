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
//! reveal) feed the machine automatically; an app that hides its window itself calls
//! [`note_hidden`] / [`note_visible`] (or the `rk_shell_idle_note` command).

use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Wry};

use crate::{ShellState, EVENT_IDLE};

/// Tray / minimise to suspend delay (MailRight, locked 2026-07-06).
pub const DEFAULT_SUSPEND_AFTER: Duration = Duration::from_secs(30);

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
    pub state: Mutex<IdleState>,
    /// How many times the platform suspend / resume action ran (diagnostics, QA).
    pub counters: Mutex<(u32, u32)>,
}

impl IdleHost {
    pub fn new(suspend_after: Duration) -> Self {
        Self {
            suspend_after,
            state: Mutex::new(IdleState::default()),
            counters: Mutex::new((0, 0)),
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

fn emit(app: &AppHandle<Wry>, state: &str) {
    let _ = app.emit(EVENT_IDLE, serde_json::json!({"state": state}));
}

/// The main window was hidden (minimise, tray, app-driven). `controller_already_hidden` skips the
/// redundant WebView2 visibility call on close-to-tray, where the OS hid the window already.
pub fn note_hidden(app: &AppHandle<Wry>, controller_already_hidden: bool) {
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
    emit(app, "hidden");
    if !controller_already_hidden {
        platform::set_visible(app, false);
    }
    let (app, delay) = (app.clone(), host.suspend_after);
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
                platform::suspend(&app);
                emit(&app, "suspended");
            }
        });
}

/// The main window is visible again.
pub fn note_visible(app: &AppHandle<Wry>) {
    let state = app.state::<ShellState>();
    let Some(host) = &state.idle else { return };
    let action = host
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .on_visible();
    match action {
        IdleAction::ShowController => {
            platform::set_visible(app, true);
            emit(app, "visible");
        }
        IdleAction::ResumeAndShowController => {
            host.counters.lock().unwrap_or_else(|e| e.into_inner()).1 += 1;
            platform::resume(app);
            platform::set_visible(app, true);
            emit(app, "resumed");
        }
        _ => {}
    }
}

#[cfg(windows)]
mod platform {
    use tauri::{AppHandle, Manager, Wry};
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_3;
    use windows::core::Interface;

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

    pub fn suspend(app: &AppHandle<Wry>) {
        if let Some(w) = main_window(app) {
            let _ = w.with_webview(|webview| {
                // SAFETY: COM calls on the live controller inside the webview closure.
                unsafe {
                    if let Ok(core) = webview.controller().CoreWebView2() {
                        if let Ok(core3) = core.cast::<ICoreWebView2_3>() {
                            let handler = webview2_com::TrySuspendCompletedHandler::create(
                                Box::new(|error, _suspended| {
                                    error?;
                                    Ok(())
                                }),
                            );
                            let _ = core3.TrySuspend(&handler);
                        }
                    }
                }
            });
        }
    }

    pub fn resume(app: &AppHandle<Wry>) {
        if let Some(w) = main_window(app) {
            let _ = w.with_webview(|webview| {
                // SAFETY: COM calls on the live controller inside the webview closure.
                unsafe {
                    if let Ok(core) = webview.controller().CoreWebView2() {
                        if let Ok(core3) = core.cast::<ICoreWebView2_3>() {
                            let _ = core3.Resume();
                        }
                    }
                }
            });
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use tauri::{AppHandle, Wry};
    pub fn set_visible(_: &AppHandle<Wry>, _: bool) {}
    pub fn suspend(_: &AppHandle<Wry>) {}
    pub fn resume(_: &AppHandle<Wry>) {}
}
