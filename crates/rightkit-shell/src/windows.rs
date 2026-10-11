//! Window creation and lifecycle helpers: hardened builders, reveal / toggle, secondary windows.
//!
//! Donors: CodeRight `os_shell/windows` (bounded labels and dimensions, app-relative URLs only,
//! lifecycle event), HeardRight `hub_window` (reveal = show + unminimize + focus; macOS needs
//! `app.show()` first), ViewRight (hidden-until-first-paint).

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::webview::NewWindowResponse;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder, Wry};

use crate::lifecycle::{reveal_mode, RevealMode, RevealSource};
use crate::nav::WindowOpenDecision;
use crate::opener::open_checked;
use crate::{ShellState, EVENT_WINDOW};

/// App event announcing initial window placement (`phase` `"begin"` / `"done"`), so a
/// control server can hold viewport requests until placement and state restore have
/// landed instead of racing them. Same name `rightkit-control` listens on. Announcing
/// never shows, orders or focuses a window.
pub const EVENT_PLACEMENT: &str = "rk-shell:placement";

fn announce_placement(app: &AppHandle<Wry>, label: &str, phase: &str) {
    let _ = app.emit(
        EVENT_PLACEMENT,
        serde_json::json!({"label": label, "phase": phase}),
    );
}

/// Run `place` (window-state restore and any other initial placement) between the
/// `begin` and `done` announcements. `done` is queued on the main thread behind the
/// size and position calls `place` made, so it is not announced before they apply.
pub fn with_placement_announced(app: &AppHandle<Wry>, label: &str, place: impl FnOnce()) {
    announce_placement(app, label, "begin");
    place();
    let (a, l) = (app.clone(), label.to_owned());
    if app
        .run_on_main_thread(move || announce_placement(&a, &l, "done"))
        .is_err()
    {
        announce_placement(app, label, "done");
    }
}

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
    // A never-show host's windows are never created focused.
    let b = if crate::lifecycle::never_show(app) {
        b.focused(false)
    } else {
        b
    };
    crate::frameless::style(b, state.frameless)
}

/// Create (or reveal) a secondary window described by `spec`.
pub fn open(app: &AppHandle<Wry>, spec: &WindowSpec) -> Result<String, String> {
    let label = safe_label(&spec.label).ok_or("invalid window label")?;
    if let Some(w) = app.get_webview_window(&label) {
        reveal_with(app, Some(&label), RevealSource::OpenWindow);
        return Ok(w.label().to_owned());
    }
    let title = spec
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(&app.state::<ShellState>().app_name)
        .to_owned();
    let focused = reveal_mode(app, RevealSource::OpenWindow) == RevealMode::Foreground
        && spec.focused.unwrap_or(true);
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
    show_window(
        app,
        &window,
        RevealSource::OpenWindow,
        RevealStyle::ShowOnly,
    )?;
    Ok(label)
}

/// The window calls one reveal makes, so every path is testable (and recordable) without a window.
pub trait RevealTarget {
    /// Before a foreground reveal of the main window: back to a Regular app, run the host's
    /// visibility callback.
    fn prepare(&mut self);
    /// macOS `app.show()`.
    fn app_show(&mut self);
    fn unminimize(&mut self) -> Result<(), String>;
    fn show(&mut self) -> Result<(), String>;
    fn focus(&mut self) -> Result<(), String>;
    /// QA background: native order-back without activation.
    fn order_back(&mut self) -> Result<(), String>;
}

/// Which calls a foreground reveal makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevealStyle {
    /// prepare, app show, unminimise, show, focus (errors ignored): `reveal`, tray, reopen.
    Full,
    /// prepare, show, focus (errors returned): the `rk_shell_window` `show` command.
    ShowFocus,
    /// show only (errors returned): show-on-ready and a newly opened window.
    ShowOnly,
}

/// Do exactly what `mode` allows. `Ok(true)` when the window was brought up, `Ok(false)` when
/// nothing was (forbidden, or a `Full` order-back that failed). `Forbidden` makes no call at all.
pub fn apply_reveal<T: RevealTarget>(
    mode: RevealMode,
    style: RevealStyle,
    target: &mut T,
) -> Result<bool, String> {
    match mode {
        RevealMode::Forbidden => Ok(false),
        RevealMode::Background => match target.order_back() {
            Ok(()) => Ok(true),
            Err(_) if style == RevealStyle::Full => Ok(false),
            Err(e) => Err(e),
        },
        RevealMode::Foreground => {
            match style {
                RevealStyle::Full => {
                    target.prepare();
                    target.app_show();
                    let _ = target.unminimize();
                    let _ = target.show();
                    let _ = target.focus();
                }
                RevealStyle::ShowFocus => {
                    target.prepare();
                    target.show()?;
                    target.focus()?;
                }
                RevealStyle::ShowOnly => target.show()?,
            }
            Ok(true)
        }
    }
}

struct WindowReveal<'a> {
    app: &'a AppHandle<Wry>,
    w: &'a WebviewWindow<Wry>,
}

impl RevealTarget for WindowReveal<'_> {
    fn prepare(&mut self) {
        // Back to a Regular app first (a hide-to-background host is an Accessory app while
        // hidden), so the activation below can bring a Dock icon and menu bar with it.
        if self.w.label() == self.app.state::<ShellState>().main_label {
            crate::lifecycle::note_visibility(self.app, crate::lifecycle::Visibility::Revealed);
        }
    }
    fn app_show(&mut self) {
        #[cfg(target_os = "macos")]
        let _ = self.app.show();
    }
    fn unminimize(&mut self) -> Result<(), String> {
        self.w.unminimize().map_err(|e| e.to_string())
    }
    fn show(&mut self) -> Result<(), String> {
        self.w.show().map_err(|e| e.to_string())
    }
    fn focus(&mut self) -> Result<(), String> {
        self.w.set_focus().map_err(|e| e.to_string())
    }
    fn order_back(&mut self) -> Result<(), String> {
        // Native AppKit ordering must run on the Tauri main thread. Window commands
        // arrive on rightkit-service connection threads; dispatch synchronously there
        // so errors are returned to the caller after WindowServer applies the order.
        #[cfg(target_os = "macos")]
        {
            dispatch_native(self.w, order_back_native)
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err("background native ordering is unimplemented on this platform".into())
        }
    }
}

/// Bring `w` up as the guard allows for `source`. The only function that touches a window for a
/// reveal; every caller reaches it through [`crate::lifecycle::reveal_mode`].
pub(crate) fn show_window(
    app: &AppHandle<Wry>,
    w: &WebviewWindow<Wry>,
    source: RevealSource,
    style: RevealStyle,
) -> Result<bool, String> {
    let mode = reveal_mode(app, source);
    apply_reveal(mode, style, &mut WindowReveal { app, w })
}

fn reveal_window(app: &AppHandle<Wry>, w: &WebviewWindow<Wry>, source: RevealSource) {
    let shown = show_window(app, w, source, RevealStyle::Full).unwrap_or(false);
    if shown && w.label() == app.state::<ShellState>().main_label {
        crate::idle::note_visible(app);
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
        dispatch_native(w, |w| {
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
        })
    }
    #[cfg(not(target_os = "macos"))]
    Ok(WindowReadback {
        visible: w.is_visible().unwrap_or(false),
        focused: w.is_focused().unwrap_or(false),
        main: false,
        level: None,
    })
}

/// Show + unminimize + focus, as the host's visibility policy allows (never-show makes this a
/// no-op). `None` targets the main window.
pub fn reveal(app: &AppHandle<Wry>, label: Option<&str>) {
    reveal_with(app, label, RevealSource::Api);
}

/// [`reveal`] naming why, for the guard.
pub fn reveal_with(app: &AppHandle<Wry>, label: Option<&str>, source: RevealSource) {
    let main = app.state::<ShellState>().main_label.clone();
    let label = label.unwrap_or(&main);
    if let Some(w) = app.get_webview_window(label) {
        reveal_window(app, &w, source);
    }
}

/// Hide when visible and focused-or-visible, otherwise reveal (tray left-click behaviour).
pub fn toggle(app: &AppHandle<Wry>, label: &str) {
    if let Some(w) = app.get_webview_window(label) {
        if w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false) {
            let _ = w.hide();
            if w.label() == app.state::<ShellState>().main_label {
                crate::idle::note_hidden(app, false);
                crate::lifecycle::note_visibility(app, crate::lifecycle::Visibility::Hidden);
            }
        } else {
            reveal_window(app, &w, RevealSource::Tray);
        }
    }
}
