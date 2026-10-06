//! Startup / shutdown order and RunEvent routing.
//!
//! Startup (plugin `setup`, then Tauri creates windows):
//!
//! 1. single-instance arbitration (a second launch forwards argv and exits before anything heavy)
//! 2. shared state managed, 3. native menu, 4. tray
//! 5. engine started off the UI thread (never blocks first paint), 6. watchdog
//! 7. each webview becomes ready: native hardening, saved geometry restored, window shown
//!    (unless launched with `--background`)
//! 8. deep links queued from argv are delivered when the frontend calls `rk_shell_ready`
//!
//! Shutdown (`quit` -> `ExitRequested` -> `Exit`): mark exiting (new engine calls refused), save
//! window geometry, stop the engine (bounded), remove the tray, release the single-instance
//! service.
//!
//! Donors: HeardRight `tray.rs` Quit (engine shutdown before exit), `hub_window` (close hides,
//! Dock reopen restores), ViewRight `RunEvent::Reopen`, CodeRight `window_state` save hooks.

use std::sync::atomic::Ordering;

use tauri::{AppHandle, Emitter, Manager, RunEvent, WindowEvent, Wry};

use crate::{ShellState, EVENT_DEEP_LINK, EVENT_MENU, EVENT_SECOND_INSTANCE};

/// True when this launch asked to stay in the tray (`--background`).
pub fn launched_in_background() -> bool {
    std::env::args().any(|a| a == crate::autostart::BACKGROUND_ARG)
}

/// QA-only launch policy. The harness sets this before the frontend exists so every
/// shell-owned show/reopen path can preserve the user's foreground app.
pub fn qa_hidden() -> bool {
    std::env::var_os("RIGHTKIT_QA_HIDDEN").as_deref() == Some(std::ffi::OsStr::new("1"))
}

pub fn qa_background() -> bool {
    std::env::var_os("RIGHTKIT_QA_BACKGROUND").as_deref() == Some(std::ffi::OsStr::new("1"))
}

pub fn quit(app: &AppHandle<Wry>) {
    app.state::<ShellState>().exiting.store(true, Ordering::SeqCst);
    app.exit(0);
}

pub(crate) fn deliver_links<'a>(app: &AppHandle<Wry>, raw: impl IntoIterator<Item = &'a str>) {
    let state = app.state::<ShellState>();
    state.deep.push_all(raw, &state.schemes);
    flush_links(app);
}

pub(crate) fn flush_links(app: &AppHandle<Wry>) {
    let state = app.state::<ShellState>();
    if !state.frontend_ready.load(Ordering::SeqCst) {
        return;
    }
    for link in state.deep.drain() {
        let _ = app.emit(EVENT_DEEP_LINK, link);
    }
}

pub(crate) fn on_second_instance(app: &AppHandle<Wry>, argv: &[String], cwd: &str) {
    let _ = app.emit(EVENT_SECOND_INSTANCE, serde_json::json!({"argv": argv, "cwd": cwd}));
    deliver_links(app, argv.iter().skip(1).map(String::as_str));
    crate::windows::reveal(app, None);
}

pub(crate) fn on_menu_event(app: &AppHandle<Wry>, id: &str) {
    let state = app.state::<ShellState>();
    let _ = app.emit(EVENT_MENU, serde_json::json!({"id": id}));
    if let Some(cb) = &state.on_menu {
        cb(app, id);
    }
}

pub(crate) fn run_event(app: &AppHandle<Wry>, event: &RunEvent) {
    let state = app.state::<ShellState>();
    match event {
        RunEvent::WindowEvent { label, event, .. } => match event {
            WindowEvent::Moved(_) | WindowEvent::Resized(_) => {
                state.windows.schedule_save(app.clone(), label.clone());
                if matches!(event, WindowEvent::Resized(_)) {
                    if let Some(w) = app.get_webview_window(label) {
                        state.traffic.reapply(&w);
                    }
                }
            }
            WindowEvent::Focused(true) => {
                if let Some(w) = app.get_webview_window(label) {
                    state.traffic.reapply(&w);
                }
                if *label == state.main_label {
                    crate::idle::note_visible(app);
                }
            }
            WindowEvent::CloseRequested { api, .. } => {
                let _ = state.windows.save_now(app, label);
                if state.close_to_tray && *label == state.main_label && !state.exiting.load(Ordering::SeqCst) {
                    api.prevent_close();
                    if let Some(w) = app.get_webview_window(label) {
                        let _ = w.hide();
                    }
                    crate::idle::note_hidden(app, true);
                }
            }
            _ => {}
        },
        RunEvent::ExitRequested { api, code, .. } => {
            // `code == None`: the last window closed. A tray-resident app keeps running.
            if code.is_none() && state.close_to_tray && !state.exiting.load(Ordering::SeqCst) {
                api.prevent_exit();
            } else {
                state.exiting.store(true, Ordering::SeqCst);
                for w in app.webview_windows().keys() {
                    let _ = state.windows.save_now(app, w);
                }
            }
        }
        RunEvent::Exit => shutdown(app),
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { has_visible_windows, .. } => {
            if !has_visible_windows {
                crate::windows::reveal(app, None);
            }
        }
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        RunEvent::Opened { urls } => {
            let raw: Vec<String> = urls.iter().map(|u| u.to_string()).collect();
            deliver_links(app, raw.iter().map(String::as_str));
            crate::windows::reveal(app, None);
        }
        _ => {}
    }
}

fn shutdown(app: &AppHandle<Wry>) {
    let state = app.state::<ShellState>();
    state.exiting.store(true, Ordering::SeqCst);
    #[cfg(feature = "engine")]
    if let Some(engine) = &state.engine {
        engine.shutdown();
    }
    if state.tray.is_some() {
        let _ = app.remove_tray_by_id(crate::tray::TRAY_ID);
    }
    // Dropping the host releases the single-instance record.
    state.hosted.lock().unwrap_or_else(|e| e.into_inner()).take();
}
