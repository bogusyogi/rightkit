//! The pill capsule's last reported hitbox and window frame, kept for the windows that anchor to
//! it (a window transition, QA geometry, aux cards of a native panel). The clamp limits are the
//! host's ([`HitboxLimits`]). Platform-free.
//!
//! The two values are process statics (they are written by host callbacks that carry no
//! generation). [`clear`] forgets them: `shutdown` calls it, so a restarted pill never anchors to
//! the previous generation's rectangle. Until the new generation reports, the snapshots are
//! `None`.

use std::sync::Mutex;

/// The rendered capsule / action-dock rectangle in the pill window's client coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PillHitbox {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// The last pill frame requested from the native window manager, in physical screen pixels.
/// Reading `outer_position()` immediately after `set_position()` can return the previous
/// animation frame on Windows, which makes auxiliary cards jump even when the reported hitbox is
/// current.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PillPhysicalFrame {
    pub x: f64,
    pub y: f64,
    pub scale: f64,
}

/// Largest hitbox width and height accepted (logical px). A reported rectangle comes from a
/// renderer or native view and is clamped before it anchors any window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HitboxLimits {
    pub max_width: f64,
    pub max_height: f64,
}

/// A reported hitbox after validation: `None` for non-finite values and rectangles under 1 px;
/// otherwise the origin floored at 0 and the size clamped to `1..=limit`.
pub fn clamp_hitbox(
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    limits: &HitboxLimits,
) -> Option<PillHitbox> {
    if !x.is_finite() || !y.is_finite() || !width.is_finite() || !height.is_finite() {
        return None;
    }
    if width < 1.0 || height < 1.0 {
        return None;
    }
    Some(PillHitbox {
        x: x.max(0.0),
        y: y.max(0.0),
        width: width.clamp(1.0, limits.max_width.max(1.0)),
        height: height.clamp(1.0, limits.max_height.max(1.0)),
    })
}

/// Where an auxiliary window (a card, a popup) of logical size `popup_w` x `popup_h` goes so it
/// is centred over the capsule with `gap` logical px between its bottom edge and the capsule's
/// visible top: the physical top-left, from the pill window's physical top-left and `scale`
/// (`frame`) and the capsule rectangle in the pill window (`hitbox`). The visible top comes from
/// the reported hitbox, never from a guess at the window's bottom-aligned frame (the native frame
/// can still be taller than the slim visible capsule for a tick).
pub fn aux_origin(
    frame: &PillPhysicalFrame,
    hitbox: &PillHitbox,
    popup_w: f64,
    popup_h: f64,
    gap: f64,
) -> (i32, i32) {
    let scale = frame.scale.max(0.1);
    let x = frame.x + (hitbox.x + hitbox.width * 0.5 - popup_w * 0.5) * scale;
    let y = frame.y + (hitbox.y - popup_h - gap) * scale;
    (x.round() as i32, y.round() as i32)
}

/// Native window observations used when a pill frame or hitbox has not arrived yet.
pub struct PillWindowProbe<'a> {
    pub outer_position: &'a dyn Fn() -> Option<(f64, f64)>,
    pub scale_factor: &'a dyn Fn() -> Option<f64>,
    pub outer_size: &'a dyn Fn() -> Option<(f64, f64)>,
}

/// Anchor an auxiliary window to the reported pill frame and hitbox or native window bounds.
pub fn aux_position(
    probe: &PillWindowProbe<'_>,
    popup_w: f64,
    popup_h: f64,
    gap: f64,
) -> Option<(i32, i32)> {
    let frame = match pill_physical_frame_snapshot() {
        Some(frame) => frame,
        None => {
            let (x, y) = (probe.outer_position)()?;
            PillPhysicalFrame {
                x,
                y,
                scale: (probe.scale_factor)().unwrap_or(1.0),
            }
        }
    };
    let hitbox = pill_hitbox_snapshot().or_else(|| {
        let scale = frame.scale.max(0.1);
        let (width, height) = (probe.outer_size)()?;
        Some(PillHitbox {
            x: 0.0,
            y: 0.0,
            width: width / scale,
            height: height / scale,
        })
    })?;
    Some(aux_origin(&frame, &hitbox, popup_w, popup_h, gap))
}

static PILL_HITBOX: Mutex<Option<PillHitbox>> = Mutex::new(None);
static PILL_PHYSICAL_FRAME: Mutex<Option<PillPhysicalFrame>> = Mutex::new(None);

/// Store a reported hitbox (see [`clamp_hitbox`]); an invalid report is ignored.
pub fn set_pill_hitbox(x: f64, y: f64, width: f64, height: f64, limits: &HitboxLimits) {
    let Some(hitbox) = clamp_hitbox(x, y, width, height, limits) else {
        return;
    };
    *PILL_HITBOX.lock().unwrap_or_else(|e| e.into_inner()) = Some(hitbox);
}

pub fn pill_hitbox_snapshot() -> Option<PillHitbox> {
    *PILL_HITBOX.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn pill_physical_frame_snapshot() -> Option<PillPhysicalFrame> {
    *PILL_PHYSICAL_FRAME
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Record the window frame the host last asked the window manager for. A report with a
/// non-finite value, or a scale outside `(0, 64]`, is ignored.
pub fn set_pill_physical_frame(x: f64, y: f64, scale: f64) {
    if !x.is_finite() || !y.is_finite() || !scale.is_finite() || scale <= 0.0 || scale > 64.0 {
        return;
    }
    *PILL_PHYSICAL_FRAME
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(PillPhysicalFrame { x, y, scale });
}

/// Forget both snapshots (called by `shutdown`; a host that restarts its pill may call it too).
pub fn clear() {
    *PILL_HITBOX.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *PILL_PHYSICAL_FRAME
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: HitboxLimits = HitboxLimits {
        max_width: 100.0,
        max_height: 50.0,
    };

    #[test]
    fn non_finite_and_tiny_reports_are_rejected() {
        assert_eq!(clamp_hitbox(f64::NAN, 0.0, 10.0, 10.0, &LIMITS), None);
        assert_eq!(clamp_hitbox(0.0, f64::INFINITY, 10.0, 10.0, &LIMITS), None);
        assert_eq!(clamp_hitbox(0.0, 0.0, 0.5, 10.0, &LIMITS), None);
        assert_eq!(clamp_hitbox(0.0, 0.0, 10.0, 0.99, &LIMITS), None);
    }

    #[test]
    fn the_origin_is_floored_and_the_size_clamped_to_the_hosts_limits() {
        let hit = clamp_hitbox(-5.0, 3.0, 5000.0, 5000.0, &LIMITS).unwrap();
        assert_eq!((hit.x, hit.y), (0.0, 3.0));
        assert_eq!((hit.width, hit.height), (100.0, 50.0));
        let small = clamp_hitbox(1.0, 2.0, 40.0, 20.0, &LIMITS).unwrap();
        assert_eq!((small.width, small.height), (40.0, 20.0));
    }

    #[test]
    fn an_aux_window_is_centred_over_the_capsule_and_sits_above_its_visible_top() {
        let frame = PillPhysicalFrame {
            x: 1000.0,
            y: 500.0,
            scale: 2.0,
        };
        let hitbox = PillHitbox {
            x: 100.0,
            y: 30.0,
            width: 60.0,
            height: 20.0,
        };
        // Centre x = 130; popup 40 wide starts at 110; popup 24 tall plus a 1 px gap ends at 30.
        let (x, y) = aux_origin(&frame, &hitbox, 40.0, 24.0, 1.0);
        assert_eq!(x, 1000 + (110.0 * 2.0) as i32);
        assert_eq!(y, 500 + ((30.0 - 24.0 - 1.0) * 2.0) as i32);
    }

    #[test]
    fn a_degenerate_scale_is_floored() {
        let frame = PillPhysicalFrame {
            x: 0.0,
            y: 0.0,
            scale: 0.0,
        };
        let hitbox = PillHitbox {
            x: 10.0,
            y: 10.0,
            width: 10.0,
            height: 10.0,
        };
        let (x, y) = aux_origin(&frame, &hitbox, 10.0, 10.0, 0.0);
        assert_eq!((x, y), (1, 0));
    }

    #[test]
    fn a_nonsense_limit_never_produces_a_zero_size() {
        let zero = HitboxLimits {
            max_width: 0.0,
            max_height: -4.0,
        };
        let hit = clamp_hitbox(0.0, 0.0, 10.0, 10.0, &zero).unwrap();
        assert_eq!((hit.width, hit.height), (1.0, 1.0));
    }
}
