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
/// landed instead of racing them. Same name `rightkit-qa-driver` listens on. Announcing
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
    /// Minimum inner width in logical pixels. Default 320 (unchanged); a non-finite or non-positive
    /// value is ignored. The initial width is raised to at least this.
    pub min_width: Option<f64>,
    /// Minimum inner height in logical pixels. Default 240 (unchanged).
    pub min_height: Option<f64>,
    /// Override native decorations after the shell's frameless policy (`Some(false)` for a
    /// borderless pill or onboarding window on a framed shell, `Some(true)` for the reverse).
    pub decorations: Option<bool>,
    /// Centre on the primary/current monitor. Wins over `x` / `y`.
    pub center: Option<bool>,
    /// Open fullscreen.
    pub fullscreen: Option<bool>,
    /// Keep the window out of the taskbar / Dock-switcher where the platform supports it.
    pub skip_taskbar: Option<bool>,
    /// Initial outer position in logical pixels (needs both `x` and `y`). It is corrected to a
    /// visible monitor when it would leave the window off-screen.
    pub x: Option<f64>,
    pub y: Option<f64>,
    /// Label of an open window to make this window's native parent (a print preview over the main
    /// window). An unknown or malformed label fails the open rather than being ignored.
    pub parent: Option<String>,
    /// Whether the user may resize the window. Default: resizable, as before.
    pub resizable: Option<bool>,
    /// Native window shadow where the platform has one. Default: on, as before.
    pub shadow: Option<bool>,
    /// `Some(false)` creates the window and leaves it hidden (no reveal, no focus); the app shows it
    /// later with its own call. Default: revealed on open, as before.
    pub visible: Option<bool>,
    /// Whether the user may maximize the window. Default: maximizable, as before.
    pub maximizable: Option<bool>,
    /// Whether the user may minimize the window. Default: minimizable, as before.
    pub minimizable: Option<bool>,
    /// A transparent webview background. Default: opaque, as before.
    pub transparent: Option<bool>,
    /// Background colour `[r, g, b, a]` painted before the first page paint (no white flash).
    /// Default: the platform's own background, as before.
    pub background_color: Option<[u8; 4]>,
    /// A modal child of `parent`: the parent is disabled while this window lives, and re-enabled and
    /// refocused when it is destroyed (also after a crash). Needs `parent`. Default: not modal.
    pub modal: Option<bool>,
    /// A one-shot payload the new window takes once with `rk_shell_take_payload`. Default: none.
    pub payload: Option<serde_json::Value>,
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

/// The effective minimum inner size of `spec`: its own values, else the shell default.
pub fn spec_min_size(spec: &WindowSpec) -> (f64, f64) {
    let pick = |v: Option<f64>, default: f64| {
        v.filter(|v| v.is_finite() && *v > 0.0)
            .map_or(default, |v| v.min(16_384.0))
    };
    (
        pick(spec.min_width, MIN_WIDTH),
        pick(spec.min_height, MIN_HEIGHT),
    )
}

/// Move a window rectangle (logical pixels) back on-screen. A rectangle whose title strip is
/// reachable on some monitor (at least `CLAMP_MARGIN_X` wide, `CLAMP_MARGIN_Y` tall inside it)
/// is returned unchanged; otherwise it is clamped onto the monitor nearest its centre. No monitors:
/// unchanged.
pub fn correct_offscreen(
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    monitors: &[crate::window_state::MonitorSnapshot],
) -> (f64, f64) {
    use crate::window_state::{CLAMP_MARGIN_X, CLAMP_MARGIN_Y};
    if monitors.is_empty() || !x.is_finite() || !y.is_finite() {
        return match (monitors.first(), x.is_finite() && y.is_finite()) {
            (Some(m), false) => (m.x, m.y),
            _ => (x, y),
        };
    }
    let reachable = |m: &crate::window_state::MonitorSnapshot| {
        x + width > m.x + CLAMP_MARGIN_X
            && x < m.x + m.width - CLAMP_MARGIN_X
            && y >= m.y
            && y < m.y + m.height - CLAMP_MARGIN_Y
    };
    if monitors.iter().any(reachable) {
        return (x, y);
    }
    let (cx, cy) = (x + width / 2.0, y + height / 2.0);
    let target = monitors
        .iter()
        .min_by(|a, b| {
            let d = |m: &crate::window_state::MonitorSnapshot| {
                (m.x + m.width / 2.0 - cx).powi(2) + (m.y + m.height / 2.0 - cy).powi(2)
            };
            d(a).total_cmp(&d(b))
        })
        .expect("non-empty monitors");
    // Clamp against the work area (#1022): a window never lands under the taskbar or menu bar.
    let (wx, wy, ww, wh) = (
        target.work_x,
        target.work_y,
        target.work_width,
        target.work_height,
    );
    let min_x = wx - (width - CLAMP_MARGIN_X).max(0.0);
    let max_x = wx + ww - CLAMP_MARGIN_X;
    let max_y = wy + wh - CLAMP_MARGIN_Y;
    (
        if min_x <= max_x {
            x.clamp(min_x, max_x)
        } else {
            wx
        },
        if wy <= max_y { y.clamp(wy, max_y) } else { wy },
    )
}

/// How [`fit_to_work_area`] treats a window (#1022).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorkAreaFit {
    /// Shrink a window larger than the work area to fit it.
    pub shrink_to_fit: bool,
    /// A bottom edge within this many logical pixels of the work area's bottom (the taskbar) is
    /// re-centred vertically instead of left docked to the bar.
    pub recenter_within_px: f64,
    /// Leave a maximised window alone.
    pub skip_maximized: bool,
}

impl Default for WorkAreaFit {
    fn default() -> Self {
        Self {
            shrink_to_fit: true,
            recenter_within_px: 32.0,
            skip_maximized: true,
        }
    }
}

/// The window rectangle before and after a fit, and the work area it was fitted to, as
/// `[x, y, width, height]` logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct FitOutcome {
    pub from: [f64; 4],
    pub to: [f64; 4],
    pub work: [f64; 4],
}

/// Fit the live window `label` inside its monitor's work area (#1022): size no larger than the work
/// area (when `shrink_to_fit`), x and y clamped inside it, and a bottom edge within
/// `recenter_within_px` of the work area's bottom re-centred vertically. A maximised window is left
/// alone when `skip_maximized`. The window is moved only when the rectangle changes.
pub fn fit_to_work_area(
    app: &AppHandle<Wry>,
    label: &str,
    fit: WorkAreaFit,
) -> Result<FitOutcome, String> {
    let window = app
        .get_webview_window(label)
        .ok_or_else(|| format!("unknown window {label}"))?;
    let monitor = window
        .current_monitor()
        .map_err(|e| e.to_string())?
        .ok_or("window is on no monitor")?;
    let snapshot = crate::window_state::MonitorSnapshot::from_monitor(&monitor);
    let scale = snapshot.scale;
    let position = window.outer_position().map_err(|e| e.to_string())?;
    let size = window.outer_size().map_err(|e| e.to_string())?;
    let from = [
        f64::from(position.x) / scale,
        f64::from(position.y) / scale,
        f64::from(size.width) / scale,
        f64::from(size.height) / scale,
    ];
    let work = [
        snapshot.work_x,
        snapshot.work_y,
        snapshot.work_width,
        snapshot.work_height,
    ];
    let mut to = from;
    let maximized = window.is_maximized().unwrap_or(false);
    if !(fit.skip_maximized && maximized) {
        let [wx, wy, ww, wh] = work;
        let [x, y, mut width, mut height] = from;
        if fit.shrink_to_fit {
            width = width.min(ww);
            height = height.min(wh);
        }
        let x = x.clamp(wx, (wx + ww - width).max(wx));
        let mut y = y.clamp(wy, (wy + wh - height).max(wy));
        let bottom = y + height;
        if (wy + wh - bottom).abs() <= fit.recenter_within_px {
            y = wy + (wh - height) / 2.0;
        }
        to = [x, y, width, height];
    }
    if to != from {
        window
            .set_size(tauri::LogicalSize::new(to[2], to[3]))
            .map_err(|e| e.to_string())?;
        window
            .set_position(tauri::LogicalPosition::new(to[0], to[1]))
            .map_err(|e| e.to_string())?;
    }
    Ok(FitOutcome { from, to, work })
}

fn dim(v: Option<f64>, default: f64, min: f64) -> f64 {
    v.filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(default)
        .max(min)
}

/// Route the page's new-window requests (`target=_blank`, middle-click, "open in new window") of a
/// window the shell did not build (for example one created from `tauri.conf.json`) to the app's
/// [`crate::ShellBuilder::on_new_window`] hook, the way [`builder`] does for its own windows. The
/// hook gets the requested URL: `true` means the app handled it and WebView2 opens nothing;
/// `false` lets WebView2 proceed as it would without the shell. Does nothing when no hook is set.
///
/// Windows (WebView2) only. Tauri's wry hook (`on_new_window`) can be set only while a window is
/// being built, so on other platforms this is a no-op: build the window with [`builder`] there.
/// Do not call it for a window made by [`builder`], whose hook already routes.
pub fn route_new_windows(window: &WebviewWindow<Wry>) {
    #[cfg(windows)]
    {
        use crate::winapi::webview2_com::NewWindowRequestedEventHandler;
        use crate::winapi::windows::core::PWSTR;

        let Some(router) = window
            .app_handle()
            .state::<ShellState>()
            .new_window_router
            .clone()
        else {
            return;
        };
        let _ = window.with_webview(move |webview| unsafe {
            let Ok(core) = webview.controller().CoreWebView2() else {
                return;
            };
            let handler = NewWindowRequestedEventHandler::create(Box::new(move |_sender, args| {
                let Some(args) = args else {
                    return Ok(());
                };
                let mut uri = PWSTR::null();
                args.Uri(&mut uri)?;
                let url = if uri.is_null() {
                    String::new()
                } else {
                    uri.to_string().unwrap_or_default()
                };
                if router(&url) {
                    args.SetHandled(true)?;
                }
                Ok(())
            }));
            let mut token: i64 = 0;
            let _ = core.add_NewWindowRequested(&handler, &mut token);
        });
    }
    #[cfg(not(windows))]
    let _ = window;
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
    let router = state.new_window_router.clone();
    let consent = state.protocol_consent.clone();
    let (opener_app, opener_label) = (app.clone(), label.to_owned());
    let b = WebviewWindowBuilder::new(app, label, url)
        .visible(false)
        .on_new_window(move |url, features| {
            // The app's router first: a request it handled never reaches the shell's policy.
            if router.as_ref().is_some_and(|route| route(url.as_str())) {
                return NewWindowResponse::Deny;
            }
            // The opening page: this window's current URL. Unknown (the window is not registered yet)
            // defers to the origin allow-list.
            let from = opener_app
                .get_webview_window(&opener_label)
                .and_then(|w| w.url().ok());
            let decision = nav.decide_window_open_pinned(
                &url,
                features.size().is_some(),
                from.as_ref(),
                nav.pinned_origin.as_deref(),
            );
            match decision {
                WindowOpenDecision::External { url } => {
                    let _ = open_checked(opener.as_ref(), &url);
                    NewWindowResponse::Deny
                }
                WindowOpenDecision::Popup => NewWindowResponse::Allow,
                // Consent for a non-web scheme is an app surface (see `nav::WindowOpenDecision`):
                // without one the shell refuses it. With one, only a page of the pinned origin
                // may ask, and the dialog runs off this thread; the new window is refused either
                // way and an allowed link is handed to the system opener.
                WindowOpenDecision::ProtocolConsent { scheme } => {
                    if let Some(consent) = consent.clone() {
                        if crate::consent::consent_surface(
                            nav.pinned_origin.as_deref(),
                            from.as_ref(),
                        ) == crate::consent::ConsentSurface::Prompt
                        {
                            let (opener, target) = (Arc::clone(&opener), url.to_string());
                            std::thread::spawn(move || {
                                if consent.confirm(&scheme) {
                                    let _ = opener.open(&target);
                                }
                            });
                        }
                    }
                    NewWindowResponse::Deny
                }
                WindowOpenDecision::Deny => NewWindowResponse::Deny,
            }
        });
    #[cfg(windows)]
    let b = match state.webview_data_directory.clone() {
        Some(dir) => b.data_directory(dir),
        None => b,
    };
    // A never-show host's windows are never created focused.
    let b = if crate::lifecycle::never_show(app) {
        b.focused(false)
    } else {
        b
    };
    crate::frameless::style(b, state.frameless)
}

/// Create (or reveal) a secondary window described by `spec`.
/// Whether [`open`] reveals the window it creates. Only `visible: Some(false)` keeps it hidden.
/// Re-enable and refocus the parent of a modal child that was destroyed (#1050). Called on the
/// child's `Destroyed` event, so a crashed or force-closed child releases its parent too.
pub(crate) fn release_modal(app: &AppHandle<Wry>, child_label: &str) {
    let state = app.state::<ShellState>();
    state
        .payloads
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(child_label);
    let parent = state
        .modal_children
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(child_label);
    if let Some(parent) = parent.and_then(|p| app.get_webview_window(&p)) {
        let _ = parent.set_enabled(true);
        let _ = parent.set_focus();
    }
}

pub fn opens_visible(spec: &WindowSpec) -> bool {
    spec.visible != Some(false)
}

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
    let (min_w, min_h) = spec_min_size(spec);
    let (width, height) = (
        dim(spec.width, 900.0, min_w),
        dim(spec.height, 640.0, min_h),
    );
    let mut b = builder(
        app,
        &label,
        WebviewUrl::App(app_path(spec.path.as_deref()).into()),
    )
    .title(title)
    .inner_size(width, height)
    .min_inner_size(min_w, min_h)
    .always_on_top(spec.always_on_top.unwrap_or(false))
    .focused(focused);
    if let Some(d) = spec.decorations {
        b = b.decorations(d);
    }
    if let Some(f) = spec.fullscreen {
        b = b.fullscreen(f);
    }
    if let Some(s) = spec.skip_taskbar {
        b = b.skip_taskbar(s);
    }
    if let Some(r) = spec.resizable {
        b = b.resizable(r);
    }
    if let Some(s) = spec.shadow {
        b = b.shadow(s);
    }
    if let Some(m) = spec.maximizable {
        b = b.maximizable(m);
    }
    if let Some(m) = spec.minimizable {
        b = b.minimizable(m);
    }
    if let Some(t) = spec.transparent {
        b = b.transparent(t);
    }
    if let Some([r, g, bl, a]) = spec.background_color {
        b = b.background_color(tauri::window::Color(r, g, bl, a));
    }
    if let Some(raw) = spec.parent.as_deref() {
        let parent_label = safe_label(raw).ok_or("invalid parent window label")?;
        let parent = app
            .get_webview_window(&parent_label)
            .ok_or_else(|| format!("unknown parent window {parent_label}"))?;
        b = b
            .parent(&parent)
            .map_err(|e| format!("parent window {parent_label}: {e}"))?;
    }
    if spec.center == Some(true) {
        b = b.center();
    } else if let (Some(x), Some(y)) = (spec.x, spec.y) {
        let monitors: Vec<_> = app
            .available_monitors()
            .unwrap_or_default()
            .iter()
            .map(crate::window_state::MonitorSnapshot::from_monitor)
            .collect();
        let (x, y) = correct_offscreen(x, y, width, height, &monitors);
        b = b.position(x, y);
    }
    // A one-shot payload waits for the window to take it (`rk_shell_take_payload`).
    if let Some(payload) = spec.payload.clone() {
        app.state::<ShellState>()
            .payloads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(label.clone(), payload);
    }
    let window = b
        .build()
        .map_err(|e| format!("create window {label}: {e}"))?;
    // A modal child disables its parent until the child is destroyed (`release_modal`).
    if spec.modal == Some(true) {
        if let Some(parent_label) = spec.parent.as_deref().and_then(safe_label) {
            if let Some(parent) = app.get_webview_window(&parent_label) {
                let _ = parent.set_enabled(false);
                app.state::<ShellState>()
                    .modal_children
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(label.clone(), parent_label);
            }
        }
    }
    let _ = app.emit(
        EVENT_WINDOW,
        serde_json::json!({"label": label, "kind": "created"}),
    );
    if opens_visible(spec) {
        show_window(
            app,
            &window,
            RevealSource::OpenWindow,
            RevealStyle::ShowOnly,
        )?;
    }
    Ok(label)
}

/// Make this app the frontmost application (macOS: `NSApplication activateIgnoringOtherApps:`, which
/// brings a background app forward when an open-document Apple event or a second launch arrives,
/// where `set_focus` alone does not). A no-op on other platforms. The reveal paths call it; an app
/// may call it too.
pub fn activate_app(app: &AppHandle<Wry>) {
    #[cfg(target_os = "macos")]
    {
        let _ = app.run_on_main_thread(activate_native);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
    }
}

#[cfg(target_os = "macos")]
fn activate_native() {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    let Some(class) = AnyClass::get(c"NSApplication") else {
        return;
    };
    // SAFETY: `sharedApplication` and `activateIgnoringOtherApps:` are long-standing NSApplication
    // selectors, called on the main thread (dispatched by `activate_app`) on the shared instance.
    unsafe {
        let shared: *mut AnyObject = msg_send![class, sharedApplication];
        if shared.is_null() {
            return;
        }
        let _: () = msg_send![shared, activateIgnoringOtherApps: true];
    }
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
        {
            let _ = self.app.show();
            activate_app(self.app);
        }
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
        // arrive on rightkit-daemon connection threads; dispatch synchronously there
        // so errors are returned to the caller after WindowServer applies the order.
        #[cfg(target_os = "macos")]
        {
            dispatch_native(self.w, order_back_native)
        }
        #[cfg(windows)]
        {
            show_without_activation(self.w)
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            Err("background native ordering is unimplemented on this platform".into())
        }
    }
}

/// Windows: show the window without activating it (`SW_SHOWNOACTIVATE`), so a background reveal
/// (a hidden QA launch, a background start) brings the window up without taking focus. Calls
/// user32 directly, so no Windows-crate feature is needed.
#[cfg(windows)]
fn show_without_activation(w: &WebviewWindow<Wry>) -> Result<(), String> {
    const SW_SHOWNOACTIVATE: i32 = 4;
    #[link(name = "user32")]
    extern "system" {
        fn ShowWindow(hwnd: *mut std::ffi::c_void, cmd: i32) -> i32;
    }
    let hwnd = w.hwnd().map_err(|e| e.to_string())?;
    // SAFETY: `hwnd` is the live top-level window Tauri created for `w`; ShowWindow only changes its
    // visibility and takes no ownership of it.
    unsafe {
        ShowWindow(hwnd.0, SW_SHOWNOACTIVATE);
    }
    Ok(())
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
    if let Some(w) = app
        .get_webview_window(label)
        .or_else(|| ensure_main(app, label))
    {
        reveal_window(app, &w, source);
    }
}

/// The main window, recreated when a `DestroyAndRecreate` host closed it (#414). `None` for any other
/// label or host, or when the window cannot be built (the caller then reveals nothing, as before).
fn ensure_main(app: &AppHandle<Wry>, label: &str) -> Option<WebviewWindow<Wry>> {
    let state = app.state::<ShellState>();
    if !state.destroy_on_close || label != state.main_label {
        return None;
    }
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == label)
        .cloned();
    let (url, width, height, title) = match config {
        Some(c) => (c.url.clone(), c.width, c.height, c.title.clone()),
        None => (
            WebviewUrl::App("index.html".into()),
            1200.0,
            760.0,
            state.app_name.clone(),
        ),
    };
    let window = builder(app, label, url)
        .title(title)
        .inner_size(width, height)
        .build()
        .ok()?;
    let _ = app.emit(
        EVENT_WINDOW,
        serde_json::json!({"label": label, "kind": "created"}),
    );
    Some(window)
}

/// Hide `window`, or destroy it when the shell runs `WebviewLifecycle::DestroyOnHide` and it is the
/// main window (#955). Returns `true` when it was destroyed: the caller then skips what only a
/// live hidden webview needs (the idle controller). The main window's geometry is saved first,
/// because a destroy raises no close request.
pub(crate) fn hide_or_destroy(
    app: &AppHandle<Wry>,
    window: &WebviewWindow<Wry>,
) -> tauri::Result<bool> {
    let state = app.state::<ShellState>();
    if state.destroy_on_hide && window.label() == state.main_label {
        if state.modules.contains(crate::Module::WindowState) {
            let _ = state.windows.save_now(app, window.label());
        }
        window.destroy()?;
        return Ok(true);
    }
    window.hide()?;
    Ok(false)
}

/// Hide when visible and focused-or-visible, otherwise reveal (tray left-click behaviour).
pub fn toggle(app: &AppHandle<Wry>, label: &str) {
    if app.get_webview_window(label).is_none() {
        // A destroyed main window (`DestroyAndRecreate`) comes back on the tray click.
        if let Some(w) = ensure_main(app, label) {
            reveal_window(app, &w, RevealSource::Tray);
        }
        return;
    }
    if let Some(w) = app.get_webview_window(label) {
        if w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false) {
            let destroyed = hide_or_destroy(app, &w).unwrap_or(false);
            if w.label() == app.state::<ShellState>().main_label {
                if !destroyed {
                    crate::idle::note_hidden(app, false);
                }
                crate::lifecycle::note_visibility(app, crate::lifecycle::Visibility::Hidden);
            }
        } else {
            reveal_window(app, &w, RevealSource::Tray);
        }
    }
}
