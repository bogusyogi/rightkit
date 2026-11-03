//! Screen-region capture: a rectangle of the whole screen (every app and the desktop), as a PNG.
//! [`crate::Automation::screenshot`]-style per-window capture (with redaction) stays the way to
//! observe one app; this is for a host that lets the *user* pick a region, such as a snipping
//! tool or a "describe what is on my screen" request.
//!
//! * macOS: `CGWindowListCreateImage` over the rectangle (best resolution), `screencapture -R`
//!   when that symbol is missing. Needs the Screen Recording grant (`PermissionDenied` otherwise;
//!   this crate does not raise the OS prompt, the host does).
//! * Windows: `BitBlt` from the screen DC with `CAPTUREBLT`, in physical pixels (call
//!   `WinPlatform::new` first, or make the process per-monitor DPI aware, so coordinates match
//!   UI Automation). Windows set to exclude themselves from capture are left out by the OS.
//!
//! Unlike a window capture, the region is not redacted: a password field or a private message
//! in the rectangle is in the file. The guard rails are therefore on the *call*:
//!
//! * it refuses while the kill switch is set;
//! * it runs the capture-safety probe on the frontmost app ([`crate::probe_capture_safety`]) and
//!   refuses when that finds a secure surface (a focused password field, secure event input, a
//!   window that withheld itself). [`capture_region_strict`] also refuses when the probe could
//!   not decide (`Unknown`), which is the right choice for an unattended caller;
//! * the rectangle is bounded (each edge up to 16384, up to 33,554,432 pixels).
//!
//! Like [`crate::media_keys`] this is a primitive: no allowlist (the screen is not one app), no
//! receipt. A host with an admission hook asks it before calling.

use crate::capture_safety::CaptureSafety;
use crate::model::*;
use std::path::Path;

/// Largest edge of a capture region.
pub const MAX_REGION_EDGE: f64 = 16384.0;
/// Largest capture region area, in pixels (points on macOS).
pub const MAX_REGION_PIXELS: f64 = 33_554_432.0;

/// Capture `region` (screen points on macOS, physical pixels on Windows; top-left origin, global
/// coordinates, so a second display can have negative ones) to a PNG at `path`. Refuses when the
/// safety probe of the frontmost app finds a secure surface; see the module docs.
pub fn capture_region(region: Rect, path: &Path) -> Result<()> {
    run(region, path, false)
}

/// [`capture_region`] that also refuses when the safety probe could not decide.
pub fn capture_region_strict(region: Rect, path: &Path) -> Result<()> {
    run(region, path, true)
}

fn run(region: Rect, path: &Path, strict: bool) -> Result<()> {
    if crate::policy::killed() {
        return Err(Error::Blocked("kill switch engaged".into()));
    }
    let finite = [region.x, region.y, region.w, region.h]
        .iter()
        .all(|v| v.is_finite());
    if !finite || region.w < 1.0 || region.h < 1.0 {
        return Err(Error::Invalid(
            "capture region is empty or not finite".into(),
        ));
    }
    if region.w > MAX_REGION_EDGE
        || region.h > MAX_REGION_EDGE
        || region.w * region.h > MAX_REGION_PIXELS
    {
        return Err(Error::Invalid("capture region is too large".into()));
    }
    let front = crate::foreign::foreground_window();
    if front.is_none() && strict {
        return Err(Error::Blocked(
            "region capture refused: the frontmost window could not be identified".into(),
        ));
    }
    if let Some(front) = front {
        match crate::probe_capture_safety(&Target::app(front.pid)).verdict {
            CaptureSafety::Safe => {}
            CaptureSafety::Unsafe(reasons) => {
                return Err(Error::Blocked(format!(
                    "region capture refused: the frontmost app shows a secure surface ({reasons:?})"
                )));
            }
            CaptureSafety::Unknown(_) if strict => {
                return Err(Error::Blocked(
                    "region capture refused: the frontmost app could not be proven safe".into(),
                ));
            }
            CaptureSafety::Unknown(_) => {}
        }
    }
    let result = capture_os(region, path);
    if result.is_err() {
        // Never leave a half-written file behind a failed capture.
        let _ = std::fs::remove_file(path);
    }
    result
}

#[cfg(target_os = "macos")]
fn capture_os(region: Rect, path: &Path) -> Result<()> {
    crate::macos::capture::capture_region(region, path)
}

#[cfg(windows)]
fn capture_os(region: Rect, path: &Path) -> Result<()> {
    crate::win::region::capture_region(region, path)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn capture_os(_region: Rect, _path: &Path) -> Result<()> {
    Err(Error::Unsupported("screen region capture on this platform"))
}
