//! Animation clock maths: easing, repeat and completion. Pure, so the frame sequence a painter
//! sees is unit-tested without a window.

use crate::geometry::Size;

/// The curve applied to an animation progress.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Easing {
    Linear,
    /// Cubic ease-in.
    EaseIn,
    /// Cubic ease-out.
    EaseOut,
    /// Cubic ease-in-out (the old pill state-morph curve).
    EaseInOut,
    /// Ease-out with a small overshoot past 1.0 before settling (a springy expand).
    EaseOutBack,
}

impl Easing {
    /// Maps linear progress `0.0..=1.0` to eased progress. `0.0` maps to `0.0` and `1.0` to `1.0`
    /// for every curve; [`Easing::EaseOutBack`] exceeds `1.0` in between.
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,
            Easing::EaseIn => t * t * t,
            Easing::EaseOut => 1.0 - (1.0 - t).powi(3),
            Easing::EaseInOut => {
                if t < 0.5 {
                    4.0 * t * t * t
                } else {
                    1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
                }
            }
            Easing::EaseOutBack => {
                const C1: f32 = 1.70158;
                const C3: f32 = C1 + 1.0;
                if t <= 0.0 || t >= 1.0 {
                    // Exact endpoints; the polynomial only reaches them to rounding.
                    return t;
                }
                let u = t - 1.0;
                1.0 + C3 * u * u * u + C1 * u * u
            }
        }
    }
}

/// A time-boxed animation. While one is active the owner thread paints one frame per display
/// refresh (paced by `DwmFlush`); when it finishes the thread goes back to blocking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Animation {
    pub duration_ms: u32,
    pub easing: Easing,
    /// Restart at 0 each time the duration elapses, until stopped.
    pub repeat: bool,
}

impl Animation {
    pub const fn once(duration_ms: u32, easing: Easing) -> Self {
        Self {
            duration_ms,
            easing,
            repeat: false,
        }
    }

    pub const fn looping(duration_ms: u32, easing: Easing) -> Self {
        Self {
            duration_ms,
            easing,
            repeat: true,
        }
    }

    /// Eased progress and completion `elapsed_ms` after the animation started.
    ///
    /// A one-shot animation reports `finished` once the duration has passed, with `t == 1.0`
    /// exactly. A repeating one never finishes and wraps from 1 back to 0. A zero duration reads
    /// as one millisecond.
    pub fn sample(&self, elapsed_ms: u64) -> (f32, bool) {
        let duration = u64::from(self.duration_ms.max(1));
        if self.repeat {
            let phase = (elapsed_ms % duration) as f32 / duration as f32;
            (self.easing.apply(phase), false)
        } else if elapsed_ms >= duration {
            (1.0, true)
        } else {
            (
                self.easing.apply(elapsed_ms as f32 / duration as f32),
                false,
            )
        }
    }
}

/// What a painter is told about the frame it is drawing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameInfo {
    /// Eased progress of the active animation, `0.0..=1.0` (a repeating animation wraps). When
    /// no animation is running this is where the last one ended (`1.0`), or `0.0` if none has run.
    pub t: f32,
    /// Milliseconds since the active animation started (the end time once it has finished).
    pub elapsed_ms: u64,
    /// Effective DPI of the monitor the window is on (96 is 100%).
    pub dpi: u32,
    /// Window size in logical pixels: the area `PaintCtx` coordinates address.
    pub size: Size,
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Easing; 5] = [
        Easing::Linear,
        Easing::EaseIn,
        Easing::EaseOut,
        Easing::EaseInOut,
        Easing::EaseOutBack,
    ];

    #[test]
    fn every_curve_starts_at_zero_and_ends_at_one() {
        for easing in ALL {
            assert_eq!(easing.apply(0.0), 0.0, "{easing:?}");
            assert!((easing.apply(1.0) - 1.0).abs() < 1.0e-6, "{easing:?}");
            // Out-of-range input is clamped.
            assert_eq!(easing.apply(-3.0), easing.apply(0.0));
            assert_eq!(easing.apply(9.0), easing.apply(1.0));
        }
    }

    #[test]
    fn curve_shapes() {
        assert_eq!(Easing::Linear.apply(0.25), 0.25);
        assert!(Easing::EaseIn.apply(0.5) < 0.5);
        assert!(Easing::EaseOut.apply(0.5) > 0.5);
        assert_eq!(Easing::EaseInOut.apply(0.5), 0.5);
        assert!(Easing::EaseInOut.apply(0.25) < 0.25);
        assert!(Easing::EaseInOut.apply(0.75) > 0.75);
        // The overshoot curve passes 1.0 before settling.
        assert!(Easing::EaseOutBack.apply(0.7) > 1.0);
    }

    #[test]
    fn eased_curves_are_monotonic_except_the_overshoot() {
        for easing in [
            Easing::Linear,
            Easing::EaseIn,
            Easing::EaseOut,
            Easing::EaseInOut,
        ] {
            let mut last = 0.0f32;
            for step in 0..=100 {
                let v = easing.apply(step as f32 / 100.0);
                assert!(v >= last - 1.0e-6, "{easing:?} at {step}");
                last = v;
            }
        }
    }

    #[test]
    fn one_shot_runs_then_finishes_at_exactly_one() {
        let anim = Animation::once(200, Easing::Linear);
        assert_eq!(anim.sample(0), (0.0, false));
        let (t, done) = anim.sample(100);
        assert!((t - 0.5).abs() < 1.0e-6 && !done);
        assert_eq!(anim.sample(200), (1.0, true));
        assert_eq!(anim.sample(10_000), (1.0, true));
    }

    #[test]
    fn repeat_wraps_and_never_finishes() {
        let anim = Animation::looping(100, Easing::Linear);
        let (t, done) = anim.sample(250);
        assert!((t - 0.5).abs() < 1.0e-6 && !done);
        let (t, done) = anim.sample(100);
        assert_eq!((t, done), (0.0, false));
        assert!(!anim.sample(u64::MAX).1);
    }

    #[test]
    fn zero_duration_reads_as_one_millisecond() {
        let once = Animation::once(0, Easing::EaseIn);
        assert_eq!(once.sample(0), (0.0, false));
        assert_eq!(once.sample(1), (1.0, true));
        let looping = Animation::looping(0, Easing::Linear);
        assert_eq!(looping.sample(5), (0.0, false));
    }

    #[test]
    fn frame_sequence_at_60_hz_is_monotonic_and_ends_on_one() {
        let anim = Animation::once(180, Easing::EaseInOut);
        let mut last = -1.0f32;
        let mut finished_at = None;
        for frame in 0..30u64 {
            let elapsed = frame * 1000 / 60;
            let (t, done) = anim.sample(elapsed);
            assert!(t >= last);
            last = t;
            if done {
                finished_at = Some(frame);
                break;
            }
        }
        assert_eq!(last, 1.0);
        // 180 ms is reached on frame 11 (183 ms) at 60 Hz.
        assert_eq!(finished_at, Some(11));
    }
}
