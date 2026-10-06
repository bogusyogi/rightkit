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

pub fn safe_label(raw: &str) -> Option<String> {
    let c = raw.trim();
    (!c.is_empty() && c.len() <= 80 && c.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))).then(|| c.to_owned())
}

fn app_path(path: Option<&str>) -> String {
    let raw = path.unwrap_or("index.html").trim().trim_start_matches('/');
    if raw.is_empty() || raw.contains("..") || raw.contains('\\') || raw.contains('\0') || raw.contains("://") {
        "index.html".to_owned()
    } else {
        raw.to_owned()
    }
}

fn dim(v: Option<f64>, default: f64, min: f64) -> f64 {
    v.filter(|v| v.is_finite() && *v > 0.0).unwrap_or(default).max(min)
}

/// A `WebviewWindowBuilder` carrying the shell policy: popup handling through [`crate::nav`],
/// frameless chrome when the shell was configured for it, hidden until restored/shown.
/// Navigation, init scripts and native hardening come from the shell plugin for every webview.
pub fn builder<'a>(app: &'a AppHandle<Wry>, label: &str, url: WebviewUrl) -> WebviewWindowBuilder<'a, Wry, AppHandle<Wry>> {
    let state = app.state::<ShellState>();
    let nav = Arc::clone(&state.nav);
    let opener = Arc::clone(&state.opener);
    let b = WebviewWindowBuilder::new(app, label, url).visible(false).on_new_window(move |url, features| {
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
    let title = spec.title.as_deref().map(str::trim).filter(|t| !t.is_empty()).unwrap_or(&app.state::<ShellState>().app_name).to_owned();
    let focused = spec.focused.unwrap_or(true);
    let window = builder(app, &label, WebviewUrl::App(app_path(spec.path.as_deref()).into()))
        .title(title)
        .inner_size(dim(spec.width, 900.0, MIN_WIDTH), dim(spec.height, 640.0, MIN_HEIGHT))
        .min_inner_size(MIN_WIDTH, MIN_HEIGHT)
        .always_on_top(spec.always_on_top.unwrap_or(false))
        .focused(focused)
        .build()
        .map_err(|e| format!("create window {label}: {e}"))?;
    let _ = app.emit(EVENT_WINDOW, serde_json::json!({"label": label, "kind": "created"}));
    let _ = window.show();
    Ok(label)
}

fn reveal_window(app: &AppHandle<Wry>, w: &WebviewWindow<Wry>) {
    #[cfg(target_os = "macos")]
    let _ = app.show();
    let _ = (app,);
    let _ = w.unminimize();
    let _ = w.show();
    let _ = w.set_focus();
    if w.label() == app.state::<ShellState>().main_label {
        crate::idle::note_visible(app);
    }
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
