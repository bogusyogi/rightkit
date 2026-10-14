//! Off-screen launch for recording scenarios (`launch_offscreen`).
//!
//! The driven window is placed outside the union of ALL displays while staying
//! visible (never hidden, never minimized), so the window server keeps compositing
//! it and a recorder can capture it without anything appearing on a user's screen.
//!
//! Two halves:
//! - In-process (the driven app, embedded path): the app calls
//!   [`start_from_env`] / [`start_for_window`] at startup, before its window is first
//!   ordered, when it was launched with `RIGHTKIT_OFFSCREEN=1`. macOS: class-scoped
//!   `constrainFrameRect:toScreen:` override, `NSProcessInfo` activity held for the
//!   session (App Nap defence), and (cargo feature `offscreen-private-spi`, default
//!   off) the private WKWebView `_setWindowOcclusionDetectionEnabled:NO`. Windows:
//!   `SetWindowPos` to off-monitor coordinates and an EcoQoS (execution-speed
//!   throttling) opt-out.
//! - Controller side: [`launch_offscreen`] spawns the app with that env, waits for
//!   the app's session report, and returns a session with PID and native window
//!   identity. [`OffscreenLaunch::end`] kills the process it started.
//!
//! [`offscreen_qualification`] re-reads the live window and verifies it is off all
//! displays, visible, not miniaturized and (macOS) that the occlusion SPI responded.

use crate::json::esc;
use std::fmt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
#[path = "offscreen_mac.rs"]
mod native;
#[cfg(windows)]
#[path = "offscreen_win.rs"]
mod native;
#[cfg(not(any(target_os = "macos", windows)))]
mod native {
    use super::*;
    pub struct Session;
    pub fn end(_: Session) -> Result<(), OffscreenError> {
        Err(OffscreenError::UnsupportedPlatform)
    }
    pub fn session_qualification(_: &Session) -> Result<QualificationReport, OffscreenError> {
        Err(OffscreenError::UnsupportedPlatform)
    }
    pub fn report(_: &Session) -> SessionReport {
        unreachable!("no session exists on this platform")
    }
}

/// Env flag the controller sets and the app reads.
pub const ENV_OFFSCREEN: &str = "RIGHTKIT_OFFSCREEN";
/// Env path where the app writes its session report (JSON, atomic rename).
pub const ENV_REPORT: &str = "RIGHTKIT_OFFSCREEN_REPORT";
/// Distance past the right edge of the display union.
pub const OFFSCREEN_GAP: f64 = 1000.0;
/// Placement read-back tolerance in points/pixels.
pub const PLACEMENT_TOLERANCE: f64 = 1.0;

// ---------- errors ----------

#[derive(Debug, Clone, PartialEq)]
pub enum OffscreenError {
    UnsupportedPlatform,
    NoDisplays,
    InvalidSize,
    NotMainThread,
    NullWindow,
    AlreadyActive,
    NoActiveSession,
    /// `constrainFrameRect:toScreen:` could not be overridden on the window class.
    ConstrainOverrideUnavailable,
    /// Cargo feature `offscreen-private-spi` is off: the occlusion SPI was not compiled in.
    OcclusionSpiNotCompiled,
    /// The WKWebView does not respond to `_setWindowOcclusionDetectionEnabled:`.
    OcclusionSpiUnavailable,
    /// `NSProcessInfo` activity could not be started.
    ActivityUnavailable,
    /// Windows: EcoQoS opt-out failed (Win32 error code).
    ThrottlingOptOutFailed(u32),
    PlacementNotApplied {
        requested: Rect,
        actual: Rect,
    },
    RestoreFailed {
        requested: Rect,
        actual: Rect,
    },
    WindowHidden,
    WindowMiniaturized,
    WindowOnDisplay {
        frame: Rect,
    },
    InvalidBackingScale,
    WindowGone,
    /// Window reads/writes failed with a platform message.
    Native(String),
    // launcher
    Launch(String),
    ReportTimeout,
    ChildExited(String),
    AppReportedError(String),
    BadReport(String),
    IdentityMismatch {
        child_pid: u32,
        reported_pid: u32,
    },
    WindowNotFound {
        pid: u32,
        window_id: u64,
    },
}

impl fmt::Display for OffscreenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for OffscreenError {}

// ---------- geometry (pure) ----------

/// A rectangle in the platform's global desktop coordinates (AppKit bottom-left on
/// macOS, virtual-screen top-left on Windows). The placement math only uses x extents
/// and the lowest y, so it is axis-convention agnostic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(&self) -> f64 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }
    /// True when the interiors overlap (touching edges do not intersect).
    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }
    fn near(&self, o: &Rect, tol: f64) -> bool {
        (self.x - o.x).abs() <= tol
            && (self.y - o.y).abs() <= tol
            && (self.w - o.w).abs() <= tol
            && (self.h - o.h).abs() <= tol
    }
}

/// Bounding box of all displays.
pub fn display_union(displays: &[Rect]) -> Option<Rect> {
    let first = displays.first()?;
    let (mut x0, mut y0, mut x1, mut y1) = (first.x, first.y, first.right(), first.bottom());
    for d in &displays[1..] {
        x0 = x0.min(d.x);
        y0 = y0.min(d.y);
        x1 = x1.max(d.right());
        y1 = y1.max(d.bottom());
    }
    Some(Rect::new(x0, y0, x1 - x0, y1 - y0))
}

/// Frame for a `size` window placed [`OFFSCREEN_GAP`] right of the union of ALL
/// displays (not just the primary one), aligned to the union's lowest y. The result
/// intersects neither any display nor the union bounding box.
pub fn offscreen_frame(displays: &[Rect], size: (f64, f64)) -> Result<Rect, OffscreenError> {
    if !(size.0.is_finite() && size.1.is_finite() && size.0 > 0.0 && size.1 > 0.0) {
        return Err(OffscreenError::InvalidSize);
    }
    if displays
        .iter()
        .any(|d| !(d.x.is_finite() && d.y.is_finite() && d.w.is_finite() && d.h.is_finite()))
    {
        return Err(OffscreenError::NoDisplays);
    }
    let u = display_union(displays).ok_or(OffscreenError::NoDisplays)?;
    Ok(Rect::new(u.right() + OFFSCREEN_GAP, u.y, size.0, size.1))
}

// ---------- qualification (pure) ----------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpiStatus {
    /// Selector present and invoked.
    Responded,
    NotCompiled,
    Unavailable,
    /// Windows has no occlusion SPI.
    NotApplicable,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Platform {
    Mac,
    Windows,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Mac
        }
    }
}

/// Live window facts the qualification checks run on.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowState {
    pub frame: Rect,
    pub displays: Vec<Rect>,
    pub visible: bool,
    pub miniaturized: bool,
    pub backing_scale: f64,
    pub spi: SpiStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub name: &'static str,
    pub error: Option<OffscreenError>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QualificationReport {
    pub platform: Platform,
    pub checks: Vec<Check>,
}

impl QualificationReport {
    pub fn passed(&self) -> bool {
        self.checks.iter().all(|c| c.error.is_none())
    }
    pub fn failures(&self) -> Vec<&Check> {
        self.checks.iter().filter(|c| c.error.is_some()).collect()
    }
    /// First failure as a typed error.
    pub fn into_result(self) -> Result<Self, OffscreenError> {
        match self.checks.iter().find_map(|c| c.error.clone()) {
            Some(e) => Err(e),
            None => Ok(self),
        }
    }
}

/// Per-OS qualification over a window snapshot.
pub fn qualify(state: &WindowState, platform: Platform) -> QualificationReport {
    let on_display = state.displays.iter().any(|d| d.intersects(&state.frame))
        || display_union(&state.displays).is_some_and(|u| u.intersects(&state.frame));
    let mut checks = vec![
        Check {
            name: "off_all_displays",
            error: if state.displays.is_empty() {
                Some(OffscreenError::NoDisplays)
            } else if on_display {
                Some(OffscreenError::WindowOnDisplay { frame: state.frame })
            } else {
                None
            },
        },
        Check {
            name: "visible",
            error: (!state.visible).then_some(OffscreenError::WindowHidden),
        },
        Check {
            name: "not_miniaturized",
            error: state
                .miniaturized
                .then_some(OffscreenError::WindowMiniaturized),
        },
        Check {
            name: "backing_scale",
            error: (!(state.backing_scale.is_finite() && state.backing_scale > 0.0))
                .then_some(OffscreenError::InvalidBackingScale),
        },
    ];
    if platform == Platform::Mac {
        checks.push(Check {
            name: "occlusion_spi_responded",
            error: match state.spi {
                SpiStatus::Responded => None,
                SpiStatus::NotCompiled => Some(OffscreenError::OcclusionSpiNotCompiled),
                SpiStatus::Unavailable | SpiStatus::NotApplicable => {
                    Some(OffscreenError::OcclusionSpiUnavailable)
                }
            },
        });
    }
    QualificationReport { platform, checks }
}

// ---------- placement / restore over a window abstraction ----------

/// Window operations the placement logic needs. Implemented by the native windows
/// and by fakes in tests.
pub trait WindowOps {
    fn displays(&self) -> Result<Vec<Rect>, OffscreenError>;
    fn frame(&self) -> Result<Rect, OffscreenError>;
    fn backing_scale(&self) -> Result<f64, OffscreenError>;
    fn is_visible(&self) -> Result<bool, OffscreenError>;
    fn is_miniaturized(&self) -> Result<bool, OffscreenError>;
    /// Checks before any move (Windows: refuse a minimized window or one owned by another process).
    fn precheck(&self) -> Result<(), OffscreenError> {
        Ok(())
    }
    fn set_origin(&mut self, x: f64, y: f64) -> Result<(), OffscreenError>;
    /// Make the window visible to the window server without activating the app.
    fn show(&mut self) -> Result<(), OffscreenError>;
    /// Take the window off screen before it is moved back (macOS `orderOut:`, so the
    /// restore never flashes on a display; Windows `ShowWindow(SW_HIDE)`). Default no-op.
    fn hide_for_restore(&mut self) -> Result<(), OffscreenError> {
        Ok(())
    }
}

/// Where the window was and where it is now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub original: Rect,
    pub placed: Rect,
    pub backing_scale: f64,
}

/// Move `w` off all displays, make it visible, and read the frame and scale back.
/// Verifies the read-back frame equals the request; the caller still runs
/// [`qualify`] for visibility and display checks.
pub fn place<W: WindowOps>(w: &mut W) -> Result<Placement, OffscreenError> {
    w.precheck()?;
    let original = w.frame()?;
    let target = offscreen_frame(&w.displays()?, (original.w, original.h))?;
    w.set_origin(target.x, target.y)?;
    w.show()?;
    let placed = w.frame()?;
    if !placed.near(&target, PLACEMENT_TOLERANCE) {
        return Err(OffscreenError::PlacementNotApplied {
            requested: target,
            actual: placed,
        });
    }
    Ok(Placement {
        original,
        placed,
        backing_scale: w.backing_scale()?,
    })
}

/// Hide, move back to the original frame and verify.
pub fn restore<W: WindowOps>(w: &mut W, p: &Placement) -> Result<(), OffscreenError> {
    w.hide_for_restore()?;
    w.set_origin(p.original.x, p.original.y)?;
    let actual = w.frame()?;
    if !actual.near(&p.original, PLACEMENT_TOLERANCE) {
        return Err(OffscreenError::RestoreFailed {
            requested: p.original,
            actual,
        });
    }
    Ok(())
}

/// Snapshot a window for [`qualify`].
pub fn snapshot<W: WindowOps>(w: &W, spi: SpiStatus) -> Result<WindowState, OffscreenError> {
    Ok(WindowState {
        frame: w.frame()?,
        displays: w.displays()?,
        visible: w.is_visible()?,
        miniaturized: w.is_miniaturized()?,
        backing_scale: w.backing_scale()?,
        spi,
    })
}

// ---------- session report ----------

/// Native identity and placement of the driven window.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionReport {
    pub pid: u32,
    /// macOS `NSWindow.windowNumber` (== CGWindowID); Windows HWND.
    pub window_id: u64,
    pub placed: Rect,
    pub original: Rect,
    pub backing_scale: f64,
    pub qualified: bool,
}

fn rect_json(r: &Rect) -> String {
    format!(
        "{{\"x\":{},\"y\":{},\"w\":{},\"h\":{}}}",
        r.x, r.y, r.w, r.h
    )
}

impl SessionReport {
    pub fn to_json(&self) -> String {
        format!(
            "{{\"pid\":{},\"window_id\":{},\"placed\":{},\"original\":{},\"backing_scale\":{},\"qualified\":{}}}",
            self.pid,
            self.window_id,
            rect_json(&self.placed),
            rect_json(&self.original),
            self.backing_scale,
            self.qualified
        )
    }
    pub fn from_json(body: &str) -> Result<Self, OffscreenError> {
        let bad = |k: &str| OffscreenError::BadReport(format!("missing {k}"));
        let num = |scope: &str, k: &str| json_num(scope, k).ok_or_else(|| bad(k));
        let sub = |k: &str| -> Result<Rect, OffscreenError> {
            let i = body.find(&format!("\"{k}\":{{")).ok_or_else(|| bad(k))?;
            let s = &body[i..];
            let s = &s[..s.find('}').ok_or_else(|| bad(k))?];
            Ok(Rect::new(
                num(s, "x")?,
                num(s, "y")?,
                num(s, "w")?,
                num(s, "h")?,
            ))
        };
        Ok(Self {
            pid: num(body, "pid")? as u32,
            window_id: num(body, "window_id")? as u64,
            placed: sub("placed")?,
            original: sub("original")?,
            backing_scale: num(body, "backing_scale")?,
            qualified: body.contains("\"qualified\":true"),
        })
    }
}

fn json_num(body: &str, key: &str) -> Option<f64> {
    let pat = format!("\"{key}\":");
    let rest = &body[body.find(&pat)? + pat.len()..];
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '-' | '.' | 'e' | 'E' | '+')))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

// ---------- in-process active session ----------

struct Active(native::Session);
// SAFETY: the native session holds raw window pointers that are only dereferenced
// on the main thread (enforced by an explicit main-thread check in every entry).
unsafe impl Send for Active {}

static ACTIVE: Mutex<Option<Active>> = Mutex::new(None);

fn lock_active() -> std::sync::MutexGuard<'static, Option<Active>> {
    ACTIVE.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
fn install(session: native::Session) -> Result<SessionReport, OffscreenError> {
    let mut g = lock_active();
    if g.is_some() {
        return Err(OffscreenError::AlreadyActive);
    }
    let report = native::report(&session);
    *g = Some(Active(session));
    Ok(report)
}

#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
fn is_active() -> bool {
    lock_active().is_some()
}

/// True when this process was launched with `RIGHTKIT_OFFSCREEN=1`.
pub fn offscreen_requested() -> bool {
    std::env::var(ENV_OFFSCREEN).is_ok_and(|v| v == "1")
}

/// Report of the active in-process session, if any.
pub fn active_report() -> Option<SessionReport> {
    lock_active().as_ref().map(|a| native::report(&a.0))
}

/// Per-OS qualification of the active in-process session: re-reads the live window.
/// Must run on the main thread.
pub fn offscreen_qualification() -> Result<QualificationReport, OffscreenError> {
    let g = lock_active();
    let a = g.as_ref().ok_or(OffscreenError::NoActiveSession)?;
    native::session_qualification(&a.0)
}

/// End the active session on the main thread: restore the window and release the
/// process activity / throttling opt-out.
pub fn end_active() -> Result<(), OffscreenError> {
    let a = lock_active()
        .take()
        .ok_or(OffscreenError::NoActiveSession)?;
    native::end(a.0)
}

/// Start off-screen mode on a native window. macOS: `ns_window` is the `NSWindow*`
/// and `wk_webview` the `WKWebView*`. Windows: `ns_window` carries the HWND value and
/// `wk_webview` is ignored. Main thread only, before the window is first ordered.
///
/// # Safety
/// Pointers must be live native objects for the duration of the session.
pub unsafe fn start_native(
    ns_window: *mut std::ffi::c_void,
    wk_webview: *mut std::ffi::c_void,
) -> Result<SessionReport, OffscreenError> {
    #[cfg(any(target_os = "macos", windows))]
    {
        if is_active() {
            return Err(OffscreenError::AlreadyActive);
        }
        install(native::start(ns_window, wk_webview)?)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (ns_window, wk_webview);
        Err(OffscreenError::UnsupportedPlatform)
    }
}

/// Write the report (or an `{"error":..}` body) atomically to `ENV_REPORT`, if set.
pub fn write_report(result: &Result<SessionReport, OffscreenError>) {
    let Some(path) = std::env::var_os(ENV_REPORT) else {
        return;
    };
    let body = match result {
        Ok(r) => r.to_json(),
        Err(e) => format!("{{\"error\":\"{}\"}}", esc(&e.to_string())),
    };
    let path = PathBuf::from(path);
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Tauri embedded path: start off-screen mode on `win` (must be created hidden,
/// i.e. `visible: false`, so it has never been ordered). `done` runs on the main
/// thread with the outcome; no blocking, so it is safe to call from `setup`.
#[cfg(feature = "tauri-plugin")]
pub fn start_for_window<R: tauri::Runtime>(
    win: &tauri::WebviewWindow<R>,
    done: impl FnOnce(Result<SessionReport, OffscreenError>) + Send + 'static,
) {
    #[cfg(target_os = "macos")]
    let handle = win.ns_window().map(|p| p as usize);
    #[cfg(windows)]
    let handle = win.hwnd().map(|h| h.0 as usize);
    #[cfg(not(any(target_os = "macos", windows)))]
    let handle: tauri::Result<usize> = Ok(0);
    let handle = match handle {
        Ok(h) => h,
        Err(e) => return done(Err(OffscreenError::Native(e.to_string()))),
    };
    let r = win.with_webview(move |pv| {
        #[cfg(target_os = "macos")]
        let wk = pv.inner();
        #[cfg(not(target_os = "macos"))]
        let wk = {
            let _ = &pv;
            std::ptr::null_mut()
        };
        // SAFETY: handles come from the live window; this closure runs on the main thread.
        done(unsafe { start_native(handle as *mut std::ffi::c_void, wk) });
    });
    if let Err(e) = r {
        // `done` was moved into the closure; surface via the report file only.
        write_report(&Err(OffscreenError::Native(e.to_string())));
    }
}

/// App startup hook: if `RIGHTKIT_OFFSCREEN=1`, start off-screen mode, write the
/// report file for the controller, and call `done`. Returns whether it was requested.
#[cfg(feature = "tauri-plugin")]
pub fn start_from_env<R: tauri::Runtime>(
    win: &tauri::WebviewWindow<R>,
    done: impl FnOnce(&Result<SessionReport, OffscreenError>) + Send + 'static,
) -> bool {
    if !offscreen_requested() {
        return false;
    }
    start_for_window(win, move |r| {
        write_report(&r);
        done(&r);
    });
    true
}

// ---------- controller-side launcher ----------

/// What to launch. `program` is the app's executable (spawned directly so this
/// process owns the PID; no `open`/LaunchServices indirection).
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub report_timeout: Duration,
    /// Directory for the report file; default `std::env::temp_dir()`.
    pub report_dir: Option<PathBuf>,
}

impl LaunchSpec {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: vec![],
            env: vec![],
            report_timeout: Duration::from_secs(30),
            report_dir: None,
        }
    }
}

/// A launched off-screen app. Dropping it kills the process it started.
#[derive(Debug)]
pub struct OffscreenLaunch {
    child: Option<Child>,
    report_path: PathBuf,
    pub report: SessionReport,
}

impl OffscreenLaunch {
    pub fn pid(&self) -> u32 {
        self.report.pid
    }
    /// Kill the launched process and clean up the report file. The window dies with
    /// the process; use [`end_active`] inside the app to restore a surviving window.
    pub fn end(mut self) -> std::io::Result<()> {
        self.shutdown()
    }
    fn shutdown(&mut self) -> std::io::Result<()> {
        let _ = std::fs::remove_file(&self.report_path);
        match self.child.take() {
            Some(mut c) => {
                let k = c.kill();
                let w = c.wait();
                k.and(w.map(|_| ()))
            }
            None => Ok(()),
        }
    }
}

impl Drop for OffscreenLaunch {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn kill(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Spawn the app with off-screen mode requested and wait for its session report.
/// Verifies the reported PID is the spawned child and (macOS) that the reported
/// window id exists and belongs to that PID.
pub fn launch_offscreen(spec: &LaunchSpec) -> Result<OffscreenLaunch, OffscreenError> {
    let dir = spec.report_dir.clone().unwrap_or_else(std::env::temp_dir);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let report_path = dir.join(format!(
        "rightkit-offscreen-{}-{nonce}.json",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&report_path);
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args)
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .env(ENV_OFFSCREEN, "1")
        .env(ENV_REPORT, &report_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| OffscreenError::Launch(e.to_string()))?;
    let start = Instant::now();
    let body = loop {
        if let Ok(b) = std::fs::read_to_string(&report_path) {
            break b;
        }
        if let Ok(Some(st)) = child.try_wait() {
            let _ = std::fs::remove_file(&report_path);
            return Err(OffscreenError::ChildExited(st.to_string()));
        }
        if start.elapsed() > spec.report_timeout {
            kill(&mut child);
            let _ = std::fs::remove_file(&report_path);
            return Err(OffscreenError::ReportTimeout);
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let fail = |child: &mut Child, e: OffscreenError| {
        kill(child);
        let _ = std::fs::remove_file(&report_path);
        Err(e)
    };
    if let Some(i) = body.find("\"error\":\"") {
        let msg = body[i + 9..].trim_end_matches(['}', '"', '\n']).to_string();
        return fail(&mut child, OffscreenError::AppReportedError(msg));
    }
    let report = match SessionReport::from_json(&body) {
        Ok(r) => r,
        Err(e) => return fail(&mut child, e),
    };
    if report.pid != child.id() {
        let e = OffscreenError::IdentityMismatch {
            child_pid: child.id(),
            reported_pid: report.pid,
        };
        return fail(&mut child, e);
    }
    #[cfg(target_os = "macos")]
    if !crate::mac::windows(report.pid as i32)
        .iter()
        .any(|w| u64::from(w.id) == report.window_id)
    {
        let e = OffscreenError::WindowNotFound {
            pid: report.pid,
            window_id: report.window_id,
        };
        return fail(&mut child, e);
    }
    Ok(OffscreenLaunch {
        child: Some(child),
        report_path,
        report,
    })
}

/// [`launch_offscreen`] admitted through an [`crate::admission::EffectGate`] (EFF-001).
/// The gate's error channel is string-typed; use the ungated function for typed errors.
pub fn launch_offscreen_gated(
    gate: &crate::admission::EffectGate,
    spec: &LaunchSpec,
) -> crate::admission::Effect<OffscreenLaunch> {
    use crate::admission::{CancelToken, EffectKind, EffectRequest, ExecError};
    let params = format!(
        "{{\"program\":\"{}\",\"args\":{},\"env\":{}}}",
        esc(&spec.program.to_string_lossy()),
        spec.args.len(),
        spec.env.len()
    );
    let req = EffectRequest::new(
        EffectKind::Process,
        "launch_offscreen",
        spec.program.to_string_lossy().to_string(),
    )
    .with_params(params);
    gate.admit(req, &CancelToken::new(), |_| {
        launch_offscreen(spec).map_err(|e| ExecError::from(e.to_string()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect::new(x, y, w, h)
    }

    fn clear_of_all(displays: &[Rect], f: &Rect) {
        for dd in displays {
            assert!(!dd.intersects(f), "{f:?} intersects {dd:?}");
        }
        assert!(!display_union(displays).unwrap().intersects(f));
    }

    #[test]
    fn single_display() {
        let ds = [d(0.0, 0.0, 1920.0, 1080.0)];
        let f = offscreen_frame(&ds, (800.0, 600.0)).unwrap();
        assert_eq!(f, d(2920.0, 0.0, 800.0, 600.0));
        clear_of_all(&ds, &f);
    }

    #[test]
    fn negative_origins_left_and_below() {
        // secondary display left of and below the primary (AppKit bottom-left coords)
        let ds = [
            d(0.0, 0.0, 1440.0, 900.0),
            d(-1920.0, -300.0, 1920.0, 1080.0),
        ];
        let f = offscreen_frame(&ds, (1000.0, 700.0)).unwrap();
        assert_eq!(f.x, 1440.0 + OFFSCREEN_GAP);
        assert_eq!(f.y, -300.0);
        clear_of_all(&ds, &f);
    }

    #[test]
    fn all_negative_layout_uses_union_right_edge() {
        let ds = [
            d(-3000.0, -500.0, 1000.0, 800.0),
            d(-2000.0, -400.0, 1000.0, 800.0),
        ];
        let f = offscreen_frame(&ds, (400.0, 300.0)).unwrap();
        assert_eq!(f.x, -1000.0 + OFFSCREEN_GAP);
        assert_eq!(f.y, -500.0);
        clear_of_all(&ds, &f);
    }

    #[test]
    fn rightmost_display_is_not_the_primary() {
        // primary at origin, a wide display placed to its right: the mover must clear it.
        let ds = [d(0.0, 0.0, 1000.0, 800.0), d(1000.0, 0.0, 3840.0, 2160.0)];
        let f = offscreen_frame(&ds, (500.0, 500.0)).unwrap();
        assert_eq!(f.x, 4840.0 + OFFSCREEN_GAP);
        clear_of_all(&ds, &f);
    }

    #[test]
    fn stacked_and_gapped_displays() {
        let ds = [
            d(0.0, 0.0, 1920.0, 1080.0),
            d(100.0, 1080.0, 1920.0, 1080.0),
            d(5000.0, -200.0, 800.0, 600.0),
        ];
        let f = offscreen_frame(&ds, (640.0, 480.0)).unwrap();
        assert_eq!(f.x, 5800.0 + OFFSCREEN_GAP);
        clear_of_all(&ds, &f);
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(
            offscreen_frame(&[], (10.0, 10.0)),
            Err(OffscreenError::NoDisplays)
        );
        let ds = [d(0.0, 0.0, 10.0, 10.0)];
        assert_eq!(
            offscreen_frame(&ds, (0.0, 10.0)),
            Err(OffscreenError::InvalidSize)
        );
        assert_eq!(
            offscreen_frame(&ds, (f64::NAN, 10.0)),
            Err(OffscreenError::InvalidSize)
        );
        assert_eq!(
            offscreen_frame(&[d(f64::INFINITY, 0.0, 1.0, 1.0)], (10.0, 10.0)),
            Err(OffscreenError::NoDisplays)
        );
    }

    #[test]
    fn touching_edges_do_not_intersect() {
        assert!(!d(0.0, 0.0, 10.0, 10.0).intersects(&d(10.0, 0.0, 10.0, 10.0)));
        assert!(d(0.0, 0.0, 10.0, 10.0).intersects(&d(9.0, 9.0, 10.0, 10.0)));
    }

    struct Fake {
        displays: Vec<Rect>,
        frame: Rect,
        visible: bool,
        mini: bool,
        scale: f64,
        /// Clamp any requested origin back to this one (window refuses placement).
        clamp_to: Option<(f64, f64)>,
        hidden_calls: u32,
        shown: bool,
        hidden_precheck: bool,
    }
    impl Fake {
        fn new() -> Self {
            Self {
                displays: vec![d(0.0, 0.0, 1920.0, 1080.0), d(-1280.0, 0.0, 1280.0, 800.0)],
                frame: d(100.0, 100.0, 800.0, 600.0),
                visible: false,
                mini: false,
                scale: 2.0,
                clamp_to: None,
                hidden_calls: 0,
                shown: false,
                hidden_precheck: false,
            }
        }
    }
    impl WindowOps for Fake {
        fn displays(&self) -> Result<Vec<Rect>, OffscreenError> {
            Ok(self.displays.clone())
        }
        fn frame(&self) -> Result<Rect, OffscreenError> {
            Ok(self.frame)
        }
        fn backing_scale(&self) -> Result<f64, OffscreenError> {
            Ok(self.scale)
        }
        fn is_visible(&self) -> Result<bool, OffscreenError> {
            Ok(self.visible)
        }
        fn is_miniaturized(&self) -> Result<bool, OffscreenError> {
            Ok(self.mini)
        }
        fn precheck(&self) -> Result<(), OffscreenError> {
            if self.hidden_precheck {
                Err(OffscreenError::WindowHidden)
            } else {
                Ok(())
            }
        }
        fn set_origin(&mut self, x: f64, y: f64) -> Result<(), OffscreenError> {
            let (x, y) = self.clamp_to.unwrap_or((x, y));
            self.frame.x = x;
            self.frame.y = y;
            Ok(())
        }
        fn show(&mut self) -> Result<(), OffscreenError> {
            self.visible = true;
            self.shown = true;
            Ok(())
        }
        fn hide_for_restore(&mut self) -> Result<(), OffscreenError> {
            self.hidden_calls += 1;
            self.visible = false;
            Ok(())
        }
    }

    #[test]
    fn place_then_restore_round_trip() {
        let mut w = Fake::new();
        let p = place(&mut w).unwrap();
        assert_eq!(p.original, d(100.0, 100.0, 800.0, 600.0));
        assert_eq!(p.placed.x, 1920.0 + OFFSCREEN_GAP);
        assert_eq!(p.backing_scale, 2.0);
        assert!(w.shown);
        let st = snapshot(&w, SpiStatus::Responded).unwrap();
        assert!(qualify(&st, Platform::Mac).passed());
        restore(&mut w, &p).unwrap();
        assert_eq!(w.frame, p.original);
        assert_eq!(w.hidden_calls, 1);
        assert!(
            !w.visible,
            "restore hides first so it never flashes on a display"
        );
    }

    #[test]
    fn place_detects_clamped_window() {
        let mut w = Fake::new();
        w.clamp_to = Some((50.0, 50.0));
        match place(&mut w) {
            Err(OffscreenError::PlacementNotApplied { actual, .. }) => assert_eq!(actual.x, 50.0),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn restore_detects_failure() {
        let mut w = Fake::new();
        let p = place(&mut w).unwrap();
        w.clamp_to = Some((0.0, 0.0));
        assert!(matches!(
            restore(&mut w, &p),
            Err(OffscreenError::RestoreFailed { .. })
        ));
    }

    #[test]
    fn precheck_refusal_moves_nothing() {
        let mut w = Fake::new();
        w.hidden_precheck = true;
        assert_eq!(place(&mut w), Err(OffscreenError::WindowHidden));
        assert_eq!(w.frame, d(100.0, 100.0, 800.0, 600.0));
    }

    fn good_state() -> WindowState {
        WindowState {
            frame: d(3000.0, 0.0, 800.0, 600.0),
            displays: vec![d(0.0, 0.0, 1920.0, 1080.0)],
            visible: true,
            miniaturized: false,
            backing_scale: 2.0,
            spi: SpiStatus::Responded,
        }
    }

    #[test]
    fn qualification_passes_and_each_check_fails_typed() {
        assert!(qualify(&good_state(), Platform::Mac).passed());

        let mut s = good_state();
        s.frame = d(1900.0, 0.0, 800.0, 600.0);
        assert!(matches!(
            qualify(&s, Platform::Mac).into_result(),
            Err(OffscreenError::WindowOnDisplay { .. })
        ));

        // in the gap between displays but inside the union bounding box also fails
        let mut s = good_state();
        s.displays = vec![d(0.0, 0.0, 1000.0, 1000.0), d(5000.0, 0.0, 1000.0, 1000.0)];
        s.frame = d(2000.0, 0.0, 800.0, 600.0);
        assert!(matches!(
            qualify(&s, Platform::Mac).into_result(),
            Err(OffscreenError::WindowOnDisplay { .. })
        ));

        let mut s = good_state();
        s.visible = false;
        assert_eq!(
            qualify(&s, Platform::Windows).into_result(),
            Err(OffscreenError::WindowHidden)
        );

        let mut s = good_state();
        s.miniaturized = true;
        assert_eq!(
            qualify(&s, Platform::Windows).into_result(),
            Err(OffscreenError::WindowMiniaturized)
        );

        let mut s = good_state();
        s.backing_scale = 0.0;
        assert_eq!(
            qualify(&s, Platform::Mac).into_result(),
            Err(OffscreenError::InvalidBackingScale)
        );

        let mut s = good_state();
        s.displays.clear();
        assert_eq!(
            qualify(&s, Platform::Mac).into_result(),
            Err(OffscreenError::NoDisplays)
        );
    }

    #[test]
    fn qualification_spi_is_macos_only() {
        let mut s = good_state();
        for (spi, err) in [
            (
                SpiStatus::NotCompiled,
                OffscreenError::OcclusionSpiNotCompiled,
            ),
            (
                SpiStatus::Unavailable,
                OffscreenError::OcclusionSpiUnavailable,
            ),
        ] {
            s.spi = spi;
            assert_eq!(qualify(&s, Platform::Mac).into_result(), Err(err));
            assert!(qualify(&s, Platform::Windows).passed());
        }
        s.spi = SpiStatus::NotApplicable;
        assert!(qualify(&s, Platform::Windows).passed());
        assert!(!qualify(&s, Platform::Mac).passed());
    }

    #[test]
    fn report_json_round_trip() {
        let r = SessionReport {
            pid: 4242,
            window_id: 987,
            placed: d(3920.0, -100.5, 800.0, 600.0),
            original: d(-100.0, 50.0, 800.0, 600.0),
            backing_scale: 2.0,
            qualified: true,
        };
        assert_eq!(SessionReport::from_json(&r.to_json()).unwrap(), r);
        assert!(matches!(
            SessionReport::from_json("{}"),
            Err(OffscreenError::BadReport(_))
        ));
    }

    #[test]
    fn no_active_session_is_typed() {
        // Not installed by any unit test.
        assert_eq!(
            offscreen_qualification().unwrap_err(),
            OffscreenError::NoActiveSession
        );
        assert_eq!(end_active().unwrap_err(), OffscreenError::NoActiveSession);
    }

    #[test]
    fn launch_reports_child_exit_and_missing_binary() {
        let mut spec = LaunchSpec::new("/usr/bin/true");
        spec.report_timeout = Duration::from_secs(5);
        #[cfg(unix)]
        assert!(matches!(
            launch_offscreen(&spec),
            Err(OffscreenError::ChildExited(_))
        ));
        let spec = LaunchSpec::new("/nonexistent/rightkit-offscreen-app");
        assert!(matches!(
            launch_offscreen(&spec),
            Err(OffscreenError::Launch(_))
        ));
    }
}
