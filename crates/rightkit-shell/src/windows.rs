//! Window creation and lifecycle helpers: hardened builders, reveal / toggle, secondary windows.
//!
//! Donors: CodeRight `os_shell/windows` (bounded labels and dimensions, app-relative URLs only,
//! lifecycle event), HeardRight `hub_window` (reveal = show + unminimize + focus; macOS needs
//! `app.show()` first), ViewRight (hidden-until-first-paint).

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::webview::NewWindowResponse;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder, Wry};

use crate::nav::WindowOpenDecision;
use crate::opener::open_checked;
use crate::{ShellState, EVENT_WINDOW};

const MIN_WIDTH: f64 = 320.0;
const MIN_HEIGHT: f64 = 240.0;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowSpec {
    pub label: String,
    pub title: Option<String>,
    /// App-relative path (e.g. `mini.html`); never an absolute or remote URL.
    pub path: Option<String>,
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub focused: Option<bool>,
    /// Always on top (mini windows, pills).
    pub always_on_top: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WindowReadback {
    pub visible: bool,
    pub focused: bool,
    pub main: bool,
    pub level: Option<i64>,
}

pub fn safe_label(raw: &str) -> Option<String> {
    let c = raw.trim();
    (!c.is_empty()
        && c.len() <= 80
        && c.chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_')))
    .then(|| c.to_owned())
}

fn app_path(path: Option<&str>) -> String {
    let raw = path.unwrap_or("index.html").trim().trim_start_matches('/');
    if raw.is_empty()
        || raw.contains("..")
        || raw.contains('\\')
        || raw.contains('\0')
        || raw.contains("://")
    {
        "index.html".to_owned()
    } else {
        raw.to_owned()
    }
}

fn dim(v: Option<f64>, default: f64, min: f64) -> f64 {
    v.filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(default)
        .max(min)
}

/// A `WebviewWindowBuilder` carrying the shell policy: popup handling through [`crate::nav`],
/// frameless chrome when the shell was configured for it, hidden until restored/shown.
/// Navigation, init scripts and native hardening come from the shell plugin for every webview.
pub fn builder<'a>(
    app: &'a AppHandle<Wry>,
    label: &str,
    url: WebviewUrl,
) -> WebviewWindowBuilder<'a, Wry, AppHandle<Wry>> {
    let state = app.state::<ShellState>();
    let nav = Arc::clone(&state.nav);
    let opener = Arc::clone(&state.opener);
    let b = WebviewWindowBuilder::new(app, label, url)
        .visible(false)
        .on_new_window(move |url, features| {
            match nav.decide_window_open(&url, features.size().is_some(), None) {
                WindowOpenDecision::External { url } => {
                    let _ = open_checked(opener.as_ref(), &url);
                    NewWindowResponse::Deny
                }
                WindowOpenDecision::Popup => NewWindowResponse::Allow,
                WindowOpenDecision::Deny => NewWindowResponse::Deny,
            }
        });
    crate::frameless::style(b, state.frameless)
}

/// Create (or reveal) a secondary window described by `spec`.
pub fn open(app: &AppHandle<Wry>, spec: &WindowSpec) -> Result<String, String> {
    let label = safe_label(&spec.label).ok_or("invalid window label")?;
    if let Some(w) = app.get_webview_window(&label) {
        reveal(app, Some(&label));
        return Ok(w.label().to_owned());
    }
    let title = spec
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(&app.state::<ShellState>().app_name)
        .to_owned();
    let focused = !crate::lifecycle::qa_background() && spec.focused.unwrap_or(true);
    let window = builder(
        app,
        &label,
        WebviewUrl::App(app_path(spec.path.as_deref()).into()),
    )
    .title(title)
    .inner_size(
        dim(spec.width, 900.0, MIN_WIDTH),
        dim(spec.height, 640.0, MIN_HEIGHT),
    )
    .min_inner_size(MIN_WIDTH, MIN_HEIGHT)
    .always_on_top(spec.always_on_top.unwrap_or(false))
    .focused(focused)
    .build()
    .map_err(|e| format!("create window {label}: {e}"))?;
    let _ = app.emit(
        EVENT_WINDOW,
        serde_json::json!({"label": label, "kind": "created"}),
    );
    show_background_aware(&window)?;
    Ok(label)
}

fn reveal_window(app: &AppHandle<Wry>, w: &WebviewWindow<Wry>) {
    if crate::lifecycle::qa_background() {
        // Native order-back is the only QA reveal operation; never unminimize or focus.
        if show_background_aware(w).is_err() {
            return;
        }
    } else {
        #[cfg(target_os = "macos")]
        let _ = app.show();
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
    if w.label() == app.state::<ShellState>().main_label {
        crate::idle::note_visible(app);
    }
}

/// Show a QA window without activating it or taking keyboard focus. On macOS the
/// native order-back keeps its real pixels available to AX/readback while preserving
/// whatever app the user is using. Production launches use ordinary `show` semantics.
pub(crate) fn show_background_aware(w: &WebviewWindow<Wry>) -> Result<(), String> {
    if !crate::lifecycle::qa_background() {
        return w.show().map_err(|e| e.to_string());
    }
    // Native AppKit ordering must run on the Tauri main thread. Window commands
    // arrive on rightkit-service connection threads; dispatch synchronously there
    // so errors are returned to the caller after WindowServer applies the order.
    #[cfg(target_os = "macos")]
    {
        dispatch_native(w, order_back_native)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = w;
        Err("background native ordering is unimplemented on this platform".into())
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn dispatch_native<T, F>(w: &WebviewWindow<Wry>, f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&WebviewWindow<Wry>) -> Result<T, String> + Send + 'static,
{
    let Some(state) = w.app_handle().try_state::<ShellState>() else {
        return Err("shell state unavailable for native dispatch".into());
    };
    if state.exiting.load(std::sync::atomic::Ordering::Acquire) {
        return Err("native dispatch rejected during shutdown".into());
    }
    if std::thread::current().id() == state.main_thread_id {
        return f(w);
    }
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cancelled_task = std::sync::Arc::clone(&cancelled);
    let app = w.app_handle().clone();
    let window = w.clone();
    w.run_on_main_thread(move || {
        if cancelled_task.load(std::sync::atomic::Ordering::Acquire) {
            let _ = tx.send(Err("native dispatch cancelled".into()));
            return;
        }
        let Some(state) = app.try_state::<ShellState>() else {
            let _ = tx.send(Err("shell state unavailable for native dispatch".into()));
            return;
        };
        if state.exiting.load(std::sync::atomic::Ordering::Acquire) {
            let _ = tx.send(Err("native dispatch rejected during shutdown".into()));
            return;
        }
        let _ = tx.send(f(&window));
    })
    .map_err(|e| e.to_string())?;
    match rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(result) => result,
        Err(e) => {
            cancelled.store(true, std::sync::atomic::Ordering::Release);
            Err(format!("main-thread native dispatch: {e}"))
        }
    }
}

// Dispatched to the main thread only on macOS.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn order_back_native(w: &WebviewWindow<Wry>) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::NSWindow;
        let ptr = w.ns_window().map_err(|e| e.to_string())?;
        if ptr.is_null() {
            return Err("native window handle unavailable".into());
        }
        // SAFETY: Tauri returns the live NSWindow for this window; used synchronously.
        let native = unsafe { &*(ptr as *const NSWindow) };
        // Keep QA windows below normal user windows even if AppKit reorders them later.
        native.setLevel(-1);
        native.orderBack(None);
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = w;
        Err("background native ordering is unimplemented on this platform".into())
    }
}

/// Read native window visibility/order after a show or order operation. macOS values
/// come from NSWindow, so Tauri's logical `is_visible` cannot mask a failed native order.
pub fn readback(w: &WebviewWindow<Wry>) -> Result<WindowReadback, String> {
    #[cfg(target_os = "macos")]
    {
        return dispatch_native(w, |w| {
            use objc2_app_kit::NSWindow;
            let ptr = w.ns_window().map_err(|e| e.to_string())?;
            if ptr.is_null() {
                return Err("native window handle unavailable".into());
            }
            // SAFETY: Tauri returns the live NSWindow for this window; used synchronously.
            let native = unsafe { &*(ptr as *const NSWindow) };
            Ok(WindowReadback {
                visible: native.isVisible(),
                focused: native.isKeyWindow(),
                main: native.isMainWindow(),
                level: Some(native.level() as i64),
            })
        });
    }
    #[cfg(not(target_os = "macos"))]
    Ok(WindowReadback {
        visible: w.is_visible().unwrap_or(false),
        focused: w.is_focused().unwrap_or(false),
        main: false,
        level: None,
    })
}

/// Show + unminimize + focus. `None` targets the main window.
pub fn reveal(app: &AppHandle<Wry>, label: Option<&str>) {
    let main = app.state::<ShellState>().main_label.clone();
    let label = label.unwrap_or(&main);
    if let Some(w) = app.get_webview_window(label) {
        reveal_window(app, &w);
    }
}

/// Hide when visible and focused-or-visible, otherwise reveal (tray left-click behaviour).
pub fn toggle(app: &AppHandle<Wry>, label: &str) {
    if let Some(w) = app.get_webview_window(label) {
        if w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false) {
            let _ = w.hide();
            if w.label() == app.state::<ShellState>().main_label {
                crate::idle::note_hidden(app, false);
            }
        } else {
            reveal_window(app, &w);
        }
    }
}
