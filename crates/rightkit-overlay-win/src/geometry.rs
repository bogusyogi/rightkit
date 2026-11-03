//! Pure geometry: DPI scaling, frame placement, clipping rectangles and hit-region lookup.
//! Nothing here touches Win32, so every rule unit-tests on any host.
//!
//! Coordinate model (one convention everywhere):
//! * A **frame** is `Rect { x, y, width, height }`. `x` and `y` are the window top-left in
//!   **physical virtual-screen pixels** (the space of `GetCursorPos`, `MonitorPlacement` and
//!   `WorkArea`). `width` and `height` are **logical pixels**: 1.0 is one pixel at 100% scaling,
//!   multiplied by the DPI scale of the monitor the window sits on.
//! * Everything drawn or hit-tested inside the window (`PaintCtx`, `HitRegion`, event
//!   coordinates) is in logical pixels relative to the window top-left.

use rightkit_overlay::{MonitorPlacement, Rect, WorkArea};

/// Largest window side in device pixels (the DIB and canvas limit).
pub const MAX_SURFACE_SIDE: i32 = 8192;

/// A size in logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

/// The factor from logical to device pixels for an effective DPI (96 is 1.0). A DPI of zero reads
/// as 96.
pub fn scale_for_dpi(dpi: u32) -> f32 {
    if dpi == 0 {
        1.0
    } else {
        dpi as f32 / 96.0
    }
}

/// A window rectangle in device pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Where a frame landed: its device rectangle, the effective DPI there and which monitor it is on
/// (the monitor bounds, as an identity for change detection).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub device: DeviceRect,
    pub dpi: u32,
    pub monitor: Option<[i32; 4]>,
}

/// Logical length to device pixels, at least 1 and at most `MAX_SURFACE_SIDE`.
pub fn device_size(logical: f64, scale: f64) -> i32 {
    ((logical * scale).round() as i64).clamp(1, i64::from(MAX_SURFACE_SIDE)) as i32
}

fn to_screen_coord(value: f64) -> i32 {
    value.round().clamp(-1_000_000.0, 1_000_000.0) as i32
}

fn monitor_key(bounds: WorkArea) -> [i32; 4] {
    [
        bounds.left.round() as i32,
        bounds.top.round() as i32,
        bounds.right.round() as i32,
        bounds.bottom.round() as i32,
    ]
}

/// Resolves a frame to device pixels on the monitor `monitor_at` finds for it.
///
/// The monitor is the one under the frame centre (sized at the DPI of the monitor under its
/// top-left corner first, so a frame straddling two monitors picks one deterministically). With
/// `clamp`, the rectangle is held inside that monitor work area by
/// [`rightkit_overlay::WorkArea::clamp`], the suite placement policy. A non-finite or negative
/// frame resolves to `None`.
pub fn place_frame<F>(frame: Rect, clamp: bool, monitor_at: F) -> Option<Placement>
where
    F: Fn(i32, i32) -> Option<MonitorPlacement>,
{
    if !frame.finite() {
        return None;
    }
    let (x, y) = (to_screen_coord(frame.x), to_screen_coord(frame.y));
    let first = monitor_at(x, y);
    let first_scale = first.map_or(1.0, |m| m.scale());
    let (w0, h0) = (
        device_size(frame.width, first_scale),
        device_size(frame.height, first_scale),
    );
    let monitor = monitor_at(x.saturating_add(w0 / 2), y.saturating_add(h0 / 2)).or(first);
    let scale = monitor.map_or(1.0, |m| m.scale());
    let mut device = DeviceRect {
        x,
        y,
        width: device_size(frame.width, scale),
        height: device_size(frame.height, scale),
    };
    if clamp {
        if let Some(m) = monitor {
            device = clamp_to_work_area(device, m.work);
        }
    }
    Some(Placement {
        device,
        dpi: monitor.map_or(96, |m| m.dpi.max(1)),
        monitor: monitor.map(|m| monitor_key(m.bounds)),
    })
}

/// Holds `rect` inside `work` with the suite clamp (size is never changed).
pub fn clamp_to_work_area(rect: DeviceRect, work: WorkArea) -> DeviceRect {
    let clamped = work.clamp(Rect {
        x: f64::from(rect.x),
        y: f64::from(rect.y),
        width: f64::from(rect.width),
        height: f64::from(rect.height),
    });
    DeviceRect {
        x: clamped.x.round() as i32,
        y: clamped.y.round() as i32,
        ..rect
    }
}

/// What to do with the pixel memory behind a window whose device size just became `need`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Backing {
    /// The current backing is big enough and not wastefully big: keep it.
    Keep,
    /// Allocate a new backing of this size (it includes growth headroom).
    Rebuild { width: usize, height: usize },
}

/// A backing is not given back until it is larger than this many times the need (and by more than
/// [`BACKING_SLACK`] pixels), so a size that wobbles up and down never reallocates.
const BACKING_SHRINK_FACTOR: usize = 2;
/// Absolute slack before a small backing counts as wastefully big.
const BACKING_SLACK: usize = 64;

fn backing_axis(current: usize, need: usize) -> usize {
    let side = MAX_SURFACE_SIDE as usize;
    let need = need.clamp(1, side);
    let ceiling = need
        .saturating_mul(BACKING_SHRINK_FACTOR)
        .max(need + BACKING_SLACK);
    if current >= need && current <= ceiling {
        return current;
    }
    // A quarter of headroom: a morph that grows a little each frame then reallocates a handful
    // of times, not once per frame.
    (need + need / 4).min(side)
}

/// Decides whether the DIB and canvas behind a window must be rebuilt for a window of `need`
/// device pixels (`width`, `height`). Grow-only with hysteresis: a size change that still fits the
/// current backing (and does not leave it more than twice too large) keeps it, so animating the
/// window size costs no allocation and keeps every cache. `current` is the backing's size, if any.
pub(crate) fn plan_backing(current: Option<(usize, usize)>, need: (usize, usize)) -> Backing {
    let (cw, ch) = current.unwrap_or((0, 0));
    let width = backing_axis(cw, need.0);
    let height = backing_axis(ch, need.1);
    if current.is_some() && (width, height) == (cw, ch) {
        Backing::Keep
    } else {
        Backing::Rebuild { width, height }
    }
}

/// Copies `height` rows of `width` pixels from the tightly packed `src` into `dst`, whose rows are
/// `dst_stride` pixels apart (a backing wider than the window). Returns false and copies nothing
/// when either buffer is too small for the request.
pub(crate) fn copy_rows(
    dst: &mut [u32],
    dst_stride: usize,
    src: &[u32],
    width: usize,
    height: usize,
) -> bool {
    let Some(src_len) = width.checked_mul(height) else {
        return false;
    };
    if width == 0 || height == 0 {
        return true;
    }
    // The last row needs no trailing padding.
    let Some(dst_len) = dst_stride
        .checked_mul(height - 1)
        .and_then(|rows| rows.checked_add(width))
    else {
        return false;
    };
    if dst_stride < width || src.len() < src_len || dst.len() < dst_len {
        return false;
    }
    if dst_stride == width {
        dst[..src_len].copy_from_slice(&src[..src_len]);
    } else {
        for row in 0..height {
            let at = row * dst_stride;
            dst[at..at + width].copy_from_slice(&src[row * width..(row + 1) * width]);
        }
    }
    true
}

/// An interactive area inside the window, in logical pixels relative to its top-left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HitRegion {
    pub id: u32,
    pub rect: Rect,
}

/// The id of the region under a logical point. Edges are **inclusive** on all four sides, the
/// same rule as `rightkit_overlay::Rect::contains`, so a region claims its right and bottom edge
/// and a point is never lost between two touching regions; where regions overlap (or touch) the
/// one listed last wins, as a later layer paints over an earlier one. Non-finite points and
/// regions never match.
pub fn hit_test(regions: &[HitRegion], x: f64, y: f64) -> Option<u32> {
    regions
        .iter()
        .rev()
        .find(|region| region.rect.contains(x, y))
        .map(|region| region.id)
}

/// Device-pixel rectangles `[left, top, right, bottom]` of the regions, rounded outwards so a
/// region never loses a partly covered pixel; degenerate and non-finite regions are skipped.
pub fn device_regions(regions: &[HitRegion], scale: f64) -> Vec<[i32; 4]> {
    regions
        .iter()
        .filter(|region| {
            region.rect.finite() && region.rect.width > 0.0 && region.rect.height > 0.0
        })
        .map(|region| {
            let r = region.rect;
            let edge = |v: f64, up: bool| {
                let scaled = (v * scale).clamp(-1.0e6, 1.0e6);
                (if up { scaled.ceil() } else { scaled.floor() }) as i32
            };
            [
                edge(r.x, false),
                edge(r.y, false),
                edge(r.x + r.width, true),
                edge(r.y + r.height, true),
            ]
        })
        .collect()
}

/// A device-pixel client coordinate to logical pixels.
pub fn client_to_logical(px: i32, scale: f32) -> f64 {
    f64::from(px) / f64::from(scale.max(f32::EPSILON))
}
