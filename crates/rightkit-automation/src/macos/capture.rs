//! Per-window PNG capture. Tries `CGWindowListCreateImage` (resolved at runtime,
//! because it is obsoleted in recent SDKs) written through ImageIO; falls back to
//! `screencapture -l`, which uses ScreenCaptureKit on current macOS.

use super::ffi::*;
use crate::model::*;
use std::path::Path;

type CreateImage = unsafe extern "C" fn(CGRect, u32, u32, u32) -> CFTypeRef;

/// Write `img` (a retained `CGImage`) as a PNG at `path`; the image size on success.
fn write_png(img: &Cf, path: &Path) -> Option<(usize, usize)> {
    unsafe {
        let (w, h) = (CGImageGetWidth(img.0), CGImageGetHeight(img.0));
        if w == 0 || h == 0 {
            return None;
        }
        let p = path.to_str()?;
        let url = Cf::new(CFURLCreateFromFileSystemRepresentation(
            std::ptr::null(),
            p.as_ptr(),
            p.len() as CFIndex,
            0,
        ))?;
        let ty = Cf::string("public.png");
        let dest = Cf::new(CGImageDestinationCreateWithURL(
            url.0,
            ty.0,
            1,
            std::ptr::null(),
        ))?;
        CGImageDestinationAddImage(dest.0, img.0, std::ptr::null());
        (CGImageDestinationFinalize(dest.0) != 0).then_some((w, h))
    }
}

fn via_cg(window_id: u64, path: &Path) -> Option<(usize, usize)> {
    unsafe {
        let sym = dlsym(RTLD_DEFAULT, c"CGWindowListCreateImage".as_ptr());
        if sym.is_null() {
            return None;
        }
        let f: CreateImage = std::mem::transmute(sym);
        // CGRectNull bounds = the window's own bounds; IncludingWindow(8); BoundsIgnoreFraming|BestResolution.
        let null = CGRect {
            origin: CGPoint {
                x: f64::INFINITY,
                y: f64::INFINITY,
            },
            size: CGSize { w: 0.0, h: 0.0 },
        };
        let img = Cf::new(f(null, 8, window_id as u32, 1 | 8))?;
        write_png(&img, path)
    }
}

/// Capture a rectangle of the screen (points, top-left origin, global display coordinates):
/// everything on screen in it, all apps, at the display's best resolution.
pub fn capture_region(r: Rect, path: &Path) -> Result<()> {
    if unsafe { CGPreflightScreenCaptureAccess() } == 0 {
        return Err(Error::PermissionDenied("Screen Recording"));
    }
    let via = unsafe {
        let sym = dlsym(RTLD_DEFAULT, c"CGWindowListCreateImage".as_ptr());
        if sym.is_null() {
            None
        } else {
            let f: CreateImage = std::mem::transmute(sym);
            let rect = CGRect {
                origin: CGPoint { x: r.x, y: r.y },
                size: CGSize { w: r.w, h: r.h },
            };
            // OnScreenOnly(1) below every window id (0 = null window); BestResolution(8).
            Cf::new(f(rect, 1, 0, 8))
        }
    };
    if let Some(img) = via {
        if write_png(&img, path).is_some() && path.exists() {
            return Ok(());
        }
    }
    let ok = std::process::Command::new("/usr/sbin/screencapture")
        .args([
            "-x",
            "-R",
            &format!(
                "{},{},{},{}",
                r.x.round() as i64,
                r.y.round() as i64,
                r.w.round() as i64,
                r.h.round() as i64
            ),
        ])
        .arg(path)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok && path.exists() {
        Ok(())
    } else {
        Err(Error::Unsupported("screen region capture failed"))
    }
}

pub fn capture(window_id: u64, path: &Path) -> Result<()> {
    if unsafe { CGPreflightScreenCaptureAccess() } == 0 {
        return Err(Error::PermissionDenied("Screen Recording"));
    }
    if via_cg(window_id, path).is_some() && path.exists() {
        return Ok(());
    }
    let ok = std::process::Command::new("/usr/sbin/screencapture")
        .args(["-x", "-o", "-l", &window_id.to_string()])
        .arg(path)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok && path.exists() {
        Ok(())
    } else {
        Err(Error::Unsupported(
            "window capture failed (window not capturable)",
        ))
    }
}
