//! Tauri commands the frontend bridge (`@rightkit/shell`) calls. Register them with
//! [`crate::handler!`]. They are application commands, so they need no ACL capability entry.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Manager, State, WebviewWindow, Wry};

use crate::clipboard::{Board, ClipboardContent};
use crate::tray::TrayUpdate;
use crate::windows::WindowSpec;
use crate::{b64, ShellState};

#[tauri::command]
pub fn rk_shell_open_external(url: String, state: State<'_, ShellState>) -> Result<(), String> {
    crate::opener::open_checked(state.opener.as_ref(), &url)
}

/// The frontend is listening: deliver queued deep links. Returns launch facts.
#[tauri::command]
pub fn rk_shell_ready(app: AppHandle<Wry>, state: State<'_, ShellState>) -> Value {
    state.frontend_ready.store(true, Ordering::SeqCst);
    crate::lifecycle::flush_links(&app);
    #[cfg(feature = "engine")]
    let engine = state.engine.as_ref().map(|e| e.lifecycle());
    #[cfg(not(feature = "engine"))]
    let engine: Option<Value> = None;
    json!({"background": crate::lifecycle::launched_in_background(), "appId": state.app_id, "appName": state.app_name, "engine": engine})
}

/// Diagnostics for tests and support: menu rows, tray, instance role, nav policy.
#[tauri::command]
pub fn rk_shell_status(app: AppHandle<Wry>, state: State<'_, ShellState>) -> Value {
    let tray = app.tray_by_id(crate::tray::TRAY_ID).is_some();
    // Where the OS placed the status item (screen points). Present only once AppKit/the shell
    // gave the item real geometry, which makes this an OS-derived fact, not a registry entry.
    let tray_rect = app.tray_by_id(crate::tray::TRAY_ID).and_then(|t| t.rect().ok().flatten()).map(|r| {
        let (p, z) = (r.position.to_physical::<f64>(1.0), r.size.to_physical::<f64>(1.0));
        json!({"x": p.x, "y": p.y, "w": z.width, "h": z.height})
    });
    let menu: Vec<Value> = state.menu_rows.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(p, l)| json!({"path": p, "label": l})).collect();
    #[cfg(any(debug_assertions, feature = "devtools"))]
    let devtools_open = app.get_webview_window(&state.main_label).map(|w| w.is_devtools_open()).unwrap_or(false);
    #[cfg(not(any(debug_assertions, feature = "devtools")))]
    let devtools_open = false;
    let windows: Vec<Value> = app
        .webview_windows()
        .iter()
        .map(|(label, w)| json!({"label": label, "visible": w.is_visible().unwrap_or(false), "title": w.title().unwrap_or_default(), "native": crate::windows::readback(w).map(|native| json!(native)).unwrap_or_else(|e| json!({"error": e}))}))
        .collect();
    let menu_enabled: BTreeMap<String, bool> =
        state.menu_items.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(id, i)| (id.clone(), i.is_enabled().unwrap_or(false))).collect();
    json!({
        "appId": state.app_id,
        "instance": *state.single_instance.lock().unwrap_or_else(|e| e.into_inner()),
        "tray": tray,
        "trayRect": tray_rect,
        "menu": menu,
        "windows": windows,
        "menuEnabled": menu_enabled,
        "stateDir": state.windows.dir(),
        "devtoolsOpen": devtools_open,
        "streamScheme": state.stream_scheme,
        "cacheRepair": state.cache_repair.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|(p, o)| json!({"dir": p, "outcome": o})),
    })
}

#[tauri::command]
pub fn rk_shell_window(action: String, window: WebviewWindow<Wry>) -> Result<(), String> {
    let r: Result<(), String> = match action.as_str() {
        "minimize" => window.minimize().map_err(|e| e.to_string()),
        "maximize" => window.maximize().map_err(|e| e.to_string()),
        "unmaximize" => window.unmaximize().map_err(|e| e.to_string()),
        "toggle-maximize" => {
            if window.is_maximized().unwrap_or(false) { window.unmaximize() } else { window.maximize() }.map_err(|e| e.to_string())
        }
        "fullscreen" => window.set_fullscreen(!window.is_fullscreen().unwrap_or(false)).map_err(|e| e.to_string()),
        "hide" => {
            let r = window.hide();
            if window.label() == "main" {
                crate::idle::note_hidden(window.app_handle(), false);
            }
            r.map_err(|e| e.to_string())
        }
        "show" => {
            let r = if crate::lifecycle::qa_background() {
                crate::windows::show_background_aware(&window)
            } else {
                window.show().and_then(|_| window.set_focus()).map_err(|e| e.to_string())
            };
            if window.label() == "main" {
                crate::idle::note_visible(window.app_handle());
            }
            r
        }
        "close" => window.close().map_err(|e| e.to_string()),
        "start-drag" => window.start_dragging().map_err(|e| e.to_string()),
        other => return Err(format!("unknown window action {other}")),
    };
    r
}

#[tauri::command]
pub fn rk_shell_open_window(spec: WindowSpec, app: AppHandle<Wry>) -> Result<String, String> {
    crate::windows::open(&app, &spec)
}

#[tauri::command]
pub fn rk_shell_set_tray(update: TrayUpdate, app: AppHandle<Wry>) -> Result<(), String> {
    crate::tray::update(&app, &update)
}

#[tauri::command]
pub fn rk_shell_set_menu_item_enabled(id: String, enabled: bool, state: State<'_, ShellState>) -> Result<(), String> {
    let items = state.menu_items.lock().unwrap_or_else(|e| e.into_inner());
    items.get(&id).ok_or_else(|| format!("no menu item {id}"))?.set_enabled(enabled).map_err(|e| e.to_string())
}

/// `enabled = None` reads; `Some(b)` sets and returns the new status.
#[tauri::command]
pub fn rk_shell_autostart(enabled: Option<bool>, state: State<'_, ShellState>) -> Result<crate::autostart::AutostartStatus, String> {
    match enabled {
        Some(on) => crate::autostart::set_enabled(&state.autostart, on),
        None => Ok(crate::autostart::status(&state.autostart)),
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardPayload {
    pub text: Option<String>,
    pub html: Option<String>,
    /// PNG, base64.
    pub png: Option<String>,
    #[serde(default)]
    pub files: Vec<String>,
    /// format name -> base64.
    #[serde(default)]
    pub custom: BTreeMap<String, String>,
    /// Private pasteboard name (macOS); omit for the general clipboard.
    pub board: Option<String>,
}

fn board_of(p: &Option<String>) -> Board {
    p.clone().map(Board::Named).unwrap_or(Board::General)
}

#[tauri::command]
pub fn rk_shell_clipboard_read(board: Option<String>) -> Result<ClipboardPayload, String> {
    let c = crate::clipboard::read(&board_of(&board))?;
    Ok(ClipboardPayload {
        text: c.text,
        html: c.html,
        png: c.png.map(|p| b64::encode(&p)),
        files: c.files,
        custom: c.custom.into_iter().map(|(k, v)| (k, b64::encode(&v))).collect(),
        board,
    })
}

#[tauri::command]
pub fn rk_shell_clipboard_write(payload: ClipboardPayload) -> Result<(), String> {
    let mut custom = BTreeMap::new();
    for (k, v) in &payload.custom {
        custom.insert(k.clone(), b64::decode(v).ok_or_else(|| format!("custom format {k} is not base64"))?);
    }
    let png = match &payload.png {
        Some(s) => Some(b64::decode(s).ok_or("png is not base64")?),
        None => None,
    };
    crate::clipboard::write(
        &board_of(&payload.board),
        &ClipboardContent { text: payload.text, html: payload.html, png, files: payload.files, custom },
    )
}

#[tauri::command]
pub async fn rk_shell_engine_call(method: String, params: Option<Value>, app: AppHandle<Wry>) -> Result<Value, String> {
    #[cfg(feature = "engine")]
    {
        // Engine calls block on the sidecar: keep them off the async executor threads.
        tauri::async_runtime::spawn_blocking(move || {
            let state = app.state::<ShellState>();
            let engine = state.engine.clone().ok_or_else(|| "no engine configured".to_string())?;
            engine.call(&app, &method, params.unwrap_or(Value::Null)).map_err(|e| format!("{}: {}", e.code, e.message))
        })
        .await
        .map_err(|e| e.to_string())?
    }
    #[cfg(not(feature = "engine"))]
    {
        let _ = (method, params, app);
        Err("engine feature disabled".into())
    }
}

#[tauri::command]
pub fn rk_shell_traffic_lights(dx: f64, dy: f64, window: WebviewWindow<Wry>, state: State<'_, ShellState>) {
    state.traffic.set_offset(&window, dx, dy);
}

#[tauri::command]
pub fn rk_shell_quit(app: AppHandle<Wry>) {
    crate::lifecycle::quit(&app);
}

/// Mirror the app theme into native chrome (window appearance; tray template icons follow it).
#[tauri::command]
pub fn rk_shell_set_theme(theme: String, window: WebviewWindow<Wry>) -> Result<(), String> {
    let t = match theme.as_str() {
        "light" => Some(tauri::Theme::Light),
        "dark" => Some(tauri::Theme::Dark),
        "system" => None,
        other => return Err(format!("unknown theme {other}")),
    };
    window.set_theme(t).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------------------------
// Notifications, presence / DnD, idle, path broker, recovery. These reject with the typed
// `ShellError` envelope (`{code, message}`), unlike the older string-rejecting commands above.
// ---------------------------------------------------------------------------------------------

use crate::error::ShellError;
use crate::notify::{NotificationRequest, NotifyOutcome, NotifyStatus};
use crate::presence::{AutoPrefs, Environment, PresenceState, Rect, WindowObservation};

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, ShellError> + Send + 'static) -> Result<T, ShellError> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| ShellError::Other(e.to_string()))?
}

/// Post a native notification. Resolves `{delivered:false, suppressed:"dnd"}` while Do-Not-Disturb
/// (manual or an automatic meeting) is in effect. Runs off the UI thread: the first call may wait on
/// the OS permission prompt.
#[tauri::command]
pub async fn rk_shell_notify(request: NotificationRequest, app: AppHandle<Wry>) -> Result<NotifyOutcome, ShellError> {
    let handle = app.clone();
    let outcome = blocking(move || {
        let state = handle.state::<ShellState>();
        crate::notify::post(&request, &state.notifications, state.presence.effective_dnd())
    })
    .await?;
    // A frontmost app's banner is suppressed by the OS; ask for the gentler attention cue instead.
    if outcome.delivered {
        let main = app.state::<ShellState>().main_label.clone();
        if let Some(w) = app.get_webview_window(&main) {
            if w.is_focused().unwrap_or(false) {
                let _ = w.request_user_attention(Some(tauri::UserAttentionType::Informational));
            }
        }
    }
    Ok(outcome)
}

/// Authorization plus what the OS reports as delivered to this app (the QA read-back).
#[tauri::command]
pub async fn rk_shell_notify_status(app: AppHandle<Wry>) -> Result<NotifyStatus, ShellError> {
    blocking(move || Ok(crate::notify::status(&app.state::<ShellState>().notifications))).await
}

/// Remove delivered notifications by id; `None` or an empty list removes all of this app's.
#[tauri::command]
pub async fn rk_shell_notify_clear(ids: Option<Vec<String>>) -> Result<(), ShellError> {
    blocking(move || crate::notify::clear_delivered(&ids.unwrap_or_default())).await
}

#[tauri::command]
pub fn rk_shell_badge(count: i64, app: AppHandle<Wry>, state: State<'_, ShellState>) -> Result<u32, ShellError> {
    let n = state.notifications.set_badge(count);
    if let Some(w) = app.get_webview_window(&state.main_label) {
        w.set_badge_count((n > 0).then_some(i64::from(n))).map_err(|e| ShellError::Other(format!("set badge count: {e}")))?;
    }
    Ok(n)
}

#[tauri::command]
pub fn rk_shell_set_dnd(enabled: bool, app: AppHandle<Wry>, state: State<'_, ShellState>) -> PresenceState {
    state.presence.set_manual_dnd(enabled);
    let s = state.presence.snapshot();
    let _ = tauri::Emitter::emit(&app, crate::EVENT_PRESENCE, s);
    s
}

#[tauri::command]
pub fn rk_shell_presence(state: State<'_, ShellState>) -> PresenceState {
    state.presence.snapshot()
}

/// Opt in or out of the automatic controls. Turning one off clears its stale automatic state.
#[tauri::command]
pub fn rk_shell_presence_prefs(prefs: AutoPrefs, state: State<'_, ShellState>) -> PresenceState {
    state.presence.set_prefs(prefs);
    if !prefs.auto_hide_fullscreen || !prefs.auto_dnd_meetings {
        let cur = state.presence.snapshot();
        state.presence.set_auto(crate::presence::AutoState {
            hidden_for_fullscreen: cur.auto_hidden && prefs.auto_hide_fullscreen,
            dnd_for_meeting: cur.auto_dnd && prefs.auto_dnd_meetings,
        });
    }
    state.presence.snapshot()
}

/// Sample the real OS now: `{environment, windows}` (count of visible windows seen).
#[tauri::command]
pub async fn rk_shell_presence_probe(app: AppHandle<Wry>) -> Result<Value, ShellError> {
    blocking(move || {
        let name = app.state::<ShellState>().app_name.clone();
        let (env, windows) = crate::presence::detect_live(&name).map_err(ShellError::Unsupported)?;
        Ok(json!({"environment": env, "windows": windows, "supported": crate::presence::controls_supported()}))
    })
    .await
}

/// Run the detection policy over an injected observation. With `apply`, the result is reconciled
/// with the current preferences and becomes the automatic state (what the poller would do), so a
/// meeting can be simulated without a real conferencing app.
#[tauri::command]
pub fn rk_shell_presence_evaluate(monitor: Rect, windows: Vec<WindowObservation>, apply: Option<bool>, state: State<'_, ShellState>) -> Value {
    let env: Environment = crate::presence::detect(monitor, &windows, &state.app_name);
    if apply.unwrap_or(false) {
        state.presence.apply_observation(crate::presence::reconcile(state.presence.prefs(), env));
    }
    json!({"environment": env, "state": state.presence.snapshot()})
}

#[tauri::command]
pub fn rk_shell_idle_status(app: AppHandle<Wry>) -> crate::idle::IdleStatus {
    crate::idle::status(&app)
}

/// App-driven hide/show of the main window for the idle machine.
#[tauri::command]
pub fn rk_shell_idle_note(hidden: bool, app: AppHandle<Wry>) {
    if hidden {
        crate::idle::note_hidden(&app, false);
    } else {
        crate::idle::note_visible(&app);
    }
}

/// Canonicalise `path` and require a broker grant (`write: true` checks a save target).
#[tauri::command]
pub fn rk_shell_path_require(path: String, write: Option<bool>, state: State<'_, ShellState>) -> Result<String, ShellError> {
    if write.unwrap_or(false) {
        state.broker.require_write_target(&path)
    } else {
        state.broker.require_file(&path)
    }
}

#[tauri::command]
pub fn rk_shell_path_revoke(path: String, state: State<'_, ShellState>) -> Result<(), ShellError> {
    state.broker.revoke_path(&path)
}

/// Trade a one-shot recent-file id (minted in Rust) for a grant.
#[tauri::command]
pub fn rk_shell_path_grant_recent(id: String, state: State<'_, ShellState>) -> Result<String, ShellError> {
    state.broker.grant_known(&id)
}

/// Re-grant a save-as target the app just wrote.
#[tauri::command]
pub fn rk_shell_path_regrant_saved(path: String, state: State<'_, ShellState>) -> Result<String, ShellError> {
    state.broker.regrant_saved_target(&path)
}

/// Mint the stream URL for a granted file. Rejects `broker_denied` for anything not granted.
#[tauri::command]
pub fn rk_shell_stream_grant(path: String, state: State<'_, ShellState>) -> Result<Value, ShellError> {
    let scheme = state.stream_scheme.clone().ok_or_else(|| ShellError::Unsupported("no stream protocol configured".into()))?;
    let id = state.broker.grant_stream(&path)?;
    let url = if cfg!(windows) { format!("http://{scheme}.localhost/{id}") } else { format!("{scheme}://localhost/{id}") };
    Ok(json!({"id": id, "url": url}))
}

fn recovery_of<'a>(state: &'a ShellState) -> Result<&'a crate::recovery::RecoveryStore, ShellError> {
    state.recovery.as_ref().ok_or_else(|| ShellError::Unsupported("setup recovery is not configured".into()))
}

#[tauri::command]
pub fn rk_shell_recovery_save(snapshot: String, state: State<'_, ShellState>) -> Result<(), ShellError> {
    recovery_of(&state)?.save(&snapshot, &state.broker)
}

#[tauri::command]
pub fn rk_shell_recovery_load(state: State<'_, ShellState>) -> Result<Option<String>, ShellError> {
    recovery_of(&state)?.load(&state.broker)
}

#[tauri::command]
pub fn rk_shell_recovery_clear(state: State<'_, ShellState>) -> Result<(), ShellError> {
    recovery_of(&state)?.clear()
}
