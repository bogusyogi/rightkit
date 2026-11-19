//! macOS extraction: `NSWorkspace iconForFile` on the bundle path, rendered into a bitmap this
//! crate sizes itself.
//!
//! The crate never asks AppKit for a TIFF or a PNG of an arbitrary image. The order is:
//!
//! 1. `limits::source_acceptable`: the path exists and is a directory (a bundle) or a regular file of
//!    at most `MAX_MAC_SOURCE_FILE_BYTES` (a file path can be an image whose thumbnail AppKit would
//!    decode).
//! 2. `NSWorkspace iconForFile`: returns an `NSImage`. This is the one call whose own allocation the
//!    crate cannot bound (AppKit / IconServices load the bundle's icon resource; nothing is
//!    rendered or encoded by us yet). It is the same call the original used.
//! 3. `NSImage.representations`, `NSImageRep.pixelsWide / pixelsHigh`: property reads only. No
//!    rendering, no encoding, no pixel buffer. Each rep that states its size is checked with
//!    `limits::check_dimensions` (`max_edge`, pixel cap); the largest that fits is chosen. If reps
//!    state sizes but none fits, the icon is "no icon" (reported); if none states a size (a
//!    scalable rep) the render side is `limits::render_side(max_edge)`.
//! 4. `NSBitmapImageRep initWithBitmapDataPlanes:...`: the only pixel allocation, exactly
//!    `width * 4 * height` bytes (`bytesPerRow` is passed explicitly), with width and height
//!    already checked, so at most `MAX_PIXELS * 4` = 1 MiB.
//! 5. `NSGraphicsContext` + `NSImageRep.drawInRect` (the chosen rep only) or `NSImage
//!    drawInRect:fromRect:operation:fraction:` (scalable case): draws into that buffer and nowhere
//!    else. Temporary work inside AppKit for the drawn rep is AppKit's.
//! 6. The pixels are copied out (length re-checked against `rgba_len`), un-premultiplied, and
//!    encoded by this crate's own encoder (`png::encode_png_rgba`). No AppKit-produced PNG or TIFF
//!    exists anywhere in the path. `lib.rs` validates the result again before it is cached.

use objc2::AnyThread;
use objc2_app_kit::{
    NSBitmapFormat, NSBitmapImageRep, NSCompositingOperation, NSDeviceRGBColorSpace,
    NSGraphicsContext, NSImageRep, NSWorkspace,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use crate::limits::{self, Reject, Sink};

pub(crate) fn extract(
    config: &crate::IconConfig,
    process_name: &str,
    exe_path: Option<&str>,
) -> Option<Vec<u8>> {
    let path = match exe_path {
        Some(path) => path.to_string(),
        None => resolve_running_bundle_path(process_name)?,
    };
    if !limits::input_str_ok(&path) {
        limits::report(config.event_sink, "exe_path", Reject::Oversize);
        return None;
    }
    if !limits::source_acceptable(
        std::path::Path::new(&path),
        limits::MAX_MAC_SOURCE_FILE_BYTES,
    ) {
        return None;
    }
    icon_png_for_path(&path, config.edge_limit(), config.event_sink)
}

/// Match a running app by localized name or bundle identifier (at most
/// `limits::MAX_RUNNING_APPS` applications are examined).
fn resolve_running_bundle_path(process_name: &str) -> Option<String> {
    let wanted = process_name.trim().to_lowercase();
    if wanted.is_empty() {
        return None;
    }
    let apps = NSWorkspace::sharedWorkspace().runningApplications();
    for app in apps.iter().take(limits::MAX_RUNNING_APPS) {
        let name = app.localizedName().map(|s| s.to_string().to_lowercase());
        let bundle = app.bundleIdentifier().map(|s| s.to_string().to_lowercase());
        if name.as_deref() == Some(wanted.as_str()) || bundle.as_deref() == Some(wanted.as_str()) {
            if let Some(url) = app.bundleURL() {
                if let Some(path) = url.path() {
                    return Some(path.to_string());
                }
            }
        }
    }
    None
}

#[allow(unused_unsafe)]
fn icon_png_for_path(path: &str, max_edge: u32, sink: Sink) -> Option<Vec<u8>> {
    let workspace = NSWorkspace::sharedWorkspace();
    let image = workspace.iconForFile(&NSString::from_str(path));

    // Choose the representation from its stated pixel size (property reads, nothing rendered).
    let reps = image.representations();
    let mut best: Option<(u64, u32, u32, objc2::rc::Retained<NSImageRep>)> = None;
    let mut stated_any = false;
    let mut rejected: Option<Reject> = None;
    for rep in reps.iter().take(limits::MAX_REPS) {
        let (wide, high) = (rep.pixelsWide(), rep.pixelsHigh());
        if wide <= 0 && high <= 0 {
            continue; // size not stated (a scalable rep)
        }
        stated_any = true;
        match limits::check_dimensions(wide as i64, high as i64, max_edge) {
            Ok((width, height)) => {
                let pixels = u64::from(width) * u64::from(height);
                if best.as_ref().is_none_or(|(p, ..)| pixels > *p) {
                    best = Some((pixels, width, height, rep));
                }
            }
            Err(reject) => rejected = Some(reject),
        }
    }
    let (width, height, chosen) = match best {
        Some((_, width, height, rep)) => (width, height, Some(rep)),
        None if stated_any => {
            if let Some(reject) = rejected {
                limits::report(sink, "mac_icon", reject);
            }
            return None;
        }
        None => {
            let side = limits::render_side(max_edge);
            (side, side, None)
        }
    };
    // Last gate before the only pixel allocation.
    let (width, height) =
        limits::check_dimensions(i64::from(width), i64::from(height), max_edge).ok()?;
    let len = limits::rgba_len(width, height)?;
    let stride = isize::try_from(width.checked_mul(4)?).ok()?;

    // The bitmap this crate sizes: width * 4 bytes a row, `height` rows, 8-bit RGBA.
    let bitmap = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            width as isize,
            height as isize,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            NSBitmapFormat(0),
            stride,
            32,
        )
    }?;
    if bitmap.pixelsWide() != width as isize
        || bitmap.pixelsHigh() != height as isize
        || bitmap.bytesPerRow() != stride
    {
        return None;
    }

    // Draw into it (and only it).
    let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&bitmap)?;
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&context));
    let target = NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(f64::from(width), f64::from(height)),
    );
    // `NSImageRep.drawInRect` reports whether it drew. `NSImage`'s draw returns nothing.
    let drawn = match &chosen {
        Some(rep) => rep.drawInRect(target),
        None => {
            image.drawInRect_fromRect_operation_fraction(
                target,
                NSRect::ZERO,
                NSCompositingOperation::SourceOver,
                1.0,
            );
            true
        }
    };
    context.flushGraphics();
    // Restored on every path: nothing between save and here returns early.
    NSGraphicsContext::restoreGraphicsState_class();
    if !drawn {
        // A failed draw leaves the bitmap blank: never encode or cache it.
        return None;
    }

    // Copy exactly `len` bytes out of the buffer this crate sized.
    let data = bitmap.bitmapData();
    if data.is_null() {
        return None;
    }
    let mut rgba = unsafe { std::slice::from_raw_parts(data, len) }.to_vec();
    if !bitmap
        .bitmapFormat()
        .contains(NSBitmapFormat::AlphaNonpremultiplied)
    {
        crate::png::unpremultiply_rgba(&mut rgba);
    }
    crate::png::encode_png_rgba(width, height, &rgba)
}
