//! The user's own windows, outside the automation allowlist: find the window they are working
//! in and move or resize it. This is what a window-snapping or layout tool needs; the engine
//! ([`crate::Automation`]) only acts on windows of apps it was allowed to drive.
//!
//! * [`foreground_window`]: the OS foreground window as a [`WindowInfo`] (macOS: the frontmost
//!   app's focused window).
//! * [`place_window`]: move and resize one window. macOS writes `AXPosition` and `AXSize`
//!   (needs the Accessibility grant, `PermissionDenied` otherwise); Windows calls `SetWindowPos`
//!   with no activation or z-order change, restoring a maximized window first.
//!
//! Boundary: a window of an app on the always-denied list ([`crate::policy::always_denied`]:
//! the OS privacy and credential prompts, other computer-use hosts) is never moved. Only normal
//! application windows are (layer 0), the rectangle is bounded, and the call refuses while the
//! kill switch is set. Like [`crate::media_keys`] it writes no receipt and asks no allowlist: a
//! host with an admission hook asks it before calling. The app may clamp the result; list the
//! window again to read back where it ended up.

use crate::model::*;

/// Smallest edge a placed window may be given.
pub const MIN_WINDOW_EDGE: f64 = 40.0;
/// Largest edge a placed window may be given.
pub const MAX_WINDOW_EDGE: f64 = 32768.0;

/// The window the user is working in, if the OS reports one.
pub fn foreground_window() -> Option<WindowInfo> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::foreground_window()
    }
    #[cfg(windows)]
    {
        crate::win::foreground_window()
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        None
    }
}

#[cfg(any(target_os = "macos", windows))]
fn all_windows() -> Vec<WindowInfo> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::all_windows()
    }
    #[cfg(windows)]
    {
        crate::win::all_windows()
    }
}

#[cfg(any(target_os = "macos", windows))]
fn app_id_of(pid: i32) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::app_id_of(pid)
    }
    #[cfg(windows)]
    {
        crate::win::app_id_of(pid)
    }
}

/// Move and resize the window `window_id` (a [`WindowInfo::window_id`]) to `frame`: top-left
/// origin, global coordinates (points on macOS, physical pixels on Windows).
pub fn place_window(window_id: u64, frame: Rect) -> Result<()> {
    if crate::policy::killed() {
        return Err(Error::Blocked("kill switch engaged".into()));
    }
    let finite = [frame.x, frame.y, frame.w, frame.h]
        .iter()
        .all(|v| v.is_finite());
    if !finite
        || !(MIN_WINDOW_EDGE..=MAX_WINDOW_EDGE).contains(&frame.w)
        || !(MIN_WINDOW_EDGE..=MAX_WINDOW_EDGE).contains(&frame.h)
    {
        return Err(Error::Invalid(format!(
            "window size must be between {MIN_WINDOW_EDGE} and {MAX_WINDOW_EDGE} on each edge"
        )));
    }
    place_os(window_id, frame)
}

#[cfg(any(target_os = "macos", windows))]
fn place_os(window_id: u64, frame: Rect) -> Result<()> {
    let w = all_windows()
        .into_iter()
        .find(|w| w.window_id == window_id)
        .ok_or_else(|| Error::NotFound(format!("window {window_id}")))?;
    if w.layer != 0 {
        return Err(Error::Blocked(
            "only normal application windows can be placed".into(),
        ));
    }
    // Fail closed: a window whose app cannot be identified is not moved.
    let app_id = app_id_of(w.pid)
        .ok_or_else(|| Error::Blocked("the window's app could not be identified".into()))?;
    if crate::policy::always_denied(&app_id) {
        return Err(Error::Blocked(format!("{app_id} is never drivable")));
    }
    #[cfg(target_os = "macos")]
    {
        crate::macos::place_window(window_id, frame)
    }
    #[cfg(windows)]
    {
        crate::win::place_window(window_id, frame)
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
fn place_os(_window_id: u64, _frame: Rect) -> Result<()> {
    Err(Error::Unsupported("window placement on this platform"))
}
