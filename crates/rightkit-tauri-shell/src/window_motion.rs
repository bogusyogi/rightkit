//! Window motion helpers that need no overlay: shrink a window onto a target rectangle.
//!
//! The host chooses the rectangle, frame count, timing and easing. The animation blocks the
//! calling thread, so run it away from the UI thread.

use std::time::Duration;

use tauri::{PhysicalPosition, PhysicalSize, Runtime, WebviewWindow};

/// Most frames one motion may take.
pub const MAX_STEPS: u32 = 240;
/// Longest pause between two frames.
pub const MAX_FRAME: Duration = Duration::from_millis(100);

/// How a motion is paced.
#[derive(Debug, Clone, Copy)]
pub struct MotionTiming {
    /// Number of frames (1 to [`MAX_STEPS`]).
    pub steps: u32,
    /// Pause after each frame (at most [`MAX_FRAME`]).
    pub frame: Duration,
}

/// `t * t`, slow start and fast end.
pub fn ease_in_quad(t: f64) -> f64 {
    t * t
}

/// Move and resize `window` from its current outer frame onto `target` (`[x, y, width, height]`,
/// physical pixels) in `timing.steps` frames; frame `i` is at `ease(i / steps)` of the way. Width
/// and height never drop below 1. Returns `false`, having moved nothing, when the window's current
/// frame cannot be read, `target` is not finite, or `timing.steps` is 0 or over [`MAX_STEPS`].
/// A non-finite easing result ends the motion and returns `false`. The caller owns visibility
/// and destruction.
pub fn shrink_onto<R: Runtime>(
    window: &WebviewWindow<R>,
    target: [f64; 4],
    timing: MotionTiming,
    ease: fn(f64) -> f64,
) -> bool {
    let [tx, ty, tw, th] = target;
    if !target.iter().all(|value| value.is_finite())
        || timing.steps == 0
        || timing.steps > MAX_STEPS
    {
        return false;
    }
    let frame = timing.frame.min(MAX_FRAME);
    let (Ok(pos), Ok(size)) = (window.outer_position(), window.outer_size()) else {
        return false;
    };
    for i in 1..=timing.steps {
        let t = ease(i as f64 / timing.steps as f64);
        if !t.is_finite() {
            return false;
        }
        let t = t.clamp(0.0, 1.0);
        let lerp = |a: f64, b: f64| a + (b - a) * t;
        let x = lerp(pos.x as f64, tx);
        let y = lerp(pos.y as f64, ty);
        let w = lerp(size.width as f64, tw).max(1.0);
        let h = lerp(size.height as f64, th).max(1.0);
        let _ = window.set_position(PhysicalPosition::new(x as i32, y as i32));
        let _ = window.set_size(PhysicalSize::new(w as u32, h as u32));
        std::thread::sleep(frame);
    }
    true
}
