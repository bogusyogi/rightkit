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

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(left: f64, top: f64, right: f64, bottom: f64, dpi: u32) -> MonitorPlacement {
        MonitorPlacement {
            bounds: WorkArea {
                left,
                top,
                right,
                bottom,
            },
            work: WorkArea {
                left,
                top,
                right,
                bottom: bottom - 40.0,
            },
            dpi,
        }
    }

    /// Two monitors side by side: 100% on the left, 150% on the right.
    fn two_monitors(x: i32, _y: i32) -> Option<MonitorPlacement> {
        if x < 1920 {
            Some(monitor(0.0, 0.0, 1920.0, 1080.0, 96))
        } else {
            Some(monitor(1920.0, 0.0, 3840.0, 1080.0, 144))
        }
    }

    #[test]
    fn dpi_scale() {
        assert_eq!(scale_for_dpi(96), 1.0);
        assert_eq!(scale_for_dpi(144), 1.5);
        assert_eq!(scale_for_dpi(192), 2.0);
        assert_eq!(scale_for_dpi(0), 1.0);
    }

    #[test]
    fn device_size_rounds_and_is_bounded() {
        assert_eq!(device_size(100.0, 1.5), 150);
        assert_eq!(device_size(33.0, 1.25), 41);
        assert_eq!(device_size(0.0, 1.0), 1);
        assert_eq!(device_size(1.0e9, 2.0), MAX_SURFACE_SIDE);
    }

    #[test]
    fn size_scales_with_the_monitor_under_the_frame() {
        let frame = Rect {
            x: 100.0,
            y: 100.0,
            width: 200.0,
            height: 80.0,
        };
        let left = place_frame(frame, false, two_monitors).unwrap();
        assert_eq!(
            (left.device.width, left.device.height, left.dpi),
            (200, 80, 96)
        );
        let frame = Rect { x: 2000.0, ..frame };
        let right = place_frame(frame, false, two_monitors).unwrap();
        assert_eq!(
            (right.device.width, right.device.height, right.dpi),
            (300, 120, 144)
        );
        assert_ne!(left.monitor, right.monitor);
        // Position is physical and passes through unscaled.
        assert_eq!(right.device.x, 2000);
    }

    #[test]
    fn straddling_frame_uses_the_monitor_under_its_centre() {
        // Top-left on the 100% monitor, but the centre of the frame is on the 150% one.
        let frame = Rect {
            x: 1900.0,
            y: 100.0,
            width: 400.0,
            height: 80.0,
        };
        let placed = place_frame(frame, false, two_monitors).unwrap();
        assert_eq!(placed.dpi, 144);
        assert_eq!(placed.device.width, 600);
    }

    #[test]
    fn clamp_holds_the_frame_in_the_work_area() {
        let frame = Rect {
            x: 100.0,
            y: 1070.0,
            width: 100.0,
            height: 50.0,
        };
        let free = place_frame(frame, false, two_monitors).unwrap();
        assert_eq!(free.device.y, 1070);
        let held = place_frame(frame, true, two_monitors).unwrap();
        // Work area bottom is 1040; a 50 px tall frame ends there.
        assert_eq!(held.device.y, 990);
        assert_eq!(held.device.x, 100);
        assert_eq!(held.device.height, 50);
    }

    #[test]
    fn unknown_monitor_falls_back_to_96_dpi() {
        let frame = Rect {
            x: 10.0,
            y: 10.0,
            width: 20.0,
            height: 20.0,
        };
        let placed = place_frame(frame, true, |_, _| None).unwrap();
        assert_eq!(placed.dpi, 96);
        assert_eq!(placed.monitor, None);
        assert_eq!(placed.device.width, 20);
    }

    #[test]
    fn invalid_frames_do_not_place() {
        let nan = Rect {
            x: f64::NAN,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        };
        assert!(place_frame(nan, false, two_monitors).is_none());
        let negative = Rect {
            x: 0.0,
            y: 0.0,
            width: -1.0,
            height: 1.0,
        };
        assert!(place_frame(negative, false, two_monitors).is_none());
    }

    fn region(id: u32, x: f64, y: f64, w: f64, h: f64) -> HitRegion {
        HitRegion {
            id,
            rect: Rect {
                x,
                y,
                width: w,
                height: h,
            },
        }
    }

    #[test]
    fn hit_lookup_is_edge_inclusive_and_last_wins() {
        let regions = [
            region(1, 0.0, 0.0, 100.0, 40.0),
            region(2, 80.0, 0.0, 20.0, 40.0),
            region(3, 100.0, 0.0, 50.0, 40.0),
        ];
        assert_eq!(hit_test(&regions, 10.0, 10.0), Some(1));
        // Overlap: the later region wins.
        assert_eq!(hit_test(&regions, 90.0, 10.0), Some(2));
        // Edges are inclusive; where regions touch the later one wins.
        assert_eq!(hit_test(&regions, 100.0, 10.0), Some(3));
        assert_eq!(hit_test(&regions, 150.0, 10.0), Some(3));
        assert_eq!(hit_test(&regions, 10.0, 40.0), Some(1));
        assert_eq!(hit_test(&regions, 0.0, 0.0), Some(1));
        assert_eq!(hit_test(&regions, 150.1, 10.0), None);
        assert_eq!(hit_test(&regions, 10.0, 40.1), None);
        assert_eq!(hit_test(&regions, -0.1, 10.0), None);
        assert_eq!(hit_test(&[], 1.0, 1.0), None);
        assert_eq!(hit_test(&regions, f64::NAN, 1.0), None);
        assert_eq!(hit_test(&regions, 1.0, f64::INFINITY), None);
    }

    #[test]
    fn hit_lookup_agrees_with_the_suite_rect_contains() {
        let rect = Rect {
            x: 5.0,
            y: 6.0,
            width: 10.0,
            height: 4.0,
        };
        let regions = [HitRegion { id: 9, rect }];
        for (x, y) in [
            (5.0, 6.0),
            (15.0, 10.0),
            (15.0, 6.0),
            (5.0, 10.0),
            (4.999, 6.0),
            (15.001, 8.0),
            (10.0, 10.001),
        ] {
            assert_eq!(
                hit_test(&regions, x, y).is_some(),
                rect.contains(x, y),
                "({x}, {y})"
            );
        }
        // A negative or non-finite region matches nothing, as `Rect::contains` says.
        let broken = [
            region(1, 0.0, 0.0, -5.0, 5.0),
            region(2, f64::NAN, 0.0, 5.0, 5.0),
        ];
        assert_eq!(hit_test(&broken, 0.0, 0.0), None);
    }

    #[test]
    fn first_backing_has_headroom_and_same_size_keeps_it() {
        let Backing::Rebuild { width, height } = plan_backing(None, (400, 100)) else {
            panic!("there is no backing yet");
        };
        assert_eq!((width, height), (500, 125));
        assert_eq!(
            plan_backing(Some((width, height)), (400, 100)),
            Backing::Keep
        );
    }

    #[test]
    fn size_changes_inside_the_backing_do_not_rebuild() {
        let backing = Some((500, 125));
        // Grow within the headroom, shrink a little: all fit.
        for need in [(450, 110), (500, 125), (260, 70), (400, 100)] {
            assert_eq!(plan_backing(backing, need), Backing::Keep, "{need:?}");
        }
    }

    #[test]
    fn growing_past_the_backing_or_shrinking_far_below_it_rebuilds() {
        assert_eq!(
            plan_backing(Some((500, 125)), (501, 100)),
            Backing::Rebuild {
                width: 626,
                height: 125
            }
        );
        // More than twice too large on one axis: give it back.
        assert_eq!(
            plan_backing(Some((500, 125)), (200, 100)),
            Backing::Rebuild {
                width: 250,
                height: 125
            }
        );
        // A tiny backing is not wasteful just because the ratio is large.
        assert_eq!(plan_backing(Some((70, 70)), (10, 10)), Backing::Keep);
    }

    #[test]
    fn a_morph_reallocates_a_handful_of_times_not_once_per_frame() {
        let mut backing: Option<(usize, usize)> = None;
        let mut rebuilds = 0;
        // Expand 120 -> 477 wide over 120 frames, then collapse again.
        let sizes = (0..120)
            .map(|i| 120 + i * 3)
            .chain((0..120).map(|i| 480 - i * 3));
        for width in sizes {
            if let Backing::Rebuild {
                width: w,
                height: h,
            } = plan_backing(backing, (width, 40))
            {
                backing = Some((w, h));
                rebuilds += 1;
            }
            let (backing_width, _) = backing.unwrap();
            assert!(
                backing_width >= width,
                "the backing always covers the window"
            );
        }
        assert!(rebuilds <= 10, "{rebuilds} rebuilds over 240 frames");
    }

    #[test]
    fn backing_is_bounded_and_never_zero() {
        let side = MAX_SURFACE_SIDE as usize;
        assert_eq!(
            plan_backing(None, (usize::MAX, side)),
            Backing::Rebuild {
                width: side,
                height: side
            }
        );
        assert_eq!(
            plan_backing(None, (0, 0)),
            Backing::Rebuild {
                width: 1,
                height: 1
            }
        );
        assert_eq!(
            plan_backing(Some((side, side)), (side, side)),
            Backing::Keep
        );
    }

    #[test]
    fn copy_rows_honours_a_wider_destination() {
        let src: Vec<u32> = (1..=6).collect(); // 3 x 2
        let mut dst = vec![0u32; 5 * 2];
        assert!(copy_rows(&mut dst, 5, &src, 3, 2));
        assert_eq!(dst, vec![1, 2, 3, 0, 0, 4, 5, 6, 0, 0]);
        // Tight destination: one straight copy.
        let mut tight = vec![0u32; 6];
        assert!(copy_rows(&mut tight, 3, &src, 3, 2));
        assert_eq!(tight, src);
    }

    #[test]
    fn copy_rows_refuses_buffers_that_are_too_small() {
        let src = vec![7u32; 6];
        let mut dst = vec![0u32; 4];
        assert!(!copy_rows(&mut dst, 5, &src, 3, 2), "destination too short");
        assert!(
            !copy_rows(&mut dst, 2, &src, 3, 2),
            "stride narrower than the row"
        );
        assert!(
            !copy_rows(&mut [0u32; 6], 3, &src[..5], 3, 2),
            "source too short"
        );
        assert!(
            !copy_rows(&mut dst, usize::MAX, &src, 3, 2),
            "overflow is refused"
        );
        assert!(dst.iter().all(|&p| p == 0), "nothing was written");
        // The last row needs no trailing padding.
        let mut exact = vec![0u32; 5 + 3];
        assert!(copy_rows(&mut exact, 5, &src, 3, 2));
    }

    #[test]
    fn device_regions_round_outwards_and_skip_degenerate() {
        let regions = [
            region(1, 10.2, 5.2, 10.0, 10.0),
            region(2, 0.0, 0.0, 0.0, 10.0),
            region(3, f64::NAN, 0.0, 1.0, 1.0),
        ];
        let device = device_regions(&regions, 1.5);
        // x: floor(15.3) = 15, ceil(30.3) = 31; y: floor(7.8) = 7, ceil(22.8) = 23.
        assert_eq!(device, vec![[15, 7, 31, 23]]);
    }

    #[test]
    fn client_coordinates_become_logical() {
        assert_eq!(client_to_logical(150, 1.5), 100.0);
        assert_eq!(client_to_logical(-30, 1.5), -20.0);
    }
}
