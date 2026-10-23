//! `OverlayHost` (PK08-4, feature `overlay`): the native side of a small always-on-top panel (a
//! recording pill, a notch, a hover card) that must never take the user's focus.
//!
//! The pure policy lives in `rightkit-overlay` (work-area clamping, the topmost and no-activate
//! style contract, the covering-window verdict, the full-screen policy). This module is the host
//! around it, in three layers:
//!
//! 1. [`OverlayCore`]: pure. The show / update / hide / dispose state machine, placement within the
//!    work area of the right monitor (mixed DPI), off-screen recovery when a monitor goes away,
//!    full-screen suppression and the auto-hide timer **as data** ([`AutoHide`]). It returns
//!    [`OverlayOp`]s and never touches a window.
//! 2. [`Overlay`] / [`OverlayHost`]: run the ops against an [`OverlaySurface`] and own the
//!    registry. Both are generic over the surface, so the whole behaviour is tested with a fake.
//! 3. [`TauriSurface`] / [`OverlayService`]: the real surface over a Tauri window (Windows:
//!    `show_noactivate` plus the `WNDPROC` no-activate guard; macOS: `orderFrontRegardless` at a
//!    window level; elsewhere a plain show) and one timer thread for auto-hide.
//!
//! The app names its overlays: [`OverlaySpec::kind`] is an app-supplied label (`"pill"`, `"notch"`),
//! never a product name, and the window itself is declared by the app (hidden, undecorated,
//! transparent, no focus) under the label [`OverlaySpec::id`]. There is no IPC command: a renderer
//! cannot show, move or hide an overlay; the app's Rust code decides.
//!
//! # Focus
//! An overlay whose style does not take focus is shown with [`OverlayOp::Show`]` { activate: false }`
//! and never otherwise: the user's app stays in front and keeps the keyboard. A style that
//! takes focus (a launcher) is shown with `activate: true`.
//!
//! # Covering
//! [`Overlay::covering`] answers "is something above the overlay?" from the z-order: the windows
//! stacked above it, nearest first, reduced to [`CoveringCandidate`]s and judged by
//! `rightkit_overlay::covering_window`. The walk itself is native (Windows); elsewhere the probe
//! reports nothing above.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use rightkit_overlay::{
    covering_window, CoveringCandidate, FullScreenPolicy, MonitorPlacement, OverlayRect,
    OverlayStyle, Rect, WorkArea,
};

/// The longest an overlay may be asked to stay up on its own.
pub const MAX_AUTO_HIDE: Duration = Duration::from_secs(600);
/// Largest logical overlay edge.
pub const MAX_OVERLAY_EDGE: f64 = 4096.0;
/// Largest logical margin.
pub const MAX_OVERLAY_MARGIN: f64 = 512.0;

/// Why an overlay call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayError {
    /// The spec or an argument is not acceptable.
    Invalid(&'static str),
    /// No monitor is available to place the overlay on.
    NoMonitor,
    /// The overlay was disposed.
    Disposed,
    /// No overlay with this id.
    Unknown(String),
    /// An overlay with this id is already registered.
    Duplicate(String),
    /// The native surface refused an operation.
    Surface(String),
}

impl std::fmt::Display for OverlayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(why) => write!(f, "invalid overlay request: {why}"),
            Self::NoMonitor => f.write_str("no monitor to place the overlay on"),
            Self::Disposed => f.write_str("the overlay was disposed"),
            Self::Unknown(id) => write!(f, "no overlay {id:?}"),
            Self::Duplicate(id) => write!(f, "overlay {id:?} is already registered"),
            Self::Surface(why) => write!(f, "overlay surface: {why}"),
        }
    }
}

impl std::error::Error for OverlayError {}

/// Where on the work area an overlay sits. Offsets are logical (100% scaling) and scale with the
/// monitor the overlay lands on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OverlayAnchor {
    TopCenter,
    BottomCenter,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Center,
    /// Logical offset from the work area's top-left corner.
    At {
        x: f64,
        y: f64,
    },
}

/// What an overlay is. Built by the app.
#[derive(Debug, Clone, PartialEq)]
pub struct OverlaySpec {
    /// The label of the Tauri window the app declared for this overlay.
    pub id: String,
    /// An app-supplied label for what this overlay is (`"pill"`, `"notch"`). Never a product name;
    /// `[a-z][a-z0-9_-]{0,31}`.
    pub kind: String,
    /// Logical size (width, height) at 100% scaling.
    pub size: (f64, f64),
    pub anchor: OverlayAnchor,
    /// Logical gap kept from the anchored edge.
    pub margin: f64,
    /// Topmost, takes focus or not, click-through. Must be topmost.
    pub style: OverlayStyle,
    /// What to do while another program fills the monitor.
    pub full_screen: FullScreenPolicy,
    /// macOS window level (NSStatusWindowLevel is 25, the default).
    pub macos_level: i32,
    /// Destroy the window on dispose (default). Set false when the app owns the window's life.
    pub destroy_on_dispose: bool,
}

impl OverlaySpec {
    /// A passive (never-focused, topmost) overlay anchored at the top centre of the work area.
    pub fn new(id: &str, kind: &str, size: (f64, f64)) -> Self {
        Self {
            id: id.to_owned(),
            kind: kind.to_owned(),
            size,
            anchor: OverlayAnchor::TopCenter,
            margin: 12.0,
            style: OverlayStyle::PASSIVE,
            full_screen: FullScreenPolicy::StayAbove,
            macos_level: 25,
            destroy_on_dispose: true,
        }
    }

    pub fn anchor(mut self, anchor: OverlayAnchor) -> Self {
        self.anchor = anchor;
        self
    }

    pub fn margin(mut self, margin: f64) -> Self {
        self.margin = margin;
        self
    }

    pub fn style(mut self, style: OverlayStyle) -> Self {
        self.style = style;
        self
    }

    pub fn full_screen(mut self, policy: FullScreenPolicy) -> Self {
        self.full_screen = policy;
        self
    }

    pub fn macos_level(mut self, level: i32) -> Self {
        self.macos_level = level;
        self
    }

    pub fn keep_window_on_dispose(mut self) -> Self {
        self.destroy_on_dispose = false;
        self
    }

    /// Check the spec; a bad one is refused before any window is touched.
    pub fn validate(&self) -> Result<(), OverlayError> {
        if self.id.is_empty() || self.id.len() > 128 || self.id.chars().any(char::is_control) {
            return Err(OverlayError::Invalid("overlay id"));
        }
        let mut chars = self.kind.chars();
        let kind_ok = self.kind.len() <= 32
            && chars.next().is_some_and(|c| c.is_ascii_lowercase())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
        if !kind_ok {
            return Err(OverlayError::Invalid("overlay kind"));
        }
        validate_size(self.size)?;
        if !self.margin.is_finite() || !(0.0..=MAX_OVERLAY_MARGIN).contains(&self.margin) {
            return Err(OverlayError::Invalid("overlay margin"));
        }
        if let OverlayAnchor::At { x, y } = self.anchor {
            if !x.is_finite() || !y.is_finite() || x.abs() > 100_000.0 || y.abs() > 100_000.0 {
                return Err(OverlayError::Invalid("overlay anchor"));
            }
        }
        if !self.style.topmost {
            return Err(OverlayError::Invalid("an overlay is topmost"));
        }
        Ok(())
    }
}

fn validate_size(size: (f64, f64)) -> Result<(), OverlayError> {
    let ok = |v: f64| v.is_finite() && (1.0..=MAX_OVERLAY_EDGE).contains(&v);
    if ok(size.0) && ok(size.1) {
        Ok(())
    } else {
        Err(OverlayError::Invalid("overlay size"))
    }
}

// ---- placement (pure) ------------------------------------------------------------------------

fn area_of(r: OverlayRect) -> WorkArea {
    WorkArea {
        left: f64::from(r.x),
        top: f64::from(r.y),
        right: f64::from(r.x) + f64::from(r.width),
        bottom: f64::from(r.y) + f64::from(r.height),
    }
}

fn contains_point(area: WorkArea, x: f64, y: f64) -> bool {
    x >= area.left && x < area.right && y >= area.top && y < area.bottom
}

fn center_of(r: OverlayRect) -> (f64, f64) {
    (
        f64::from(r.x) + f64::from(r.width) / 2.0,
        f64::from(r.y) + f64::from(r.height) / 2.0,
    )
}

fn inside(outer: WorkArea, inner: OverlayRect) -> bool {
    let a = area_of(inner);
    a.left >= outer.left && a.top >= outer.top && a.right <= outer.right && a.bottom <= outer.bottom
}

/// The overlay's rectangle in physical pixels on `monitor`: the logical size scaled by the
/// monitor's DPI (never larger than its work area), anchored inside the work area, then clamped
/// into it, so the result is always reachable.
pub fn place_overlay(
    size: (f64, f64),
    anchor: OverlayAnchor,
    margin: f64,
    monitor: &MonitorPlacement,
) -> OverlayRect {
    let scale = monitor.scale();
    let work = monitor.work;
    let work_w = (work.right - work.left).max(1.0);
    let work_h = (work.bottom - work.top).max(1.0);
    let w = (size.0 * scale).round().clamp(1.0, work_w);
    let h = (size.1 * scale).round().clamp(1.0, work_h);
    let m = margin * scale;
    let (x, y) = match anchor {
        OverlayAnchor::TopCenter => (work.left + (work_w - w) / 2.0, work.top + m),
        OverlayAnchor::BottomCenter => (work.left + (work_w - w) / 2.0, work.bottom - h - m),
        OverlayAnchor::TopLeft => (work.left + m, work.top + m),
        OverlayAnchor::TopRight => (work.right - w - m, work.top + m),
        OverlayAnchor::BottomLeft => (work.left + m, work.bottom - h - m),
        OverlayAnchor::BottomRight => (work.right - w - m, work.bottom - h - m),
        OverlayAnchor::Center => (
            work.left + (work_w - w) / 2.0,
            work.top + (work_h - h) / 2.0,
        ),
        OverlayAnchor::At { x, y } => (work.left + x * scale, work.top + y * scale),
    };
    let r = work.clamp(Rect {
        x,
        y,
        width: w,
        height: h,
    });
    OverlayRect {
        x: r.x.round() as i32,
        y: r.y.round() as i32,
        width: r.width.round() as i32,
        height: r.height.round() as i32,
    }
}

/// The monitor an overlay lands on: the one containing `hint` (a point in physical pixels, for
/// example the pointer), else the one it was last on, else the first listed (the primary).
pub fn choose_monitor(
    monitors: &[MonitorPlacement],
    hint: Option<(i32, i32)>,
    last: Option<OverlayRect>,
) -> Option<&MonitorPlacement> {
    let by_point = |x: f64, y: f64| monitors.iter().find(|m| contains_point(m.bounds, x, y));
    hint.and_then(|(x, y)| by_point(f64::from(x), f64::from(y)))
        .or_else(|| {
            let (x, y) = center_of(last?);
            by_point(x, y)
        })
        .or_else(|| monitors.first())
}

// ---- the state machine (pure) ----------------------------------------------------------------

/// One thing the surface is asked to do. The core never does it itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayOp {
    /// Move and size the window (physical pixels).
    Place(OverlayRect),
    /// Show it. `activate` is false for every style that does not take focus: the window must come
    /// up in the topmost band without becoming the foreground window.
    Show { activate: bool },
    /// Hide it without touching focus or z-order.
    Hide,
    /// Destroy the window.
    Destroy,
}

/// An auto-hide to schedule: after `after`, call fire with this `generation`. A later show, hide or
/// dispose bumps the generation, so a stale timer does nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoHide {
    pub generation: u64,
    pub after: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Hidden,
    Shown,
    Disposed,
}

/// What an overlay is doing now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayState {
    /// The app asked for it to be up.
    pub wanted: bool,
    /// It is on screen (wanted and not suppressed by a full-screen program).
    pub on_screen: bool,
    pub rect: Option<OverlayRect>,
    pub disposed: bool,
}

/// The overlay state machine. Pure: every method returns the ops to run.
#[derive(Debug, Clone)]
pub struct OverlayCore {
    spec: OverlaySpec,
    size: (f64, f64),
    phase: Phase,
    rect: Option<OverlayRect>,
    placed_dpi: u32,
    foreground_fills: bool,
    generation: u64,
}

pub type Plan = (Vec<OverlayOp>, Option<AutoHide>);

impl OverlayCore {
    pub fn new(spec: OverlaySpec) -> Result<Self, OverlayError> {
        spec.validate()?;
        Ok(Self {
            size: spec.size,
            spec,
            phase: Phase::Hidden,
            rect: None,
            placed_dpi: 96,
            foreground_fills: false,
            generation: 0,
        })
    }

    pub fn spec(&self) -> &OverlaySpec {
        &self.spec
    }

    fn suppressed(&self) -> bool {
        !self.spec.full_screen.shows(self.foreground_fills)
    }

    pub fn state(&self) -> OverlayState {
        let wanted = self.phase == Phase::Shown;
        OverlayState {
            wanted,
            on_screen: wanted && !self.suppressed(),
            rect: self.rect,
            disposed: self.phase == Phase::Disposed,
        }
    }

    fn show_op(&self) -> OverlayOp {
        OverlayOp::Show {
            activate: self.spec.style.takes_focus,
        }
    }

    /// Show (or re-show) the overlay on the monitor chosen by `hint` / its last place / the primary.
    /// Placement is recomputed every time, so a moved monitor or a changed scale is picked up.
    pub fn show(
        &mut self,
        monitors: &[MonitorPlacement],
        hint: Option<(i32, i32)>,
        auto_hide: Option<Duration>,
    ) -> Result<Plan, OverlayError> {
        if self.phase == Phase::Disposed {
            return Err(OverlayError::Disposed);
        }
        let monitor = *choose_monitor(monitors, hint, self.rect).ok_or(OverlayError::NoMonitor)?;
        self.generation += 1;
        let rect = place_overlay(self.size, self.spec.anchor, self.spec.margin, &monitor);
        let mut ops = Vec::with_capacity(2);
        if self.phase != Phase::Shown || self.rect != Some(rect) {
            ops.push(OverlayOp::Place(rect));
        }
        self.rect = Some(rect);
        self.placed_dpi = monitor.dpi;
        self.phase = Phase::Shown;
        if !self.suppressed() {
            ops.push(self.show_op());
        }
        let auto = auto_hide.filter(|d| !d.is_zero()).map(|after| AutoHide {
            generation: self.generation,
            after: after.min(MAX_AUTO_HIDE),
        });
        Ok((ops, auto))
    }

    /// Change the logical size; a shown overlay is re-placed on the monitor it is on.
    pub fn resize(
        &mut self,
        size: (f64, f64),
        monitors: &[MonitorPlacement],
    ) -> Result<Vec<OverlayOp>, OverlayError> {
        if self.phase == Phase::Disposed {
            return Err(OverlayError::Disposed);
        }
        validate_size(size)?;
        self.size = size;
        if self.phase != Phase::Shown {
            return Ok(Vec::new());
        }
        let monitor = *choose_monitor(monitors, None, self.rect).ok_or(OverlayError::NoMonitor)?;
        let rect = place_overlay(self.size, self.spec.anchor, self.spec.margin, &monitor);
        self.placed_dpi = monitor.dpi;
        if self.rect == Some(rect) {
            return Ok(Vec::new());
        }
        self.rect = Some(rect);
        Ok(vec![OverlayOp::Place(rect)])
    }

    pub fn hide(&mut self) -> Vec<OverlayOp> {
        if self.phase != Phase::Shown {
            return Vec::new();
        }
        self.generation += 1;
        let was_on_screen = !self.suppressed();
        self.phase = Phase::Hidden;
        if was_on_screen {
            vec![OverlayOp::Hide]
        } else {
            Vec::new()
        }
    }

    /// A scheduled auto-hide fired. Does nothing when something happened since it was scheduled.
    pub fn fire_auto_hide(&mut self, generation: u64) -> Vec<OverlayOp> {
        if generation != self.generation {
            return Vec::new();
        }
        self.hide()
    }

    /// Release the overlay: hide it, destroy the window when the spec says so, refuse everything
    /// after. A second dispose is a no-op.
    pub fn dispose(&mut self) -> Vec<OverlayOp> {
        if self.phase == Phase::Disposed {
            return Vec::new();
        }
        self.generation += 1;
        let mut ops = Vec::new();
        if self.phase == Phase::Shown && !self.suppressed() {
            ops.push(OverlayOp::Hide);
        }
        if self.spec.destroy_on_dispose {
            ops.push(OverlayOp::Destroy);
        }
        self.phase = Phase::Disposed;
        self.rect = None;
        ops
    }

    /// Whether a program fills the overlay's monitor now. With [`FullScreenPolicy::Hide`] a shown
    /// overlay goes away while it does and comes back after.
    pub fn set_foreground_fills_monitor(&mut self, fills: bool) -> Vec<OverlayOp> {
        if self.phase == Phase::Disposed || fills == self.foreground_fills {
            return Vec::new();
        }
        let was = self.suppressed();
        self.foreground_fills = fills;
        let now = self.suppressed();
        match (self.phase == Phase::Shown, was, now) {
            (true, false, true) => vec![OverlayOp::Hide],
            (true, true, false) => vec![self.show_op()],
            _ => Vec::new(),
        }
    }

    /// The monitors changed (one removed, one moved, a scale changed). A shown overlay whose
    /// centre is on no monitor any more is brought back onto the primary; one that is still on a
    /// monitor is re-placed when its scale changed or it no longer fits the work area. Hidden
    /// overlays are placed at their next show.
    pub fn monitors_changed(&mut self, monitors: &[MonitorPlacement]) -> Vec<OverlayOp> {
        let (Phase::Shown, Some(rect)) = (self.phase, self.rect) else {
            return Vec::new();
        };
        if monitors.is_empty() {
            return Vec::new();
        }
        let (cx, cy) = center_of(rect);
        let current = monitors.iter().find(|m| contains_point(m.bounds, cx, cy));
        let (target, recovering) = match current {
            Some(m) => (*m, false),
            None => (monitors[0], true),
        };
        let wanted = place_overlay(self.size, self.spec.anchor, self.spec.margin, &target);
        let moved = recovering || target.dpi != self.placed_dpi || !inside(target.work, rect);
        if !moved || wanted == rect {
            self.placed_dpi = target.dpi;
            return Vec::new();
        }
        self.rect = Some(wanted);
        self.placed_dpi = target.dpi;
        let mut ops = vec![OverlayOp::Place(wanted)];
        if recovering && !self.suppressed() {
            // Re-assert it above everything on the monitor it was brought to.
            ops.push(self.show_op());
        }
        ops
    }

    /// After a surface refusal: the overlay is not up. (Disposed stays disposed.)
    fn abort(&mut self) {
        if self.phase == Phase::Shown {
            self.phase = Phase::Hidden;
        }
        self.generation += 1;
    }
}

// ---- the surface and the controller ----------------------------------------------------------

/// What the covering check reads from the native z-order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OverlayProbe {
    /// The overlay is in the topmost band.
    pub topmost: bool,
    /// The windows above it, nearest first.
    pub above: Vec<CoveringCandidate>,
}

/// The native window an overlay drives.
pub trait OverlaySurface: Send + 'static {
    /// Do one op. `Show { activate: false }` must not change the foreground window.
    fn apply(&mut self, op: &OverlayOp) -> Result<(), String>;
    /// Read the z-order for the covering check. The default reports nothing above.
    fn probe(&mut self) -> OverlayProbe {
        OverlayProbe {
            topmost: true,
            above: Vec::new(),
        }
    }
}

/// An [`OverlayCore`] with its surface.
pub struct Overlay<S: OverlaySurface> {
    core: OverlayCore,
    surface: S,
}

impl<S: OverlaySurface> Overlay<S> {
    pub fn new(spec: OverlaySpec, surface: S) -> Result<Self, OverlayError> {
        Ok(Self {
            core: OverlayCore::new(spec)?,
            surface,
        })
    }

    pub fn spec(&self) -> &OverlaySpec {
        self.core.spec()
    }

    pub fn state(&self) -> OverlayState {
        self.core.state()
    }

    /// The surface (tests read what a fake recorded).
    pub fn surface(&self) -> &S {
        &self.surface
    }

    fn run(&mut self, ops: Vec<OverlayOp>) -> Result<(), OverlayError> {
        for op in ops {
            if let Err(why) = self.surface.apply(&op) {
                // Not up after a refusal; take it down if it half came up.
                self.core.abort();
                let _ = self.surface.apply(&OverlayOp::Hide);
                return Err(OverlayError::Surface(why));
            }
        }
        Ok(())
    }

    pub fn show(
        &mut self,
        monitors: &[MonitorPlacement],
        hint: Option<(i32, i32)>,
        auto_hide: Option<Duration>,
    ) -> Result<Option<AutoHide>, OverlayError> {
        let (ops, auto) = self.core.show(monitors, hint, auto_hide)?;
        self.run(ops)?;
        Ok(auto)
    }

    pub fn resize(
        &mut self,
        size: (f64, f64),
        monitors: &[MonitorPlacement],
    ) -> Result<(), OverlayError> {
        let ops = self.core.resize(size, monitors)?;
        self.run(ops)
    }

    pub fn hide(&mut self) -> Result<(), OverlayError> {
        let ops = self.core.hide();
        self.run(ops)
    }

    pub fn fire_auto_hide(&mut self, generation: u64) -> Result<(), OverlayError> {
        let ops = self.core.fire_auto_hide(generation);
        self.run(ops)
    }

    pub fn set_foreground_fills_monitor(&mut self, fills: bool) -> Result<(), OverlayError> {
        let ops = self.core.set_foreground_fills_monitor(fills);
        self.run(ops)
    }

    pub fn monitors_changed(&mut self, monitors: &[MonitorPlacement]) -> Result<(), OverlayError> {
        let ops = self.core.monitors_changed(monitors);
        self.run(ops)
    }

    /// Dispose; surface errors on the way out are ignored, the overlay is gone either way.
    pub fn dispose(&mut self) {
        for op in self.core.dispose() {
            let _ = self.surface.apply(&op);
        }
    }

    /// What covers the overlay now: `"not_topmost"` when it left the topmost band, else the class
    /// name of the nearest visible foreign window above it that overlaps it. `None` when nothing
    /// does, or the overlay is not on screen.
    pub fn covering(&mut self) -> Option<String> {
        let state = self.core.state();
        let rect = state.rect.filter(|_| state.on_screen)?;
        let probe = self.surface.probe();
        covering_window(probe.topmost, area_of(rect), &probe.above)
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The registry of overlays, generic over the surface.
pub struct OverlayHost<S: OverlaySurface> {
    overlays: Mutex<HashMap<String, Arc<Mutex<Overlay<S>>>>>,
}

impl<S: OverlaySurface> Default for OverlayHost<S> {
    fn default() -> Self {
        Self {
            overlays: Mutex::new(HashMap::new()),
        }
    }
}

impl<S: OverlaySurface> OverlayHost<S> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, spec: OverlaySpec, surface: S) -> Result<(), OverlayError> {
        let id = spec.id.clone();
        let overlay = Overlay::new(spec, surface)?;
        let mut map = lock(&self.overlays);
        if map.contains_key(&id) {
            return Err(OverlayError::Duplicate(id));
        }
        map.insert(id, Arc::new(Mutex::new(overlay)));
        Ok(())
    }

    fn get(&self, id: &str) -> Result<Arc<Mutex<Overlay<S>>>, OverlayError> {
        lock(&self.overlays)
            .get(id)
            .cloned()
            .ok_or_else(|| OverlayError::Unknown(id.to_owned()))
    }

    pub fn ids(&self) -> Vec<String> {
        let mut ids: Vec<_> = lock(&self.overlays).keys().cloned().collect();
        ids.sort();
        ids
    }

    pub fn len(&self) -> usize {
        lock(&self.overlays).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn show(
        &self,
        id: &str,
        monitors: &[MonitorPlacement],
        hint: Option<(i32, i32)>,
        auto_hide: Option<Duration>,
    ) -> Result<Option<AutoHide>, OverlayError> {
        let overlay = self.get(id)?;
        let mut overlay = lock(&overlay);
        overlay.show(monitors, hint, auto_hide)
    }

    pub fn hide(&self, id: &str) -> Result<(), OverlayError> {
        let overlay = self.get(id)?;
        let mut overlay = lock(&overlay);
        overlay.hide()
    }

    pub fn resize(
        &self,
        id: &str,
        size: (f64, f64),
        monitors: &[MonitorPlacement],
    ) -> Result<(), OverlayError> {
        let overlay = self.get(id)?;
        let mut overlay = lock(&overlay);
        overlay.resize(size, monitors)
    }

    pub fn fire_auto_hide(&self, id: &str, generation: u64) {
        if let Ok(overlay) = self.get(id) {
            let _ = lock(&overlay).fire_auto_hide(generation);
        }
    }

    pub fn state(&self, id: &str) -> Option<OverlayState> {
        self.get(id).ok().map(|o| lock(&o).state())
    }

    pub fn covering(&self, id: &str) -> Result<Option<String>, OverlayError> {
        let overlay = self.get(id)?;
        let mut overlay = lock(&overlay);
        Ok(overlay.covering())
    }

    pub fn set_foreground_fills_monitor(&self, id: &str, fills: bool) -> Result<(), OverlayError> {
        let overlay = self.get(id)?;
        let mut overlay = lock(&overlay);
        overlay.set_foreground_fills_monitor(fills)
    }

    /// Tell every overlay the monitors changed. Returns the ids that could not be re-placed.
    pub fn monitors_changed(&self, monitors: &[MonitorPlacement]) -> Vec<String> {
        let all: Vec<_> = lock(&self.overlays)
            .iter()
            .map(|(id, o)| (id.clone(), Arc::clone(o)))
            .collect();
        all.into_iter()
            .filter_map(|(id, o)| lock(&o).monitors_changed(monitors).err().map(|_| id))
            .collect()
    }

    /// Dispose and forget. The surface is dropped with the overlay, so nothing outlives it.
    pub fn dispose(&self, id: &str) -> Result<(), OverlayError> {
        let removed = lock(&self.overlays)
            .remove(id)
            .ok_or_else(|| OverlayError::Unknown(id.to_owned()))?;
        lock(&removed).dispose();
        Ok(())
    }

    /// Dispose every overlay (app exit).
    pub fn dispose_all(&self) {
        let all: Vec<_> = lock(&self.overlays).drain().map(|(_, o)| o).collect();
        for overlay in all {
            lock(&overlay).dispose();
        }
    }
}

// ---- auto-hide timers ------------------------------------------------------------------------

type FireFn = Arc<dyn Fn(&str, u64) + Send + Sync>;

#[derive(Default)]
struct TimerState {
    deadlines: HashMap<String, (u64, Instant)>,
    running: bool,
}

/// One short-lived thread for every pending auto-hide: it exists only while one is pending.
struct Timers {
    state: Mutex<TimerState>,
    cv: Condvar,
    fire: FireFn,
}

impl Timers {
    fn new(fire: FireFn) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(TimerState::default()),
            cv: Condvar::new(),
            fire,
        })
    }

    fn schedule(self: &Arc<Self>, id: &str, auto: AutoHide) {
        let mut st = lock(&self.state);
        st.deadlines.insert(
            id.to_owned(),
            (auto.generation, Instant::now() + auto.after),
        );
        self.cv.notify_all();
        if st.running {
            return;
        }
        st.running = true;
        drop(st);
        let this = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("rk-shell-overlay-timer".into())
            .spawn(move || this.run());
        if spawned.is_err() {
            lock(&self.state).running = false;
        }
    }

    fn cancel(&self, id: &str) {
        lock(&self.state).deadlines.remove(id);
        self.cv.notify_all();
    }

    #[cfg(test)]
    fn pending(&self) -> usize {
        lock(&self.state).deadlines.len()
    }

    fn run(self: Arc<Self>) {
        let mut st = lock(&self.state);
        loop {
            let Some((id, (generation, at))) = st
                .deadlines
                .iter()
                .min_by_key(|(_, (_, at))| *at)
                .map(|(id, v)| (id.clone(), *v))
            else {
                st.running = false;
                return;
            };
            let now = Instant::now();
            if now >= at {
                st.deadlines.remove(&id);
                drop(st);
                (self.fire)(&id, generation);
                st = lock(&self.state);
            } else {
                st = self
                    .cv
                    .wait_timeout(st, at - now)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
        }
    }
}

// ---- Tauri ---------------------------------------------------------------------------------

use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, WebviewWindow, Wry};

/// An overlay over a Tauri window the app declared (hidden, undecorated, transparent).
pub struct TauriSurface {
    window: WebviewWindow<Wry>,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    macos_level: i32,
}

impl TauriSurface {
    /// Prepare `window` for `spec`: topmost, off the taskbar, click-through as asked, and on Windows
    /// the no-activate guard (installed on the thread that owns the window, before it is shown).
    pub fn new(window: WebviewWindow<Wry>, spec: &OverlaySpec) -> Self {
        let _ = window.set_always_on_top(true);
        let _ = window.set_skip_taskbar(true);
        let _ = window.set_ignore_cursor_events(spec.style.click_through);
        #[cfg(windows)]
        if !spec.style.takes_focus {
            if let Ok(hwnd) = window.hwnd() {
                let raw = hwnd.0 as isize;
                let _ = window.run_on_main_thread(move || {
                    rightkit_overlay::windows::make_noactivate(raw);
                });
            }
        }
        Self {
            window,
            macos_level: spec.macos_level,
        }
    }

    #[cfg(windows)]
    fn raw_hwnd(&self) -> Option<isize> {
        self.window.hwnd().ok().map(|h| h.0 as isize)
    }
}

impl OverlaySurface for TauriSurface {
    fn apply(&mut self, op: &OverlayOp) -> Result<(), String> {
        match *op {
            OverlayOp::Place(r) => {
                let w = u32::try_from(r.width.max(1)).unwrap_or(1);
                let h = u32::try_from(r.height.max(1)).unwrap_or(1);
                self.window
                    .set_size(PhysicalSize::new(w, h))
                    .map_err(|e| e.to_string())?;
                self.window
                    .set_position(PhysicalPosition::new(r.x, r.y))
                    .map_err(|e| e.to_string())
            }
            OverlayOp::Show { activate } => self.show(activate),
            OverlayOp::Hide => self.hide(),
            OverlayOp::Destroy => self.window.destroy().map_err(|e| e.to_string()),
        }
    }

    fn probe(&mut self) -> OverlayProbe {
        #[cfg(windows)]
        if let Some(hwnd) = self.raw_hwnd() {
            return native_windows::probe(hwnd);
        }
        OverlayProbe {
            topmost: true,
            above: Vec::new(),
        }
    }
}

impl TauriSurface {
    fn show(&mut self, activate: bool) -> Result<(), String> {
        if activate {
            self.window.show().map_err(|e| e.to_string())?;
            return self.window.set_focus().map_err(|e| e.to_string());
        }
        #[cfg(windows)]
        {
            let hwnd = self.raw_hwnd().ok_or("native window handle unavailable")?;
            if rightkit_overlay::windows::show_noactivate(hwnd) {
                Ok(())
            } else {
                Err("the window could not be shown without activation".into())
            }
        }
        #[cfg(target_os = "macos")]
        {
            let level = self.macos_level;
            crate::windows::dispatch_native(&self.window, move |w| {
                use objc2_app_kit::NSWindow;
                let ptr = w.ns_window().map_err(|e| e.to_string())?;
                if ptr.is_null() {
                    return Err("native window handle unavailable".into());
                }
                // SAFETY: Tauri returns the live NSWindow for this window; used synchronously on
                // the main thread.
                let native = unsafe { &*(ptr as *const NSWindow) };
                native.setLevel(level as isize);
                native.orderFrontRegardless();
                Ok(())
            })
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            self.window.show().map_err(|e| e.to_string())
        }
    }

    fn hide(&mut self) -> Result<(), String> {
        #[cfg(windows)]
        if let Some(hwnd) = self.raw_hwnd() {
            return if rightkit_overlay::windows::hide(hwnd) {
                Ok(())
            } else {
                self.window.hide().map_err(|e| e.to_string())
            };
        }
        #[cfg(target_os = "macos")]
        {
            crate::windows::dispatch_native(&self.window, |w| {
                use objc2_app_kit::NSWindow;
                let ptr = w.ns_window().map_err(|e| e.to_string())?;
                if ptr.is_null() {
                    return Err("native window handle unavailable".into());
                }
                // SAFETY: as in `show`.
                let native = unsafe { &*(ptr as *const NSWindow) };
                native.orderOut(None);
                Ok(())
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.window.hide().map_err(|e| e.to_string())
        }
    }
}

#[cfg(windows)]
mod native_windows {
    //! The z-order walk for the covering check: the windows above the overlay, nearest first.
    use super::OverlayProbe;
    use crate::winapi::windows::Win32::Foundation::{HWND, RECT};
    use crate::winapi::windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    use crate::winapi::windows::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetWindow, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId,
        IsWindowVisible, GWL_EXSTYLE, GW_HWNDPREV, WS_EX_TOPMOST,
    };
    use rightkit_overlay::{CoveringCandidate, WorkArea, MAX_COVERING_WALK};

    fn handle(raw: isize) -> HWND {
        HWND(raw as *mut std::ffi::c_void)
    }

    fn candidate(window: HWND) -> CoveringCandidate {
        // SAFETY: plain reads of a window handle; every call tolerates a handle that has gone away
        // (it fails, and the candidate reports not visible).
        unsafe {
            let mut class = [0u16; 256];
            let len = GetClassNameW(window, &mut class).max(0) as usize;
            let class_name = String::from_utf16_lossy(&class[..len.min(class.len())]);
            let visible = IsWindowVisible(window).as_bool();
            let mut pid = 0u32;
            GetWindowThreadProcessId(window, Some(&mut pid));
            let mut cloaked = 0u32;
            let cloaked = DwmGetWindowAttribute(
                window,
                DWMWA_CLOAKED,
                (&mut cloaked as *mut u32).cast(),
                std::mem::size_of::<u32>() as u32,
            )
            .is_ok()
                && cloaked != 0;
            let mut rect = RECT::default();
            let have_rect = GetWindowRect(window, &mut rect).is_ok();
            CoveringCandidate {
                class_name,
                visible: visible && have_rect,
                own_process: pid == std::process::id(),
                cloaked,
                rect: WorkArea {
                    left: f64::from(rect.left),
                    top: f64::from(rect.top),
                    right: f64::from(rect.right),
                    bottom: f64::from(rect.bottom),
                },
            }
        }
    }

    pub(super) fn probe(raw: isize) -> OverlayProbe {
        let overlay = handle(raw);
        // SAFETY: as in `candidate`.
        let topmost =
            unsafe { GetWindowLongPtrW(overlay, GWL_EXSTYLE) } as u32 & WS_EX_TOPMOST.0 != 0;
        let mut above = Vec::new();
        let mut current = overlay;
        while above.len() < MAX_COVERING_WALK {
            // SAFETY: GW_HWNDPREV is the window directly above `current` in the z-order.
            match unsafe { GetWindow(current, GW_HWNDPREV) } {
                Ok(prev) if !prev.0.is_null() => {
                    above.push(candidate(prev));
                    current = prev;
                }
                _ => break,
            }
        }
        OverlayProbe { topmost, above }
    }
}

fn monitors_of(app: &AppHandle<Wry>) -> Vec<MonitorPlacement> {
    let to_area = |pos: &PhysicalPosition<i32>, size: &PhysicalSize<u32>| WorkArea {
        left: f64::from(pos.x),
        top: f64::from(pos.y),
        right: f64::from(pos.x) + f64::from(size.width),
        bottom: f64::from(pos.y) + f64::from(size.height),
    };
    let one = |m: &tauri::Monitor| {
        let scale = m.scale_factor();
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let work = m.work_area();
        MonitorPlacement {
            bounds: to_area(m.position(), m.size()),
            work: to_area(&work.position, &work.size),
            dpi: (scale * 96.0).round().max(1.0) as u32,
        }
    };
    let primary = app.primary_monitor().ok().flatten().map(|m| one(&m));
    let mut all: Vec<MonitorPlacement> = app
        .available_monitors()
        .map(|ms| ms.iter().map(one).collect())
        .unwrap_or_default();
    if let Some(primary) = primary {
        match all.iter().position(|m| *m == primary) {
            Some(0) => {}
            Some(ix) => {
                let p = all.remove(ix);
                all.insert(0, p);
            }
            None => all.insert(0, primary),
        }
    }
    all
}

/// What the app can say about one `show`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShowOptions {
    /// Take the overlay down again after this long (at most [`MAX_AUTO_HIDE`]).
    pub auto_hide: Option<Duration>,
    /// Place it on the monitor containing this physical point. Default: the pointer.
    pub near: Option<(i32, i32)>,
}

/// The overlays of one app over Tauri windows. Manage one: `app.manage(OverlayService::new())`.
pub struct OverlayService {
    host: Arc<OverlayHost<TauriSurface>>,
    timers: Arc<Timers>,
}

impl Default for OverlayService {
    fn default() -> Self {
        Self::new()
    }
}

impl OverlayService {
    pub fn new() -> Self {
        let host: Arc<OverlayHost<TauriSurface>> = Arc::new(OverlayHost::new());
        let weak: Weak<OverlayHost<TauriSurface>> = Arc::downgrade(&host);
        let timers = Timers::new(Arc::new(move |id, generation| {
            if let Some(host) = weak.upgrade() {
                host.fire_auto_hide(id, generation);
            }
        }));
        Self { host, timers }
    }

    /// Take the window labelled `spec.id` as an overlay. The window must exist and be hidden.
    pub fn attach(&self, app: &AppHandle<Wry>, spec: OverlaySpec) -> Result<(), OverlayError> {
        spec.validate()?;
        let window = app
            .get_webview_window(&spec.id)
            .ok_or_else(|| OverlayError::Unknown(spec.id.clone()))?;
        let surface = TauriSurface::new(window, &spec);
        self.host.register(spec, surface)
    }

    pub fn show(
        &self,
        app: &AppHandle<Wry>,
        id: &str,
        options: ShowOptions,
    ) -> Result<(), OverlayError> {
        let monitors = monitors_of(app);
        let hint = options.near.or_else(|| {
            app.cursor_position()
                .ok()
                .map(|p| (p.x.round() as i32, p.y.round() as i32))
        });
        if let Some(auto) = self.host.show(id, &monitors, hint, options.auto_hide)? {
            self.timers.schedule(id, auto);
        }
        Ok(())
    }

    pub fn hide(&self, id: &str) -> Result<(), OverlayError> {
        self.timers.cancel(id);
        self.host.hide(id)
    }

    pub fn resize(
        &self,
        app: &AppHandle<Wry>,
        id: &str,
        size: (f64, f64),
    ) -> Result<(), OverlayError> {
        self.host.resize(id, size, &monitors_of(app))
    }

    /// Call after a display change (a monitor added, removed or rescaled): a shown overlay that is
    /// no longer on any monitor comes back onto the primary. Returns the ids that could not move.
    pub fn monitors_changed(&self, app: &AppHandle<Wry>) -> Vec<String> {
        self.host.monitors_changed(&monitors_of(app))
    }

    /// Re-read whether another program fills the screen and apply each overlay's policy. On
    /// Windows this asks the foreground window; elsewhere nothing fills (the answer is `false`).
    /// Call it on foreground changes; it starts no thread.
    pub fn refresh_full_screen(&self) {
        let fills = rightkit_overlay::windows::foreground_is_borderless_fullscreen(&[]);
        for id in self.host.ids() {
            let _ = self.host.set_foreground_fills_monitor(&id, fills);
        }
    }

    pub fn covering(&self, id: &str) -> Result<Option<String>, OverlayError> {
        self.host.covering(id)
    }

    pub fn state(&self, id: &str) -> Option<OverlayState> {
        self.host.state(id)
    }

    /// Dispose one overlay (cancels its timer, hides it, destroys its window, forgets it).
    pub fn dispose(&self, id: &str) -> Result<(), OverlayError> {
        self.timers.cancel(id);
        self.host.dispose(id)
    }

    /// Dispose every overlay (app exit).
    pub fn dispose_all(&self) {
        for id in self.host.ids() {
            self.timers.cancel(&id);
        }
        self.host.dispose_all();
    }

    /// How many overlays are registered.
    pub fn len(&self) -> usize {
        self.host.len()
    }

    pub fn is_empty(&self) -> bool {
        self.host.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(l: f64, t: f64, r: f64, b: f64) -> WorkArea {
        WorkArea {
            left: l,
            top: t,
            right: r,
            bottom: b,
        }
    }

    /// 1920x1080 at 100%, taskbar 40 px at the bottom.
    fn primary() -> MonitorPlacement {
        MonitorPlacement {
            bounds: area(0.0, 0.0, 1920.0, 1080.0),
            work: area(0.0, 0.0, 1920.0, 1040.0),
            dpi: 96,
        }
    }

    /// 3840x2160 at 200%, to the right of the primary.
    fn hidpi_right() -> MonitorPlacement {
        MonitorPlacement {
            bounds: area(1920.0, 0.0, 5760.0, 2160.0),
            work: area(1920.0, 0.0, 5760.0, 2160.0),
            dpi: 192,
        }
    }

    fn spec() -> OverlaySpec {
        OverlaySpec::new("pill", "pill", (200.0, 40.0))
    }

    #[test]
    fn a_spec_is_checked_before_any_window_is_touched() {
        assert!(spec().validate().is_ok());
        for bad in [
            OverlaySpec::new("", "pill", (1.0, 1.0)),
            OverlaySpec::new("p", "Pill", (1.0, 1.0)),
            OverlaySpec::new("p", "", (1.0, 1.0)),
            OverlaySpec::new("p", "heard right", (1.0, 1.0)),
            OverlaySpec::new("p", &"a".repeat(33), (1.0, 1.0)),
            OverlaySpec::new("p", "pill", (0.0, 10.0)),
            OverlaySpec::new("p", "pill", (f64::NAN, 10.0)),
            OverlaySpec::new("p", "pill", (10.0, MAX_OVERLAY_EDGE + 1.0)),
            spec().margin(-1.0),
            spec().margin(f64::INFINITY),
            spec().anchor(OverlayAnchor::At {
                x: f64::NAN,
                y: 0.0,
            }),
            spec().style(OverlayStyle {
                topmost: false,
                ..OverlayStyle::PASSIVE
            }),
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn placement_scales_with_the_monitor_and_stays_inside_its_work_area() {
        // Top centre of the primary, 12 px below the top edge.
        let r = place_overlay((200.0, 40.0), OverlayAnchor::TopCenter, 12.0, &primary());
        assert_eq!(
            r,
            OverlayRect {
                x: 860,
                y: 12,
                width: 200,
                height: 40
            }
        );
        // The same overlay on the 200% monitor is twice as large in pixels and still centred.
        let r = place_overlay(
            (200.0, 40.0),
            OverlayAnchor::TopCenter,
            12.0,
            &hidpi_right(),
        );
        assert_eq!(
            r,
            OverlayRect {
                x: 1920 + (3840 - 400) / 2,
                y: 24,
                width: 400,
                height: 80
            }
        );
        // Bottom anchors keep clear of the taskbar (the work area, not the bounds).
        let r = place_overlay((200.0, 40.0), OverlayAnchor::BottomCenter, 12.0, &primary());
        assert_eq!(r.y + r.height, 1040 - 12);
        // An overlay larger than the work area is fitted, and a far offset is clamped back in.
        let r = place_overlay((4000.0, 4000.0), OverlayAnchor::Center, 0.0, &primary());
        assert_eq!((r.width, r.height), (1920, 1040));
        let r = place_overlay(
            (100.0, 100.0),
            OverlayAnchor::At {
                x: 99_000.0,
                y: 99_000.0,
            },
            0.0,
            &primary(),
        );
        assert_eq!((r.x + r.width, r.y + r.height), (1920, 1040));
    }

    #[test]
    fn the_monitor_is_the_hinted_one_then_the_last_one_then_the_primary() {
        let monitors = [primary(), hidpi_right()];
        let on_right = choose_monitor(&monitors, Some((3000, 500)), None).unwrap();
        assert_eq!(on_right.dpi, 192);
        let last = OverlayRect {
            x: 2500,
            y: 10,
            width: 100,
            height: 50,
        };
        assert_eq!(
            choose_monitor(&monitors, None, Some(last)).unwrap().dpi,
            192
        );
        // A hint on no monitor falls back to the last place, then the primary.
        assert_eq!(
            choose_monitor(&monitors, Some((-5000, -5000)), Some(last))
                .unwrap()
                .dpi,
            192
        );
        assert_eq!(choose_monitor(&monitors, None, None).unwrap().dpi, 96);
        assert!(choose_monitor(&[], None, None).is_none());
    }

    #[test]
    fn show_places_then_shows_without_activation_for_a_passive_style() {
        let mut core = OverlayCore::new(spec()).unwrap();
        let (ops, auto) = core.show(&[primary()], None, None).unwrap();
        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0], OverlayOp::Place(_)));
        assert_eq!(ops[1], OverlayOp::Show { activate: false });
        assert!(auto.is_none());
        assert!(core.state().on_screen);
        // Showing again in the same place only re-asserts the show.
        let (ops, _) = core.show(&[primary()], None, None).unwrap();
        assert_eq!(ops, vec![OverlayOp::Show { activate: false }]);
        // A keyboard style is the only kind that activates.
        let launcher =
            OverlaySpec::new("l", "launcher", (400.0, 60.0)).style(OverlayStyle::KEYBOARD);
        let (ops, _) = OverlayCore::new(launcher)
            .unwrap()
            .show(&[primary()], None, None)
            .unwrap();
        assert_eq!(ops.last(), Some(&OverlayOp::Show { activate: true }));
    }

    #[test]
    fn no_op_the_core_emits_for_a_passive_overlay_ever_activates() {
        let mut core = OverlayCore::new(spec().full_screen(FullScreenPolicy::Hide)).unwrap();
        let mut all = Vec::new();
        let monitors = [primary(), hidpi_right()];
        all.extend(
            core.show(&monitors, Some((3000, 10)), Some(Duration::from_secs(1)))
                .unwrap()
                .0,
        );
        all.extend(core.set_foreground_fills_monitor(true));
        all.extend(core.set_foreground_fills_monitor(false));
        all.extend(core.monitors_changed(&[primary()]));
        all.extend(core.resize((300.0, 50.0), &[primary()]).unwrap());
        all.extend(core.hide());
        all.extend(core.show(&monitors, None, None).unwrap().0);
        all.extend(core.dispose());
        assert!(!all.is_empty());
        assert!(
            !all.contains(&OverlayOp::Show { activate: true }),
            "an overlay that does not take focus must never ask to activate: {all:?}"
        );
    }

    #[test]
    fn auto_hide_is_data_and_a_later_event_makes_the_old_timer_inert() {
        let mut core = OverlayCore::new(spec()).unwrap();
        let (_, auto) = core
            .show(&[primary()], None, Some(Duration::from_secs(3)))
            .unwrap();
        let first = auto.unwrap();
        assert_eq!(first.after, Duration::from_secs(3));
        // A second show re-arms with a new generation; the first timer firing does nothing.
        let (_, auto) = core
            .show(&[primary()], None, Some(Duration::from_secs(5)))
            .unwrap();
        let second = auto.unwrap();
        assert_ne!(first.generation, second.generation);
        assert!(core.fire_auto_hide(first.generation).is_empty());
        assert!(core.state().on_screen);
        assert_eq!(
            core.fire_auto_hide(second.generation),
            vec![OverlayOp::Hide]
        );
        assert!(!core.state().wanted);
        // A hide cancels a pending timer the same way.
        let (_, auto) = core
            .show(&[primary()], None, Some(Duration::from_secs(1)))
            .unwrap();
        core.hide();
        assert!(core.fire_auto_hide(auto.unwrap().generation).is_empty());
        // The wait is capped; a zero wait means none.
        let (_, auto) = core
            .show(&[primary()], None, Some(Duration::from_secs(86_400)))
            .unwrap();
        assert_eq!(auto.unwrap().after, MAX_AUTO_HIDE);
        let (_, auto) = core.show(&[primary()], None, Some(Duration::ZERO)).unwrap();
        assert!(auto.is_none());
    }

    #[test]
    fn a_full_screen_program_hides_a_hide_policy_overlay_and_it_returns() {
        let mut core = OverlayCore::new(spec().full_screen(FullScreenPolicy::Hide)).unwrap();
        core.show(&[primary()], None, None).unwrap();
        assert_eq!(
            core.set_foreground_fills_monitor(true),
            vec![OverlayOp::Hide]
        );
        let state = core.state();
        assert!(state.wanted && !state.on_screen);
        // Shown while suppressed: placed, but not made visible.
        let (ops, _) = core.show(&[primary()], None, None).unwrap();
        assert!(ops.iter().all(|op| !matches!(op, OverlayOp::Show { .. })));
        assert_eq!(
            core.set_foreground_fills_monitor(false),
            vec![OverlayOp::Show { activate: false }]
        );
        // A stay-above overlay ignores full screen.
        let mut above = OverlayCore::new(spec()).unwrap();
        above.show(&[primary()], None, None).unwrap();
        assert!(above.set_foreground_fills_monitor(true).is_empty());
        assert!(above.state().on_screen);
    }

    #[test]
    fn a_shown_overlay_is_brought_back_when_its_monitor_goes_away() {
        let mut core = OverlayCore::new(spec()).unwrap();
        let both = [primary(), hidpi_right()];
        core.show(&both, Some((3000, 10)), None).unwrap();
        let before = core.state().rect.unwrap();
        assert!(before.x >= 1920, "placed on the right monitor");
        // The right monitor is unplugged: the overlay lands on the primary, visibly.
        let ops = core.monitors_changed(&[primary()]);
        let rect = core.state().rect.unwrap();
        assert_eq!(ops[0], OverlayOp::Place(rect));
        assert_eq!(ops[1], OverlayOp::Show { activate: false });
        assert!(inside(primary().work, rect), "{rect:?}");
        assert_eq!(
            (rect.width, rect.height),
            (200, 40),
            "size follows the new scale"
        );
        // Nothing changed: nothing to do.
        assert!(core.monitors_changed(&[primary()]).is_empty());
    }

    #[test]
    fn a_rescaled_monitor_re_places_the_overlay_and_a_hidden_one_waits() {
        let mut core = OverlayCore::new(spec()).unwrap();
        core.show(&[primary()], None, None).unwrap();
        let mut scaled = primary();
        scaled.dpi = 144;
        let ops = core.monitors_changed(&[scaled]);
        assert_eq!(ops.len(), 1);
        let rect = core.state().rect.unwrap();
        assert_eq!((rect.width, rect.height), (300, 60), "150% of 200x40");
        core.hide();
        assert!(core.monitors_changed(&[primary()]).is_empty());
        // The next show lands correctly for the monitor then.
        let (ops, _) = core.show(&[primary()], None, None).unwrap();
        assert!(matches!(ops[0], OverlayOp::Place(r) if r.width == 200));
    }

    #[test]
    fn show_without_a_monitor_and_after_dispose_are_refused() {
        let mut core = OverlayCore::new(spec()).unwrap();
        assert_eq!(
            core.show(&[], None, None).unwrap_err(),
            OverlayError::NoMonitor
        );
        core.show(&[primary()], None, None).unwrap();
        let ops = core.dispose();
        assert_eq!(ops, vec![OverlayOp::Hide, OverlayOp::Destroy]);
        assert!(core.dispose().is_empty(), "dispose is idempotent");
        assert_eq!(
            core.show(&[primary()], None, None).unwrap_err(),
            OverlayError::Disposed
        );
        assert_eq!(
            core.resize((10.0, 10.0), &[primary()]).unwrap_err(),
            OverlayError::Disposed
        );
        assert!(core.state().disposed);
        // An app that owns its window keeps it.
        let mut keep = OverlayCore::new(spec().keep_window_on_dispose()).unwrap();
        assert!(keep.dispose().is_empty());
    }

    // ---- over a fake surface ----

    /// A window system in miniature: who has the foreground, what is visible, and what was asked.
    #[derive(Default)]
    struct World {
        foreground: String,
        visible: bool,
        rect: Option<OverlayRect>,
        destroyed: u32,
        log: Vec<OverlayOp>,
        fail_show: bool,
        candidates: Vec<CoveringCandidate>,
        topmost: bool,
    }

    struct FakeSurface {
        world: Arc<Mutex<World>>,
        /// Dropped with the surface; tests watch its strong count to see nothing leaks.
        _token: Arc<()>,
    }

    impl OverlaySurface for FakeSurface {
        fn apply(&mut self, op: &OverlayOp) -> Result<(), String> {
            let mut w = lock(&self.world);
            w.log.push(*op);
            match *op {
                OverlayOp::Place(r) => w.rect = Some(r),
                OverlayOp::Show { activate } => {
                    if w.fail_show {
                        return Err("refused".into());
                    }
                    w.visible = true;
                    if activate {
                        w.foreground = "overlay".into();
                    }
                }
                OverlayOp::Hide => w.visible = false,
                OverlayOp::Destroy => {
                    w.visible = false;
                    w.destroyed += 1;
                }
            }
            Ok(())
        }

        fn probe(&mut self) -> OverlayProbe {
            let w = lock(&self.world);
            OverlayProbe {
                topmost: w.topmost,
                above: w.candidates.clone(),
            }
        }
    }

    fn fake() -> (Arc<Mutex<World>>, Arc<()>, FakeSurface) {
        let world = Arc::new(Mutex::new(World {
            foreground: "other-app".into(),
            topmost: true,
            ..World::default()
        }));
        let token = Arc::new(());
        let surface = FakeSurface {
            world: Arc::clone(&world),
            _token: Arc::clone(&token),
        };
        (world, token, surface)
    }

    #[test]
    fn showing_and_clicking_through_the_lifecycle_leaves_the_users_app_in_front() {
        let (world, _token, surface) = fake();
        let host = OverlayHost::new();
        host.register(spec(), surface).unwrap();
        host.show("pill", &[primary()], None, Some(Duration::from_secs(2)))
            .unwrap();
        {
            let w = lock(&world);
            assert!(w.visible);
            assert_eq!(
                w.foreground, "other-app",
                "focus stays with the previous app"
            );
        }
        host.resize("pill", (260.0, 44.0), &[primary()]).unwrap();
        host.hide("pill").unwrap();
        host.show("pill", &[primary()], None, None).unwrap();
        assert_eq!(lock(&world).foreground, "other-app");
        // A launcher style does take the keyboard.
        let (world2, _t2, surface2) = fake();
        let launcher =
            OverlaySpec::new("l", "launcher", (400.0, 60.0)).style(OverlayStyle::KEYBOARD);
        host.register(launcher, surface2).unwrap();
        host.show("l", &[primary()], None, None).unwrap();
        assert_eq!(lock(&world2).foreground, "overlay");
    }

    #[test]
    fn off_screen_recovery_runs_through_the_surface_when_a_monitor_is_removed() {
        let (world, _token, surface) = fake();
        let host = OverlayHost::new();
        host.register(spec(), surface).unwrap();
        let both = [primary(), hidpi_right()];
        host.show("pill", &both, Some((4000, 10)), None).unwrap();
        assert!(lock(&world).rect.unwrap().x >= 1920);
        let failed = host.monitors_changed(&[primary()]);
        assert!(failed.is_empty());
        let w = lock(&world);
        let rect = w.rect.unwrap();
        assert!(inside(primary().work, rect), "{rect:?}");
        assert!(w.visible);
    }

    #[test]
    fn a_surface_refusal_leaves_the_overlay_down_and_reports_it() {
        let (world, _token, surface) = fake();
        let host = OverlayHost::new();
        host.register(spec(), surface).unwrap();
        lock(&world).fail_show = true;
        let err = host.show("pill", &[primary()], None, None).unwrap_err();
        assert_eq!(err, OverlayError::Surface("refused".into()));
        assert!(!host.state("pill").unwrap().wanted);
        assert!(!lock(&world).visible);
        // It recovers when the surface does.
        lock(&world).fail_show = false;
        host.show("pill", &[primary()], None, None).unwrap();
        assert!(lock(&world).visible);
    }

    #[test]
    fn dispose_hides_destroys_once_forgets_and_leaks_nothing() {
        let (world, token, surface) = fake();
        let host = OverlayHost::new();
        host.register(spec(), surface).unwrap();
        host.show("pill", &[primary()], None, Some(Duration::from_secs(60)))
            .unwrap();
        assert_eq!(Arc::strong_count(&token), 2, "the surface holds the token");
        assert_eq!(
            host.register(spec(), fake().2).unwrap_err(),
            OverlayError::Duplicate("pill".into())
        );
        host.dispose("pill").unwrap();
        assert_eq!(
            Arc::strong_count(&token),
            1,
            "the surface was dropped with the overlay"
        );
        assert!(host.is_empty());
        {
            let w = lock(&world);
            assert_eq!(w.destroyed, 1);
            assert!(!w.visible);
        }
        assert_eq!(
            host.show("pill", &[primary()], None, None).unwrap_err(),
            OverlayError::Unknown("pill".into())
        );
        assert_eq!(
            host.dispose("pill").unwrap_err(),
            OverlayError::Unknown("pill".into())
        );
        // A timer that fires after dispose finds nothing.
        host.fire_auto_hide("pill", 1);
        // dispose_all empties the registry and destroys each window.
        let (w2, t2, s2) = fake();
        host.register(spec(), s2).unwrap();
        host.dispose_all();
        assert!(host.is_empty());
        assert_eq!(lock(&w2).destroyed, 1);
        assert_eq!(Arc::strong_count(&t2), 1);
    }

    fn candidate(class: &str, rect: WorkArea) -> CoveringCandidate {
        CoveringCandidate {
            class_name: class.to_owned(),
            visible: true,
            own_process: false,
            cloaked: false,
            rect,
        }
    }

    #[test]
    fn covering_reads_the_z_order_through_the_surface() {
        let (world, _token, surface) = fake();
        let host = OverlayHost::new();
        host.register(spec(), surface).unwrap();
        // Not shown: nothing to cover.
        assert_eq!(host.covering("pill").unwrap(), None);
        host.show("pill", &[primary()], None, None).unwrap();
        let rect = host.state("pill").unwrap().rect.unwrap();
        let over = area(
            f64::from(rect.x) - 5.0,
            f64::from(rect.y) - 5.0,
            f64::from(rect.x + rect.width) + 5.0,
            f64::from(rect.y + rect.height) + 5.0,
        );
        {
            let mut w = lock(&world);
            w.candidates = vec![
                candidate("Elsewhere", area(1000.0, 500.0, 1100.0, 600.0)),
                CoveringCandidate {
                    own_process: true,
                    ..candidate("OwnWindow", over)
                },
                CoveringCandidate {
                    cloaked: true,
                    ..candidate("Ghost", over)
                },
                CoveringCandidate {
                    visible: false,
                    ..candidate("Hidden", over)
                },
                candidate("GameWindow", over),
                candidate("Later", over),
            ];
        }
        assert_eq!(host.covering("pill").unwrap(), Some("GameWindow".into()));
        // Nothing foreign above it: not covered.
        lock(&world).candidates.clear();
        assert_eq!(host.covering("pill").unwrap(), None);
        // It left the topmost band: reported before anything else.
        lock(&world).topmost = false;
        assert_eq!(host.covering("pill").unwrap(), Some("not_topmost".into()));
        // A hidden overlay is not covered by anything.
        host.hide("pill").unwrap();
        assert_eq!(host.covering("pill").unwrap(), None);
    }

    #[test]
    fn an_unknown_id_is_an_error_and_names_the_id() {
        let host: OverlayHost<FakeSurface> = OverlayHost::new();
        assert_eq!(
            host.hide("nope").unwrap_err(),
            OverlayError::Unknown("nope".into())
        );
        assert!(OverlayError::Unknown("nope".into())
            .to_string()
            .contains("nope"));
        assert!(host.state("nope").is_none());
    }

    #[test]
    fn the_timer_thread_fires_in_order_cancels_and_exits_when_idle() {
        let fired: Arc<Mutex<Vec<(String, u64)>>> = Arc::default();
        let sink = Arc::clone(&fired);
        let timers = Timers::new(Arc::new(move |id, generation| {
            lock(&sink).push((id.to_owned(), generation));
        }));
        timers.schedule(
            "b",
            AutoHide {
                generation: 2,
                after: Duration::from_millis(120),
            },
        );
        timers.schedule(
            "a",
            AutoHide {
                generation: 1,
                after: Duration::from_millis(40),
            },
        );
        timers.schedule(
            "c",
            AutoHide {
                generation: 3,
                after: Duration::from_millis(60),
            },
        );
        timers.cancel("c");
        // Re-arming an id replaces its earlier deadline.
        timers.schedule(
            "b",
            AutoHide {
                generation: 4,
                after: Duration::from_millis(100),
            },
        );
        assert_eq!(timers.pending(), 2);
        let deadline = Instant::now() + Duration::from_secs(5);
        while timers.pending() > 0 || lock(&timers.state).running {
            assert!(Instant::now() < deadline, "timers did not finish");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            *lock(&fired),
            vec![("a".to_owned(), 1), ("b".to_owned(), 4)]
        );
        // The thread has ended; a new schedule starts one again.
        timers.schedule(
            "d",
            AutoHide {
                generation: 5,
                after: Duration::from_millis(10),
            },
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while lock(&fired).len() < 3 {
            assert!(Instant::now() < deadline, "second round did not fire");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
