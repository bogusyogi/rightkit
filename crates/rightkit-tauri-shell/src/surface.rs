//! Window surfaces: one window, several declared shapes (PK08-3, K08 "Window surfaces").
//!
//! A first-run app keeps one hidden window and shows it in different shapes: a 960x640 legal and
//! onboarding window, then the 1200x760 app window. Each app hand-wrote the same sequence
//! (ScrapeRight `window_surface.rs`): un-maximise, resize, centre, reveal, plus a timer that shows
//! the window anyway when the frontend never asks. This module owns the sequence; the app owns the
//! numbers.
//!
//! ```ignore
//! Shell::builder("myapp")
//!     .surface(SurfaceSpec::new("setup", 960.0, 640.0).close(SurfaceClose::Quit))
//!     .surface(SurfaceSpec::new("main", 1200.0, 760.0).minimum(900.0, 600.0))
//!     .surface_fallback(Duration::from_secs(4), "setup")
//!     // renderer: invoke('rk_shell_set_surface', { surface: 'setup' })
//! ```
//!
//! # Rules
//!
//! * **The geometry is config.** A [`SurfaceSpec`] is data: id, size, minimum, placement,
//!   activation, close behaviour. Nothing here knows "legal" or "onboarding".
//! * **Serialised.** [`set_surface`] holds one lock from planning to the last step, so two
//!   callers never interleave resize, centre and reveal, and the window never visibly resizes after
//!   it appears (the reveal is the last step).
//! * **No resize when nothing changes.** A window already within [`SAME_SIZE_TOLERANCE`] of the
//!   target is not resized or re-centred under the default [`SurfacePlacement::CenterOnChange`].
//! * **Fits the screen.** The target size is held to the monitor the window is on; a window left
//!   off every monitor (a monitor was unplugged) is moved back onto the nearest one.
//! * **The visibility policy still rules.** Reveal goes through [`crate::windows::reveal_with`] /
//!   `show_window`, so `never_show`, a background launch (`--background`) and QA-hidden hosts are
//!   obeyed. The first-ready fallback is the same call: it can never override them.
//! * **The caller is the window.** `rk_shell_set_surface` shapes the window that invoked it (read
//!   from the native context, never an argument) and refuses a content webview.
//!
//! The planning is pure ([`plan`]) and the Tauri calls sit behind [`SurfaceTarget`], so every rule
//! above is tested without a window.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, WebviewWindow, Wry};

use crate::error::ShellError;
use crate::lifecycle::RevealSource;
use crate::window_state::MonitorSnapshot;
use crate::windows::{correct_offscreen, RevealStyle};
use crate::{CloseBehavior, ShellState, EVENT_WINDOW};

/// A window within this many logical pixels of the target size, on both axes, is already the
/// right shape (the OS rounds sizes).
pub const SAME_SIZE_TOLERANCE: f64 = 2.0;
/// Default wait before the first-ready fallback shows a window the frontend never shaped.
pub const DEFAULT_FALLBACK: Duration = Duration::from_secs(4);
/// Longest fallback wait a host may ask for.
pub const MAX_FALLBACK: Duration = Duration::from_secs(60);
const MAX_ID_LEN: usize = 40;
const MAX_DIMENSION: f64 = 16_384.0;

/// Where a surface puts its window.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum SurfacePlacement {
    /// Leave the window where it is (still moved back on-screen when it is off every monitor).
    Keep,
    /// Centre only when the window was resized (the first-run default: nothing moves when the
    /// shape is already right).
    #[default]
    CenterOnChange,
    /// Centre every time the surface is set.
    Center,
    /// Move to this logical position, corrected onto a visible monitor.
    At { x: f64, y: f64 },
}

/// What a surface does to the window's visibility once it is shaped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SurfaceActivation {
    /// Reveal like the tray does: show, un-minimise and focus, as the host's visibility policy
    /// allows (the default).
    #[default]
    Reveal,
    /// Show without asking for focus, as the policy allows.
    ShowOnly,
    /// Shape the window and leave it as it is (hidden stays hidden).
    Keep,
}

/// What closing the window does while this surface is current.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SurfaceClose {
    /// Whatever the shell was configured with (the default).
    #[default]
    Inherit,
    /// Closing quits, even when the app normally hides to the tray (a setup window).
    Quit,
    /// Closing hides the window and the app keeps running.
    HideToTray,
}

impl SurfaceClose {
    /// The shell behaviour this overrides, or `None` for [`SurfaceClose::Inherit`].
    pub fn behavior(self) -> Option<CloseBehavior> {
        match self {
            Self::Inherit => None,
            Self::Quit => Some(CloseBehavior::Quit),
            Self::HideToTray => Some(CloseBehavior::HideToTray),
        }
    }
}

/// One declared shape of a window.
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceSpec {
    /// Name the renderer sets: `a-z`, `0-9`, `-`, `_`, at most 40 characters.
    pub id: String,
    /// Inner size in logical pixels.
    pub size: (f64, f64),
    /// Minimum inner size, applied before the resize.
    pub minimum: Option<(f64, f64)>,
    pub placement: SurfacePlacement,
    pub activation: SurfaceActivation,
    pub close_behavior: SurfaceClose,
}

impl SurfaceSpec {
    /// A surface of `width` x `height` with the defaults: centre on change, reveal, inherit close.
    pub fn new(id: &str, width: f64, height: f64) -> Self {
        Self {
            id: id.to_owned(),
            size: (width, height),
            minimum: None,
            placement: SurfacePlacement::default(),
            activation: SurfaceActivation::default(),
            close_behavior: SurfaceClose::default(),
        }
    }

    pub fn minimum(mut self, width: f64, height: f64) -> Self {
        self.minimum = Some((width, height));
        self
    }

    pub fn placement(mut self, placement: SurfacePlacement) -> Self {
        self.placement = placement;
        self
    }

    pub fn activation(mut self, activation: SurfaceActivation) -> Self {
        self.activation = activation;
        self
    }

    pub fn close(mut self, close: SurfaceClose) -> Self {
        self.close_behavior = close;
        self
    }

    /// Check the id and the numbers. The registry runs it; a bad spec is a startup error.
    pub fn validate(&self) -> Result<(), ShellError> {
        let id_ok = !self.id.is_empty()
            && self.id.len() <= MAX_ID_LEN
            && self
                .id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'));
        if !id_ok {
            return Err(ShellError::Invalid(format!(
                "{:?} is not a surface id (a-z, 0-9, '-', '_'; at most {MAX_ID_LEN})",
                self.id
            )));
        }
        let dim_ok = |(w, h): (f64, f64)| {
            w.is_finite()
                && h.is_finite()
                && w > 0.0
                && h > 0.0
                && w <= MAX_DIMENSION
                && h <= MAX_DIMENSION
        };
        if !dim_ok(self.size) {
            return Err(ShellError::Invalid(format!(
                "surface {:?} needs a finite size above zero and at most {MAX_DIMENSION}",
                self.id
            )));
        }
        if let Some(min) = self.minimum {
            if !dim_ok(min) || min.0 > self.size.0 || min.1 > self.size.1 {
                return Err(ShellError::Invalid(format!(
                    "surface {:?} has a minimum that is not a finite size within its own size",
                    self.id
                )));
            }
        }
        if let SurfacePlacement::At { x, y } = self.placement {
            if !x.is_finite() || !y.is_finite() {
                return Err(ShellError::Invalid(format!(
                    "surface {:?} has a non-finite position",
                    self.id
                )));
            }
        }
        Ok(())
    }
}

/// What the plan needs to know about the window, all in logical pixels.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WindowFacts {
    /// Current inner size; `None` when the window cannot say.
    pub inner: Option<(f64, f64)>,
    /// Current outer position; `None` when unknown (a minimised window).
    pub position: Option<(f64, f64)>,
    pub maximized: bool,
    pub fullscreen: bool,
    pub minimized: bool,
    /// The monitor the window is on, when known.
    pub current_monitor: Option<MonitorSnapshot>,
    /// Every connected monitor.
    pub monitors: Vec<MonitorSnapshot>,
}

/// One call the plan makes on the window.
#[derive(Debug, Clone, PartialEq)]
pub enum SurfaceStep {
    LeaveFullscreen,
    Unmaximize,
    Unminimize,
    SetMinimum { width: f64, height: f64 },
    Resize { width: f64, height: f64 },
    Center,
    MoveTo { x: f64, y: f64 },
    Reveal(SurfaceActivation),
}

/// The steps for one [`set_surface`], in order.
#[derive(Debug, Clone, PartialEq)]
pub struct SurfacePlan {
    pub steps: Vec<SurfaceStep>,
    /// The size the window ends at (after fitting it to the monitor).
    pub target: (f64, f64),
    /// The window was not already the right shape.
    pub resized: bool,
}

fn within(a: (f64, f64), b: (f64, f64)) -> bool {
    (a.0 - b.0).abs() < SAME_SIZE_TOLERANCE && (a.1 - b.1).abs() < SAME_SIZE_TOLERANCE
}

/// The size `spec` asks for, held to `monitor` (when known) and never below the minimum.
pub fn fitted_size(spec: &SurfaceSpec, monitor: Option<&MonitorSnapshot>) -> (f64, f64) {
    let (mut w, mut h) = spec.size;
    if let Some(m) = monitor {
        if m.width.is_finite() && m.width > 0.0 {
            w = w.min(m.width);
        }
        if m.height.is_finite() && m.height > 0.0 {
            h = h.min(m.height);
        }
    }
    if let Some((min_w, min_h)) = spec.minimum {
        w = w.max(min_w.min(spec.size.0));
        h = h.max(min_h.min(spec.size.1));
    }
    (w, h)
}

/// Plan the steps that give `facts` the shape of `spec`. Pure.
pub fn plan(spec: &SurfaceSpec, facts: &WindowFacts) -> SurfacePlan {
    let monitor = facts
        .current_monitor
        .or_else(|| facts.monitors.first().copied());
    let target = fitted_size(spec, monitor.as_ref());
    let same = facts.inner.is_some_and(|c| within(c, target));
    // A maximised or full-screen window ignores `set_size`: it is never "already the right shape".
    let resized = !same || facts.maximized || facts.fullscreen;
    let mut steps = Vec::new();
    if resized {
        if facts.fullscreen {
            steps.push(SurfaceStep::LeaveFullscreen);
        }
        if facts.maximized {
            steps.push(SurfaceStep::Unmaximize);
        }
    }
    if facts.minimized && spec.activation != SurfaceActivation::Keep {
        steps.push(SurfaceStep::Unminimize);
    }
    if let Some((width, height)) = spec.minimum {
        steps.push(SurfaceStep::SetMinimum { width, height });
    }
    if resized {
        steps.push(SurfaceStep::Resize {
            width: target.0,
            height: target.1,
        });
    }
    let end_size = if resized {
        target
    } else {
        facts.inner.unwrap_or(target)
    };
    match spec.placement {
        SurfacePlacement::Center => steps.push(SurfaceStep::Center),
        SurfacePlacement::CenterOnChange if resized => steps.push(SurfaceStep::Center),
        SurfacePlacement::At { x, y } => {
            let (x, y) = correct_offscreen(x, y, end_size.0, end_size.1, &facts.monitors);
            steps.push(SurfaceStep::MoveTo { x, y });
        }
        // Keep, and CenterOnChange with nothing changed: only recover a window that is off every
        // monitor, which has no meaning for a minimised window (its position is unknown).
        _ => {
            if let (Some((x, y)), false) = (facts.position, facts.minimized) {
                let (nx, ny) = correct_offscreen(x, y, end_size.0, end_size.1, &facts.monitors);
                if (nx - x).abs() > 0.5 || (ny - y).abs() > 0.5 {
                    steps.push(SurfaceStep::MoveTo { x: nx, y: ny });
                }
            }
        }
    }
    if spec.activation != SurfaceActivation::Keep {
        steps.push(SurfaceStep::Reveal(spec.activation));
    }
    SurfacePlan {
        steps,
        target,
        resized,
    }
}

/// The calls a plan makes, so the order and the error rules are testable without a window.
pub trait SurfaceTarget {
    fn facts(&self) -> WindowFacts;
    /// Run one step. `Reveal` obeys the host's visibility policy and answers whether the window
    /// was brought up.
    fn apply(&mut self, step: &SurfaceStep) -> Result<bool, String>;
}

/// What [`apply_plan`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SurfaceReport {
    pub surface: String,
    pub resized: bool,
    pub revealed: bool,
    /// Steps that failed without stopping the sequence (a window that would not un-maximise).
    pub skipped: usize,
}

/// Run `plan` on `target`. A failed resize or reveal is an error; a failed un-maximise, un-minimise,
/// minimum, centre or move is counted in the report and the sequence continues, because the window
/// is still usable and the reveal must not be lost.
pub fn apply_plan(
    surface: &str,
    plan: &SurfacePlan,
    target: &mut impl SurfaceTarget,
) -> Result<SurfaceReport, String> {
    let mut report = SurfaceReport {
        surface: surface.to_owned(),
        resized: plan.resized,
        revealed: false,
        skipped: 0,
    };
    for step in &plan.steps {
        match (target.apply(step), step) {
            (Ok(shown), SurfaceStep::Reveal(_)) => report.revealed = shown,
            (Ok(_), _) => {}
            (Err(e), SurfaceStep::Resize { .. } | SurfaceStep::Reveal(_)) => return Err(e),
            (Err(_), _) => report.skipped += 1,
        }
    }
    Ok(report)
}

/// Cancels a first-ready fallback. One latch per armed fallback.
#[derive(Default)]
struct Latch {
    cancelled: Mutex<bool>,
    cv: Condvar,
}

impl Latch {
    fn cancel(&self) {
        *self.cancelled.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.cv.notify_all();
    }

    /// Wait up to `timeout`; true when it was cancelled first.
    fn wait_cancelled(&self, timeout: Duration) -> bool {
        let guard = self.cancelled.lock().unwrap_or_else(|e| e.into_inner());
        let (guard, _) = self
            .cv
            .wait_timeout_while(guard, timeout, |cancelled| !*cancelled)
            .unwrap_or_else(|e| e.into_inner());
        *guard
    }
}

/// What the first-ready fallback does when its wait ends with the window never shaped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FallbackAction {
    /// The frontend shaped the window in time.
    Nothing,
    /// Set this surface (its own activation decides visibility).
    SetSurface(String),
    /// No default surface: reveal the window through the visibility guard.
    Reveal,
}

/// The fallback decision. Pure.
pub fn fallback_action(current: Option<&str>, default_surface: Option<&str>) -> FallbackAction {
    match (current, default_surface) {
        (Some(_), _) => FallbackAction::Nothing,
        (None, Some(id)) => FallbackAction::SetSurface(id.to_owned()),
        (None, None) => FallbackAction::Reveal,
    }
}

/// The declared surfaces and what each window shows now. Managed Tauri state, installed by the
/// shell plugin.
#[derive(Default)]
pub struct SurfaceRegistry {
    specs: Mutex<BTreeMap<String, SurfaceSpec>>,
    current: Mutex<BTreeMap<String, String>>,
    /// Held from planning to the last step of every `set_surface`.
    serial: Mutex<()>,
    latches: Mutex<Vec<Arc<Latch>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl SurfaceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare `spec`. Fails on an invalid spec or an id declared before.
    pub fn register(&self, spec: SurfaceSpec) -> Result<(), ShellError> {
        spec.validate()?;
        let mut specs = lock(&self.specs);
        if specs.contains_key(&spec.id) {
            return Err(ShellError::Invalid(format!(
                "surface {:?} is declared more than once",
                spec.id
            )));
        }
        specs.insert(spec.id.clone(), spec);
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<SurfaceSpec> {
        lock(&self.specs).get(id).cloned()
    }

    /// Declared ids, sorted.
    pub fn ids(&self) -> Vec<String> {
        lock(&self.specs).keys().cloned().collect()
    }

    /// The surface the window `label` shows now, or `None` before the first `set_surface`.
    pub fn current(&self, label: &str) -> Option<String> {
        lock(&self.current).get(label).cloned()
    }

    /// The close behaviour the current surface of `label` imposes, or `None` to follow the shell.
    pub fn close_override(&self, label: &str) -> Option<CloseBehavior> {
        let id = self.current(label)?;
        self.get(&id)?.close_behavior.behavior()
    }

    fn set_current(&self, label: &str, id: &str) {
        lock(&self.current).insert(label.to_owned(), id.to_owned());
    }

    /// Forget a closed window.
    pub fn forget(&self, label: &str) {
        lock(&self.current).remove(label);
    }

    fn arm(&self) -> Arc<Latch> {
        let latch = Arc::new(Latch::default());
        lock(&self.latches).push(Arc::clone(&latch));
        latch
    }

    /// Cancel every armed first-ready fallback (a surface was set, or the app is exiting).
    pub fn cancel_fallbacks(&self) {
        for latch in lock(&self.latches).drain(..) {
            latch.cancel();
        }
    }
}

/// The effective "closing hides the window" for `label`: the current surface's override when it has
/// one, else the shell's setting.
pub(crate) fn close_hides(app: &AppHandle<Wry>, shell_close_to_tray: bool, label: &str) -> bool {
    match app
        .try_state::<SurfaceRegistry>()
        .and_then(|r| r.close_override(label))
    {
        Some(CloseBehavior::Quit) => false,
        Some(_) => true,
        None => shell_close_to_tray,
    }
}

struct WindowTarget<'a> {
    app: &'a AppHandle<Wry>,
    window: &'a WebviewWindow<Wry>,
}

fn logical_monitors(app: &AppHandle<Wry>) -> Vec<MonitorSnapshot> {
    app.available_monitors()
        .unwrap_or_default()
        .iter()
        .map(MonitorSnapshot::from_monitor)
        .collect()
}

impl SurfaceTarget for WindowTarget<'_> {
    fn facts(&self) -> WindowFacts {
        let w = self.window;
        let scale = w
            .scale_factor()
            .ok()
            .filter(|s| s.is_finite() && *s > 0.0)
            .unwrap_or(1.0);
        let minimized = w.is_minimized().unwrap_or(false);
        WindowFacts {
            inner: w.inner_size().ok().map(|s| {
                let l = s.to_logical::<f64>(scale);
                (l.width, l.height)
            }),
            position: if minimized {
                None
            } else {
                w.outer_position().ok().map(|p| {
                    let l = p.to_logical::<f64>(scale);
                    (l.x, l.y)
                })
            },
            maximized: w.is_maximized().unwrap_or(false),
            fullscreen: w.is_fullscreen().unwrap_or(false),
            minimized,
            current_monitor: w
                .current_monitor()
                .ok()
                .flatten()
                .map(|m| MonitorSnapshot::from_monitor(&m)),
            monitors: logical_monitors(self.app),
        }
    }

    fn apply(&mut self, step: &SurfaceStep) -> Result<bool, String> {
        let w = self.window;
        let quiet = |r: tauri::Result<()>| r.map(|_| false).map_err(|e| e.to_string());
        match step {
            SurfaceStep::LeaveFullscreen => quiet(w.set_fullscreen(false)),
            SurfaceStep::Unmaximize => quiet(w.unmaximize()),
            SurfaceStep::Unminimize => quiet(w.unminimize()),
            SurfaceStep::SetMinimum { width, height } => {
                quiet(w.set_min_size(Some(LogicalSize::new(*width, *height))))
            }
            SurfaceStep::Resize { width, height } => {
                quiet(w.set_size(LogicalSize::new(*width, *height)))
            }
            SurfaceStep::Center => quiet(w.center()),
            SurfaceStep::MoveTo { x, y } => quiet(w.set_position(LogicalPosition::new(*x, *y))),
            SurfaceStep::Reveal(SurfaceActivation::ShowOnly) => crate::windows::show_window(
                self.app,
                w,
                RevealSource::WindowCommand,
                RevealStyle::ShowOnly,
            ),
            SurfaceStep::Reveal(_) => crate::windows::show_window(
                self.app,
                w,
                RevealSource::WindowCommand,
                RevealStyle::Full,
            ),
        }
    }
}

/// Give the window `label` the shape of surface `id`. Serialised with every other `set_surface`.
pub fn set_surface(
    app: &AppHandle<Wry>,
    label: &str,
    id: &str,
) -> Result<SurfaceReport, ShellError> {
    let registry = app
        .try_state::<SurfaceRegistry>()
        .ok_or_else(|| ShellError::Unsupported("the shell is not installed".into()))?;
    let spec = registry
        .get(id)
        .ok_or_else(|| ShellError::Invalid(format!("unknown surface: {id}")))?;
    let window = app
        .get_webview_window(label)
        .ok_or_else(|| ShellError::Invalid(format!("unknown window {label}")))?;
    let _serial = lock(&registry.serial);
    let mut target = WindowTarget {
        app,
        window: &window,
    };
    let plan = plan(&spec, &target.facts());
    let report = apply_plan(&spec.id, &plan, &mut target).map_err(ShellError::Other)?;
    registry.set_current(label, &spec.id);
    // The window now has a shape: a pending first-ready fallback has nothing left to do.
    registry.cancel_fallbacks();
    let _ = app.emit(
        EVENT_WINDOW,
        serde_json::json!({"label": label, "kind": "surface", "surface": spec.id}),
    );
    if report.revealed && label == app.state::<ShellState>().main_label {
        crate::idle::note_visible(app);
    }
    Ok(report)
}

/// Arm the first-ready fallback for `label`: after `wait` (bounded by [`MAX_FALLBACK`]) a window
/// the frontend never shaped is shaped with `default_surface`, or revealed through the visibility
/// guard when there is none. Cancelled by any `set_surface` and by app exit; it never forces a
/// window up against `never_show` or a background launch, because it makes the same guarded calls.
pub(crate) fn arm_fallback(
    app: &AppHandle<Wry>,
    label: String,
    wait: Duration,
    default_surface: Option<String>,
) {
    let Some(registry) = app.try_state::<SurfaceRegistry>() else {
        return;
    };
    let latch = registry.arm();
    let wait = wait.min(MAX_FALLBACK);
    let app = app.clone();
    let _ = std::thread::Builder::new()
        .name("rk-shell-surface-fallback".into())
        .spawn(move || {
            if latch.wait_cancelled(wait) {
                return;
            }
            let Some(registry) = app.try_state::<SurfaceRegistry>() else {
                return;
            };
            match fallback_action(
                registry.current(&label).as_deref(),
                default_surface.as_deref(),
            ) {
                FallbackAction::Nothing => {}
                FallbackAction::SetSurface(id) => {
                    let _ = set_surface(&app, &label, &id);
                }
                FallbackAction::Reveal => {
                    crate::windows::reveal_with(&app, Some(&label), RevealSource::WindowCommand);
                }
            }
        });
}

/// Shape the invoking window as surface `surface` (declared with `ShellBuilder::surface`). The
/// window is the caller's own, read from the native context; a content webview is refused.
#[tauri::command]
pub async fn rk_shell_set_surface(
    webview: tauri::Webview<Wry>,
    surface: String,
) -> Result<SurfaceReport, ShellError> {
    crate::commands::guard_command(&webview, "rk_shell_set_surface")?;
    crate::caller_gate::require_app_caller(&webview)?;
    let app = webview.app_handle().clone();
    let label = webview.label().to_owned();
    tauri::async_runtime::spawn_blocking(move || set_surface(&app, &label, &surface))
        .await
        .map_err(|e| ShellError::Other(e.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::RevealMode;
    use crate::windows::{apply_reveal, RevealTarget};

    fn monitor(x: f64, y: f64, w: f64, h: f64) -> MonitorSnapshot {
        MonitorSnapshot {
            x,
            y,
            width: w,
            height: h,
            scale: 1.0,
        }
    }

    fn facts(inner: (f64, f64), pos: (f64, f64)) -> WindowFacts {
        let m = monitor(0.0, 0.0, 1920.0, 1080.0);
        WindowFacts {
            inner: Some(inner),
            position: Some(pos),
            current_monitor: Some(m),
            monitors: vec![m],
            ..Default::default()
        }
    }

    fn setup() -> SurfaceSpec {
        SurfaceSpec::new("setup", 960.0, 640.0).close(SurfaceClose::Quit)
    }

    #[test]
    fn specs_are_validated() {
        assert!(setup().validate().is_ok());
        for bad in [
            SurfaceSpec::new("", 1.0, 1.0),
            SurfaceSpec::new("Upper", 100.0, 100.0),
            SurfaceSpec::new(&"x".repeat(41), 100.0, 100.0),
            SurfaceSpec::new("a", 0.0, 100.0),
            SurfaceSpec::new("a", f64::NAN, 100.0),
            SurfaceSpec::new("a", 100.0, 99_999.0),
            SurfaceSpec::new("a", 100.0, 100.0).minimum(200.0, 50.0),
            SurfaceSpec::new("a", 100.0, 100.0).placement(SurfacePlacement::At {
                x: f64::INFINITY,
                y: 0.0,
            }),
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        let r = SurfaceRegistry::new();
        r.register(setup()).unwrap();
        assert!(r.register(setup()).is_err(), "an id is declared once");
        assert_eq!(r.ids(), ["setup"]);
    }

    #[test]
    fn a_first_run_window_is_resized_centred_and_revealed_last() {
        let p = plan(&setup(), &facts((1200.0, 760.0), (100.0, 100.0)));
        assert!(p.resized);
        assert_eq!(
            p.steps,
            vec![
                SurfaceStep::Resize {
                    width: 960.0,
                    height: 640.0
                },
                SurfaceStep::Center,
                SurfaceStep::Reveal(SurfaceActivation::Reveal),
            ]
        );
    }

    #[test]
    fn setup_to_main_geometry_transition_never_reveals_before_the_resize() {
        let main = SurfaceSpec::new("main", 1200.0, 760.0).minimum(900.0, 600.0);
        let p = plan(&main, &facts((960.0, 640.0), (480.0, 220.0)));
        let order: Vec<_> = p.steps.iter().map(|s| std::mem::discriminant(s)).collect();
        let pos = |want: SurfaceStep| {
            p.steps
                .iter()
                .position(|s| std::mem::discriminant(s) == std::mem::discriminant(&want))
        };
        let min = pos(SurfaceStep::SetMinimum {
            width: 0.0,
            height: 0.0,
        })
        .unwrap();
        let resize = pos(SurfaceStep::Resize {
            width: 0.0,
            height: 0.0,
        })
        .unwrap();
        let reveal = pos(SurfaceStep::Reveal(SurfaceActivation::Reveal)).unwrap();
        assert!(min < resize && resize < reveal, "{p:?}");
        assert_eq!(reveal, order.len() - 1, "the reveal is the last step");
    }

    #[test]
    fn a_window_already_the_right_shape_is_not_resized_or_recentred() {
        let p = plan(&setup(), &facts((960.5, 639.0), (480.0, 220.0)));
        assert!(!p.resized);
        assert_eq!(
            p.steps,
            vec![SurfaceStep::Reveal(SurfaceActivation::Reveal)]
        );
        // `Center` re-centres anyway; `Keep` never does.
        let always = setup().placement(SurfacePlacement::Center);
        assert!(plan(&always, &facts((960.0, 640.0), (480.0, 220.0)))
            .steps
            .contains(&SurfaceStep::Center));
        let keep = setup().placement(SurfacePlacement::Keep);
        assert!(!plan(&keep, &facts((1200.0, 760.0), (10.0, 10.0)))
            .steps
            .contains(&SurfaceStep::Center));
    }

    #[test]
    fn a_maximised_or_fullscreen_window_is_restored_before_the_resize() {
        let mut f = facts((960.0, 640.0), (0.0, 0.0));
        f.maximized = true;
        let p = plan(&setup(), &f);
        assert!(p.resized, "maximised is never the right shape");
        assert_eq!(p.steps[0], SurfaceStep::Unmaximize);
        f.maximized = false;
        f.fullscreen = true;
        assert_eq!(plan(&setup(), &f).steps[0], SurfaceStep::LeaveFullscreen);
    }

    #[test]
    fn the_size_is_held_to_the_monitor_but_not_below_the_minimum() {
        let small = monitor(0.0, 0.0, 800.0, 600.0);
        let spec = SurfaceSpec::new("main", 1200.0, 760.0).minimum(700.0, 500.0);
        assert_eq!(fitted_size(&spec, Some(&small)), (800.0, 600.0));
        let tiny = monitor(0.0, 0.0, 640.0, 480.0);
        assert_eq!(fitted_size(&spec, Some(&tiny)), (700.0, 500.0));
        assert_eq!(fitted_size(&spec, None), (1200.0, 760.0));
    }

    #[test]
    fn a_window_left_off_every_monitor_is_moved_back_without_a_resize() {
        // The second monitor was unplugged: the window sits at x = 2500 on a 1920-wide screen.
        let p = plan(&setup(), &facts((960.0, 640.0), (2500.0, 200.0)));
        assert!(!p.resized);
        let moved = p.steps.iter().find_map(|s| match s {
            SurfaceStep::MoveTo { x, y } => Some((*x, *y)),
            _ => None,
        });
        let (x, _) = moved.expect("an off-screen window is moved back");
        assert!(x < 1920.0, "{x}");
        // A window on screen is left alone.
        let ok = plan(&setup(), &facts((960.0, 640.0), (100.0, 100.0)));
        assert!(!ok
            .steps
            .iter()
            .any(|s| matches!(s, SurfaceStep::MoveTo { .. })));
    }

    #[test]
    fn an_absolute_position_is_corrected_onto_a_monitor() {
        let spec = setup().placement(SurfacePlacement::At {
            x: 5000.0,
            y: 5000.0,
        });
        let p = plan(&spec, &facts((960.0, 640.0), (0.0, 0.0)));
        let (x, y) = p
            .steps
            .iter()
            .find_map(|s| match s {
                SurfaceStep::MoveTo { x, y } => Some((*x, *y)),
                _ => None,
            })
            .unwrap();
        assert!(x < 1920.0 && y < 1080.0, "({x},{y})");
    }

    #[test]
    fn a_keep_activation_shapes_without_showing() {
        let spec = setup().activation(SurfaceActivation::Keep);
        let mut f = facts((1200.0, 760.0), (0.0, 0.0));
        f.minimized = true;
        let p = plan(&spec, &f);
        assert!(!p
            .steps
            .iter()
            .any(|s| matches!(s, SurfaceStep::Reveal(_) | SurfaceStep::Unminimize)));
        // Revealing a minimised window un-minimises it first.
        f.minimized = true;
        assert!(plan(&setup(), &f).steps.contains(&SurfaceStep::Unminimize));
    }

    /// A recording stand-in for the window. Its reveal obeys a `RevealMode` through the shell's own
    /// `apply_reveal`, the function every real reveal goes through.
    struct Fake {
        mode: RevealMode,
        log: Vec<String>,
        fail_step: Option<&'static str>,
    }

    struct Rev<'a>(&'a mut Vec<String>);
    impl RevealTarget for Rev<'_> {
        fn prepare(&mut self) {
            self.0.push("prepare".into());
        }
        fn app_show(&mut self) {
            self.0.push("app_show".into());
        }
        fn unminimize(&mut self) -> Result<(), String> {
            self.0.push("unminimize".into());
            Ok(())
        }
        fn show(&mut self) -> Result<(), String> {
            self.0.push("show".into());
            Ok(())
        }
        fn focus(&mut self) -> Result<(), String> {
            self.0.push("focus".into());
            Ok(())
        }
        fn order_back(&mut self) -> Result<(), String> {
            self.0.push("order_back".into());
            Ok(())
        }
    }

    impl SurfaceTarget for Fake {
        fn facts(&self) -> WindowFacts {
            WindowFacts::default()
        }
        fn apply(&mut self, step: &SurfaceStep) -> Result<bool, String> {
            let name = match step {
                SurfaceStep::Resize { .. } => "resize",
                SurfaceStep::Center => "center",
                SurfaceStep::Unmaximize => "unmaximize",
                SurfaceStep::Reveal(_) => "reveal",
                _ => "other",
            };
            if self.fail_step == Some(name) {
                return Err(format!("{name} failed"));
            }
            if let SurfaceStep::Reveal(a) = step {
                let style = if *a == SurfaceActivation::ShowOnly {
                    RevealStyle::ShowOnly
                } else {
                    RevealStyle::Full
                };
                let mode = self.mode;
                return apply_reveal(mode, style, &mut Rev(&mut self.log));
            }
            self.log.push(name.into());
            Ok(false)
        }
    }

    fn run(
        mode: RevealMode,
        fail: Option<&'static str>,
    ) -> (Result<SurfaceReport, String>, Vec<String>) {
        let p = plan(&setup(), &facts((1200.0, 760.0), (100.0, 100.0)));
        let mut fake = Fake {
            mode,
            log: Vec::new(),
            fail_step: fail,
        };
        let r = apply_plan("setup", &p, &mut fake);
        (r, fake.log)
    }

    #[test]
    fn a_background_or_never_show_host_is_never_foregrounded_by_a_surface() {
        let (r, log) = run(RevealMode::Forbidden, None);
        let r = r.unwrap();
        assert!(!r.revealed);
        assert_eq!(log, ["resize", "center"], "no show, no focus");
        let (r, log) = run(RevealMode::Background, None);
        assert!(r.unwrap().revealed);
        assert!(log.contains(&"order_back".to_string()));
        assert!(!log.iter().any(|c| c == "focus" || c == "show"), "{log:?}");
        let (r, log) = run(RevealMode::Foreground, None);
        assert!(r.unwrap().revealed);
        assert!(log.contains(&"focus".to_string()));
    }

    #[test]
    fn a_failed_resize_or_reveal_is_an_error_but_a_failed_centre_is_not() {
        assert!(run(RevealMode::Foreground, Some("resize")).0.is_err());
        let (r, log) = run(RevealMode::Foreground, Some("center"));
        let r = r.unwrap();
        assert_eq!(r.skipped, 1);
        assert!(r.revealed, "the reveal is not lost to a centre failure");
        assert!(log.contains(&"focus".to_string()));
    }

    #[test]
    fn the_fallback_never_acts_on_a_window_the_frontend_shaped() {
        assert_eq!(
            fallback_action(Some("setup"), Some("main")),
            FallbackAction::Nothing
        );
        assert_eq!(
            fallback_action(None, Some("setup")),
            FallbackAction::SetSurface("setup".into())
        );
        assert_eq!(fallback_action(None, None), FallbackAction::Reveal);
    }

    #[test]
    fn a_fallback_wait_is_cancellable_and_bounded() {
        let r = SurfaceRegistry::new();
        let latch = r.arm();
        let t = std::thread::spawn({
            let latch = Arc::clone(&latch);
            move || latch.wait_cancelled(Duration::from_secs(30))
        });
        std::thread::sleep(Duration::from_millis(30));
        r.cancel_fallbacks();
        assert!(t.join().unwrap(), "cancel ends the wait at once");
        // An uncancelled wait times out and reports it was not cancelled.
        assert!(!Latch::default().wait_cancelled(Duration::from_millis(10)));
    }

    #[test]
    fn the_current_surface_names_what_closing_does() {
        let r = SurfaceRegistry::new();
        r.register(setup()).unwrap();
        r.register(SurfaceSpec::new("main", 1200.0, 760.0)).unwrap();
        assert_eq!(
            r.close_override("main"),
            None,
            "no surface yet: follow the shell"
        );
        r.set_current("main", "setup");
        assert_eq!(r.close_override("main"), Some(CloseBehavior::Quit));
        r.set_current("main", "main");
        assert_eq!(r.close_override("main"), None);
        r.set_current("main", "setup");
        r.forget("main");
        assert_eq!(r.current("main"), None);
    }
}
