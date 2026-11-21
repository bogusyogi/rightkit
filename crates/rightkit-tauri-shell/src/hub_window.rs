//! Window presentation mechanics for an app whose main window is a long-lived, hidden-and-shown
//! "hub": build it, show it, hide it, suspend its renderer, restore it to a work area, hand it over
//! to and from an anchor window, and retire an overlay window.
//!
//! Donor: HeardRight `hub_window.rs`. Everything here is mechanism. The host supplies every
//! identity and decision as an input: window labels, titles, routes, sizes, colours, event names,
//! the event sink `fn(&str, &str)` (which receives `(event, detail)`), and callbacks for anything
//! that reaches the host's own runtime, pill, settings or licence code.
//!
//! * [`window_snapshot`]: `visible=.. pos=.. size=..` for diagnostics.
//! * [`RenderingPolicy`], [`suspend_rendering`], [`resume_rendering`]: the idle-suspend wiring.
//! * [`PillOrigin`]: the "next show originates from the anchor" flag and the launch-done latch.
//! * [`fit_work_area`]: keep a restored window inside its monitor's work area (Windows).
//! * [`SurfaceSize`], [`apply_surface_size`]: resize, optionally re-centre, and re-apply the resize
//!   and maximise policy of a window for the surface it is about to present, before it is shown.
//! * [`show_and_focus`], [`show_only`], [`unminimize`] and the [`macos`] helpers: show, focus, order
//!   front, titlebar policy. Every one of them asks the shell's reveal guard first.
//! * [`build_window`] and [`WindowBuild`]: one bounded builder for the hub and for any window of the
//!   same shape (splash). The window is always built hidden; a visible one is revealed through the
//!   guard.
//! * [`retire_overlay`]: click-through, hide, close for a webview overlay that must never ghost.
//! * [`morph`]: the frame maths, timing and (Windows, feature `overlay`) the native shell that grows
//!   the hub out of, or returns it to, an anchor rectangle.
//! * [`controller`]: show or route the hub, present a launch splash, and hand off the legal gate.
//!
//! # Reveal policy
//!
//! Every function here that can make a window visible, focused, ordered front, activated or
//! animated on screen asks [`reveal_mode`] (the shell's own guard, `lifecycle::reveal_mode`) with a
//! host-supplied [`RevealSource`] and does exactly what the answer allows: [`RevealMode::Forbidden`]
//! (the host's `never_show`, a registered never-show environment variable, or the `reveal_on_reopen`
//! opt-out) makes no call at all; [`RevealMode::Background`] (QA background) orders the window back
//! without activation and never unminimises, focuses, activates or animates;
//! [`RevealMode::Foreground`] is the ordinary reveal. The show/focus of a window goes through the
//! shell's `windows::show_window`; the native paths the shell has no hook for (AppKit order-front,
//! the title-bar opacity restore, the morph's overlay window) read the same guard. This module keeps
//! no copy of the policy.
//!
//! Values that arrive from another process (a route token from a menu-bar helper or a pill bridge)
//! are bounded with [`route_token`] before they become part of a route.

use std::any::Any;
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Manager, Runtime, WebviewUrl, WebviewWindow, Wry};

pub use crate::lifecycle::{reveal_mode, RevealMode, RevealSource};
use crate::windows::RevealStyle;

pub mod controller;
pub mod macos;
pub mod morph;

/// Host diagnostics sink: `(event, detail)`.
pub type EventSink = fn(&str, &str);

/// Longest route [`build_window`] accepts.
pub const MAX_ROUTE_LEN: usize = 2048;
/// Longest token [`route_token`] accepts.
pub const MAX_ROUTE_TOKEN_LEN: usize = 40;
/// Longest window title [`build_window`] accepts, in bytes.
pub const MAX_TITLE_LEN: usize = 256;
/// Largest window dimension (logical pixels) any size input accepts.
pub const MAX_WINDOW_DIMENSION: f64 = 16_384.0;
/// Largest `recenter_within_px` [`fit_work_area`] accepts (logical pixels).
pub const MAX_RECENTER_WITHIN_PX: f64 = 512.0;

/// `Some(raw)` when `raw` is a short ASCII token (`a-z A-Z 0-9 - _ .`), else `None`. Use it on any
/// value that came from another process before it is put into a route or a URL fragment.
pub fn route_token(raw: &str) -> Option<&str> {
    (!raw.is_empty()
        && raw.len() <= MAX_ROUTE_TOKEN_LEN
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
    .then_some(raw)
}

/// `visible=.. pos=.. size=..` for a log line.
pub fn window_snapshot<R: Runtime>(win: &WebviewWindow<R>) -> String {
    let visible = win
        .is_visible()
        .map(|value| value.to_string())
        .unwrap_or_else(|err| format!("err:{err}"));
    let pos = win
        .outer_position()
        .map(|pos| format!("{},{}", pos.x, pos.y))
        .unwrap_or_else(|err| format!("err:{err}"));
    let size = win
        .outer_size()
        .map(|size| format!("{}x{}", size.width, size.height))
        .unwrap_or_else(|err| format!("err:{err}"));
    format!("visible={visible} pos={pos} size={size}")
}

/// What suspending the hub's renderer needs from the host.
pub struct RenderingPolicy<'a> {
    /// The window label this policy governs; any other label is ignored.
    pub label: &'a str,
    /// Runs first on every hide of `label` (stop a native capture thread the suspend would not stop).
    pub on_hidden: &'a dyn Fn(),
    /// `true` keeps the renderer running (a hidden QA run drives the window while it is hidden).
    pub keep_rendering: &'a dyn Fn() -> bool,
}

/// The window `label` is being hidden: run the host hook, then hand the hide to the shell's idle
/// machine (Windows: WebView2 `SetIsVisible(false)` then `TrySuspend`; macOS: nothing to suspend).
/// The shell owns the controller sequencing, the stale-timer guard and the retry of a failed step.
pub fn suspend_rendering(win: &WebviewWindow<Wry>, label: &str, policy: &RenderingPolicy<'_>) {
    if label != policy.label {
        return;
    }
    (policy.on_hidden)();
    if (policy.keep_rendering)() {
        return;
    }
    crate::idle::note_hidden(win.app_handle(), false);
}

/// The window `label` is about to show: resume its renderer (a no-op unless the idle machine
/// suspended it). Nothing resumes when the guard forbids the reveal for `source`: the window is not
/// going to be shown, so the idle machine must not be told it is visible. This function itself shows
/// nothing.
pub fn resume_rendering(
    win: &WebviewWindow<Wry>,
    label: &str,
    governed_label: &str,
    source: RevealSource,
) {
    if label == governed_label && reveal_mode(win.app_handle(), source) != RevealMode::Forbidden {
        crate::idle::note_visible(win.app_handle());
    }
}

/// Whether the next show of the window originates from an anchor window (the pill), and whether
/// the launch flight is over. Two independent latches; `const`-constructible for a `static`.
pub struct PillOrigin {
    armed: AtomicBool,
    launch_done: AtomicBool,
}

impl PillOrigin {
    pub const fn new() -> Self {
        Self {
            armed: AtomicBool::new(false),
            launch_done: AtomicBool::new(false),
        }
    }

    /// The launch flight is over: from now on [`PillOrigin::arm_for_window`] may arm.
    pub fn mark_launch_done(&self) {
        self.launch_done.store(true, Ordering::SeqCst);
    }

    pub fn launch_done(&self) -> bool {
        self.launch_done.load(Ordering::SeqCst)
    }

    /// Arm when the launch flight is over and the window `label` is hidden, or absent when
    /// `absent_counts_as_hidden`.
    pub fn arm_for_window<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        label: &str,
        absent_counts_as_hidden: bool,
    ) {
        if !self.launch_done() {
            return;
        }
        let hidden_or_absent = app
            .get_webview_window(label)
            .map(|win| !win.is_visible().unwrap_or(false))
            .unwrap_or(absent_counts_as_hidden);
        if hidden_or_absent {
            self.armed.store(true, Ordering::SeqCst);
        }
    }

    /// Read and clear the flag.
    pub fn take(&self) -> bool {
        self.armed.swap(false, Ordering::SeqCst)
    }

    pub fn is_armed(&self) -> bool {
        self.armed.load(Ordering::SeqCst)
    }

    pub fn clear(&self) {
        self.armed.store(false, Ordering::SeqCst);
    }
}

impl Default for PillOrigin {
    fn default() -> Self {
        Self::new()
    }
}

/// Keep a restored window entirely inside its monitor's work area, through
/// [`crate::windows::fit_to_work_area`] with the host's `fit` (shrink an oversized window, clamp it
/// inside, re-centre one docked within `recenter_within_px` of the taskbar, leave a maximised window
/// alone when `skip_maximized`). Windows only: a no-op elsewhere. The crate has no default fit: the
/// host passes every field. `recenter_within_px` must be finite and `0..=`[`MAX_RECENTER_WITHIN_PX`];
/// otherwise nothing runs and `{prefix}_work_area_repair_error` is reported. This moves or resizes a
/// window; it never shows one. Events: `{prefix}_work_area_repair` (`from=.. to=.. work=..` logical
/// x,y,w,h) when the window moved, `{prefix}_work_area_repair_error` on failure.
pub fn fit_work_area(
    app: &AppHandle<Wry>,
    label: &str,
    prefix: &str,
    fit: crate::windows::WorkAreaFit,
    sink: EventSink,
) {
    if !(fit.recenter_within_px.is_finite()
        && (0.0..=MAX_RECENTER_WITHIN_PX).contains(&fit.recenter_within_px))
    {
        sink(
            &format!("{prefix}_work_area_repair_error"),
            "recenter_within_px out of range",
        );
        return;
    }
    #[cfg(target_os = "windows")]
    {
        match crate::windows::fit_to_work_area(app, label, fit) {
            Ok(fit) if fit.to != fit.from => sink(
                &format!("{prefix}_work_area_repair"),
                &format!(
                    "from={:?} to={:?} work={:?} (logical x,y,w,h)",
                    fit.from, fit.to, fit.work
                ),
            ),
            Ok(_) => {}
            Err(error) => sink(&format!("{prefix}_work_area_repair_error"), &error),
        }
    }
    #[cfg(not(target_os = "windows"))]
    let _ = (app, label, prefix, sink);
}

/// The shape a window takes for one surface (agreement, onboarding, app): all host values, no
/// default.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfaceSize<'a> {
    /// Name written into the log detail (`surface=<name>`); the host's own label for the surface.
    pub name: &'a str,
    /// Inner size in logical pixels; each side finite, above 0 and at most [`MAX_WINDOW_DIMENSION`].
    pub size: (f64, f64),
    /// Whether the user may resize the window on this surface.
    pub resizable: bool,
    /// Whether the window keeps the zoom/maximise affordance on this surface (the macOS green
    /// button). Re-applied every call, because enabling resizing can bring it back.
    pub maximizable: bool,
    /// Whether the window is re-centred after a resize.
    pub recenter: bool,
}

/// A window already within this many logical pixels of the size on both axes is left alone.
const SURFACE_SIZE_TOLERANCE_PX: f64 = 2.0;

/// Resize (and, when `shape.recenter`, re-centre) `win` for a surface, idempotently: a window already
/// within 2 logical pixels of the size on both axes is not resized or moved, so the steady path costs
/// nothing. The resize and maximise policy are re-applied every time (a builder sets them only at
/// creation), before that early return. The size is validated like [`build_window`]'s. This changes
/// geometry only; it never shows a window. Events, each with `surface=<name>` in the detail:
/// `{prefix}_resizable_error`, `{prefix}_maximizable_error`, `{prefix}_resize_error` (also with
/// `error=..`) and `{prefix}_resized` (`w=.. h=..`).
pub fn apply_surface_size<R: Runtime>(
    win: &WebviewWindow<R>,
    shape: SurfaceSize<'_>,
    prefix: &str,
    sink: EventSink,
) {
    let name = shape.name;
    if !size_ok(shape.size) {
        sink(
            &format!("{prefix}_resize_error"),
            &format!("surface={name} error=invalid size"),
        );
        return;
    }
    if let Err(err) = win.set_resizable(shape.resizable) {
        sink(
            &format!("{prefix}_resizable_error"),
            &format!("surface={name} resizable={} error={err}", shape.resizable),
        );
    }
    if let Err(err) = win.set_maximizable(shape.maximizable) {
        sink(
            &format!("{prefix}_maximizable_error"),
            &format!(
                "surface={name} maximizable={} error={err}",
                shape.maximizable
            ),
        );
    }
    let (w, h) = shape.size;
    let scale = win.scale_factor().unwrap_or(1.0);
    if let Ok(current) = win.inner_size() {
        let current = current.to_logical::<f64>(scale);
        if (current.width - w).abs() < SURFACE_SIZE_TOLERANCE_PX
            && (current.height - h).abs() < SURFACE_SIZE_TOLERANCE_PX
        {
            return;
        }
    }
    if let Err(err) = win.set_size(tauri::LogicalSize::new(w, h)) {
        sink(
            &format!("{prefix}_resize_error"),
            &format!("surface={name} error={err}"),
        );
        return;
    }
    if shape.recenter {
        let _ = win.center();
    }
    sink(
        &format!("{prefix}_resized"),
        &format!("surface={name} w={w} h={h}"),
    );
}

/// The Wry app handle and window behind a generic pair: the shell's reveal functions are Wry only.
fn wry_pair<'a, R: Runtime>(
    app: &'a AppHandle<R>,
    win: &'a WebviewWindow<R>,
) -> Option<(&'a AppHandle<Wry>, &'a WebviewWindow<Wry>)> {
    let app = (app as &dyn Any).downcast_ref::<AppHandle<Wry>>()?;
    let win = (win as &dyn Any).downcast_ref::<WebviewWindow<Wry>>()?;
    Some((app, win))
}

/// The one place this module brings a window up. Asks [`reveal_mode`] for `source`; `Forbidden`
/// makes no window call and reports `{prefix}_reveal_suppressed`; otherwise the shell's
/// `windows::show_window` does exactly what the mode allows (`Background`: order back without
/// activation; `Foreground`: the calls `style` names). A non-Wry runtime is refused
/// (`{prefix}_show_error`) and counts as `Forbidden`. Returns the mode applied.
fn reveal_with_style<R: Runtime>(
    app: &AppHandle<R>,
    win: &WebviewWindow<R>,
    source: RevealSource,
    style: RevealStyle,
    prefix: &str,
    sink: EventSink,
) -> RevealMode {
    let mode = reveal_mode(app, source);
    if mode == RevealMode::Forbidden {
        sink(
            &format!("{prefix}_reveal_suppressed"),
            &format!("mode=Forbidden source={source:?}"),
        );
        return mode;
    }
    let Some((wry_app, wry_win)) = wry_pair(app, win) else {
        sink(
            &format!("{prefix}_show_error"),
            "reveal requires the Wry runtime",
        );
        return RevealMode::Forbidden;
    };
    if let Err(err) = crate::windows::show_window(wry_app, wry_win, source, style) {
        sink(&format!("{prefix}_show_error"), &err);
    }
    mode
}

/// Show `win` and nothing else (no focus, no activation) as the guard allows for `source`. For a
/// window that must never take focus (a splash). Events: `{prefix}_reveal_suppressed`,
/// `{prefix}_show_error`. Returns the mode applied.
pub fn show_only<R: Runtime>(
    app: &AppHandle<R>,
    win: &WebviewWindow<R>,
    source: RevealSource,
    prefix: &str,
    sink: EventSink,
) -> RevealMode {
    reveal_with_style(app, win, source, RevealStyle::ShowOnly, prefix, sink)
}

/// Unminimise `win` when it is minimised and the guard allows a foreground reveal for `source`
/// (`Background` and `Forbidden` never unminimise). `true` when it was unminimised. Events:
/// `{prefix}_reveal_suppressed`, `{prefix}_unminimize_error`.
pub fn unminimize<R: Runtime>(
    app: &AppHandle<R>,
    win: &WebviewWindow<R>,
    source: RevealSource,
    prefix: &str,
    sink: EventSink,
) -> bool {
    if !win.is_minimized().unwrap_or(false) {
        return false;
    }
    let mode = reveal_mode(app, source);
    if mode != RevealMode::Foreground {
        sink(
            &format!("{prefix}_reveal_suppressed"),
            &format!("unminimize mode={mode:?} source={source:?}"),
        );
        return false;
    }
    match win.unminimize() {
        Ok(()) => true,
        Err(err) => {
            sink(&format!("{prefix}_unminimize_error"), &err.to_string());
            false
        }
    }
}

/// Show the window and ask for focus as the guard allows for `source`; on macOS then order it front
/// and focus it on the main thread (`Foreground` only). `Forbidden`: no call at all. `Background`:
/// the shell orders the window back without activation, and no focus, AppKit activation or order
/// front follows. Errors go to the sink as `{show_prefix}_show_error` (the shell reports a failed
/// show or focus as one error) and the macOS steps as `{native_prefix}_appkit_*` and
/// `{native_prefix}_main_thread_focus_error`; a suppressed reveal as `{show_prefix}_reveal_suppressed`.
/// Returns the mode applied, so a host can gate its own follow-up work on `Foreground`.
pub fn show_and_focus<R: Runtime>(
    app: &AppHandle<R>,
    win: &WebviewWindow<R>,
    source: RevealSource,
    show_prefix: &str,
    native_prefix: &str,
    sink: EventSink,
) -> RevealMode {
    let mode = reveal_with_style(app, win, source, RevealStyle::ShowFocus, show_prefix, sink);
    if mode == RevealMode::Foreground {
        macos::order_front(app, win, source, native_prefix, sink);
        macos::focus_on_main(app, win, source, native_prefix, sink);
    }
    mode
}

/// Everything [`build_window`] needs to make one hidden-or-visible app window. All inputs are the
/// host's; nothing here knows a product, a route, a theme or a window policy, and there is no
/// `Default`: the host states every field.
#[derive(Debug, Clone)]
pub struct WindowBuild<'a> {
    /// Passes `windows::safe_label` unchanged.
    pub label: &'a str,
    /// At most [`MAX_TITLE_LEN`] bytes, no NUL.
    pub title: &'a str,
    /// App-relative route starting with `/` (for example `/?view=splash`), at most
    /// [`MAX_ROUTE_LEN`] bytes, no scheme, no backslash, no NUL.
    pub route: &'a str,
    /// Logical pixels; each side finite, above 0 and at most [`MAX_WINDOW_DIMENSION`].
    pub inner_size: (f64, f64),
    /// Same bounds as `inner_size`.
    pub min_size: Option<(f64, f64)>,
    /// Reveal the window after it is built. The window is always built hidden; a visible one is
    /// shown through the guard ([`show_only`]) with `reveal_source`, so `never_show` and the
    /// shell's reveal mode decide whether it ever appears.
    pub visible: bool,
    /// The guard source for the reveal of a `visible` window.
    pub reveal_source: RevealSource,
    pub resizable: bool,
    pub maximizable: bool,
    pub minimizable: Option<bool>,
    pub transparent: Option<bool>,
    pub background_color: Option<[u8; 4]>,
    pub shadow: bool,
    pub center: bool,
    pub skip_taskbar: Option<bool>,
    pub always_on_top: Option<bool>,
}

fn route_ok(route: &str) -> bool {
    route.starts_with('/')
        && route.len() <= MAX_ROUTE_LEN
        && !route.contains("://")
        && !route.contains('\\')
        && !route.contains('\0')
}

fn size_ok((w, h): (f64, f64)) -> bool {
    w.is_finite()
        && h.is_finite()
        && w > 0.0
        && h > 0.0
        && w <= MAX_WINDOW_DIMENSION
        && h <= MAX_WINDOW_DIMENSION
}

/// Build a window through [`crate::windows::builder`] (frameless chrome and popup policy come from
/// the shell). Wry only: the runtime is checked and refused otherwise. Must run where Tauri allows
/// a window build (setup, or a command that is not holding the event loop). The window is built
/// hidden; when `spec.visible` it is then revealed through the guard (events `window_build_*`:
/// `window_build_reveal_suppressed`, `window_build_show_error`).
pub fn build_window<R: Runtime>(
    app: &AppHandle<R>,
    spec: &WindowBuild<'_>,
    sink: EventSink,
) -> Result<WebviewWindow<R>, String> {
    if crate::windows::safe_label(spec.label).as_deref() != Some(spec.label) {
        return Err(format!("invalid window label {:?}", spec.label));
    }
    if !route_ok(spec.route) {
        return Err(format!("invalid route for window {}", spec.label));
    }
    if spec.title.len() > MAX_TITLE_LEN || spec.title.contains('\0') {
        return Err(format!("invalid title for window {}", spec.label));
    }
    if !size_ok(spec.inner_size) || spec.min_size.is_some_and(|m| !size_ok(m)) {
        return Err(format!("invalid size for window {}", spec.label));
    }
    let wry_app = (app as &dyn Any)
        .downcast_ref::<AppHandle<Wry>>()
        .ok_or_else(|| format!("{} window requires the Wry runtime", spec.label))?;
    let mut builder = crate::windows::builder(
        wry_app,
        spec.label,
        WebviewUrl::App(spec.route.to_owned().into()),
    )
    .title(spec.title)
    .inner_size(spec.inner_size.0, spec.inner_size.1)
    .resizable(spec.resizable)
    .maximizable(spec.maximizable)
    .shadow(spec.shadow);
    if let Some((w, h)) = spec.min_size {
        builder = builder.min_inner_size(w, h);
    }
    if let Some(value) = spec.minimizable {
        builder = builder.minimizable(value);
    }
    if let Some(value) = spec.transparent {
        builder = builder.transparent(value);
    }
    if let Some([r, g, b, a]) = spec.background_color {
        builder = builder.background_color(tauri::window::Color(r, g, b, a));
    }
    if let Some(value) = spec.always_on_top {
        builder = builder.always_on_top(value);
    }
    if spec.center {
        builder = builder.center();
    }
    if let Some(value) = spec.skip_taskbar {
        builder = builder.skip_taskbar(value);
    }
    // `windows::builder` builds hidden; nothing here asks the builder to show the window.
    let win = builder.build().map_err(|err| err.to_string())?;
    if spec.visible {
        let mode = reveal_mode(wry_app, spec.reveal_source);
        if mode == RevealMode::Forbidden {
            sink(
                "window_build_reveal_suppressed",
                &format!(
                    "label={} mode=Forbidden source={:?}",
                    spec.label, spec.reveal_source
                ),
            );
        } else if let Err(err) =
            crate::windows::show_window(wry_app, &win, spec.reveal_source, RevealStyle::ShowOnly)
        {
            sink(
                "window_build_show_error",
                &format!("label={} error={err}", spec.label),
            );
        }
    }
    (Box::new(win) as Box<dyn std::any::Any>)
        .downcast::<WebviewWindow<R>>()
        .map(|win| *win)
        .map_err(|_| format!("{} window runtime mismatch", spec.label))
}

/// Host steps around [`retire_overlay`].
pub struct RetireHooks<'a, R: Runtime> {
    /// Runs after the window is made click-through and before it is hidden.
    pub before_hide: &'a dyn Fn(),
    /// Runs after the hide and before the close (make a native window non-activating, sweep).
    pub after_hide: &'a dyn Fn(&WebviewWindow<R>),
}

/// An overlay window is either visible or gone: make it click-through, hide it at once, then close
/// it, so even asynchronous webview teardown cannot leave visible or input-capturing ghost content.
/// Events: `aux_overlay_retire_cursor_error`, `aux_overlay_retire_hide_error`,
/// `aux_overlay_retire_error`, `aux_overlay_retired`, each with `label=..` (and `error=..`).
pub fn retire_overlay<R: Runtime>(
    win: &WebviewWindow<R>,
    label: &str,
    hooks: &RetireHooks<'_, R>,
    sink: EventSink,
) -> Result<(), String> {
    if let Err(err) = win.set_ignore_cursor_events(true) {
        sink(
            "aux_overlay_retire_cursor_error",
            &format!("label={label} error={err}"),
        );
    }
    (hooks.before_hide)();
    if let Err(err) = win.hide() {
        sink(
            "aux_overlay_retire_hide_error",
            &format!("label={label} error={err}"),
        );
    }
    (hooks.after_hide)(win);
    win.close().map_err(|err| {
        sink(
            "aux_overlay_retire_error",
            &format!("label={label} error={err}"),
        );
        err.to_string()
    })?;
    sink("aux_overlay_retired", &format!("label={label}"));
    Ok(())
}
