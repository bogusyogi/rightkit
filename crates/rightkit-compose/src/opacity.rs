//! Validated opacity & frame-exact fades on CutRight's frame grid.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{Easing, Keyframe, Rational, Ticks, Track};

/// Finite opacity in `0..=1`. Construction & deserialisation validate bounds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct Opacity(f64);

/// Why an opacity value cannot be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum OpacityError {
    #[error("opacity must be finite")]
    NonFinite,
    #[error("opacity must be within 0..=1")]
    OutOfRange,
}

impl Opacity {
    pub const TRANSPARENT: Self = Self(0.0);
    pub const OPAQUE: Self = Self(1.0);

    /// Reject non-finite or out-of-range values with a typed error.
    pub fn new(value: f64) -> Result<Self, OpacityError> {
        if !value.is_finite() {
            Err(OpacityError::NonFinite)
        } else if !(0.0..=1.0).contains(&value) {
            Err(OpacityError::OutOfRange)
        } else {
            Ok(Self(value))
        }
    }

    /// Explicitly clamp finite values to `0..=1`; reject NaN & infinity.
    pub fn clamped(value: f64) -> Result<Self, OpacityError> {
        if !value.is_finite() {
            return Err(OpacityError::NonFinite);
        }
        Ok(Self(value.clamp(0.0, 1.0)))
    }

    pub fn as_f64(self) -> f64 {
        self.0
    }

    pub fn as_f32(self) -> f32 {
        self.0 as f32
    }

    /// One-key layer opacity track, suitable for `Layer::opacity`.
    pub fn constant_track(self) -> Track<f64> {
        vec![Keyframe {
            at: Ticks::ZERO,
            value: self.0,
            easing: Easing::Hold,
        }]
    }
}

impl TryFrom<f64> for Opacity {
    type Error = OpacityError;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Opacity> for f64 {
    fn from(value: Opacity) -> Self {
        value.0
    }
}

/// Fade shape in absolute output frames. Positive fade lengths use
/// `min(1f32, (into + 1) as f32 / fade_in as f32,
/// (remaining + 1) as f32 / fade_out as f32)`. Nonpositive lengths disable
/// that ramp. In/out ramps combine with `min`, including overlapping fades.
/// Calculations use `f32` before widening so CutRight's opacity bits survive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameFade {
    pub start_frame: i64,
    pub frames: i64,
    pub fade_in_frames: i64,
    pub fade_out_frames: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FrameFadeError {
    #[error("visible frame interval must be nonempty & inside the fade's item")]
    VisibleRange,
    #[error("invalid frame rate, collapsed frame starts or tick overflow")]
    FrameTiming,
}

impl FrameFade {
    /// Opacity of one frame. Empty items & frames outside item return zero.
    pub fn opacity_at(self, frame: i64) -> Opacity {
        let into = i128::from(frame) - i128::from(self.start_frame);
        let remaining = i128::from(self.frames) - into - 1;
        if self.frames <= 0 || into < 0 || remaining < 0 {
            return Opacity::TRANSPARENT;
        }
        let mut opacity = 1.0f32;
        if self.fade_in_frames > 0 {
            opacity = opacity.min((into + 1) as f32 / self.fade_in_frames as f32);
        }
        if self.fade_out_frames > 0 {
            opacity = opacity.min((remaining + 1) as f32 / self.fade_out_frames as f32);
        }
        Opacity(f64::from(opacity.clamp(0.0, 1.0)))
    }

    /// Build a layer-local opacity track for `[visible_start, visible_end)`.
    /// Layer timing must use those absolute frame starts at `fps`. Clipped
    /// items (including negative starts) retain their original fade phase.
    /// Each changed value gets a Hold key at its rounded absolute frame start
    /// minus the visible start; intervening ticks retain that frame's opacity.
    pub fn opacity_track(
        self,
        fps: Rational,
        visible_start: i64,
        visible_end: i64,
    ) -> Result<Track<f64>, FrameFadeError> {
        if self.frames <= 0
            || visible_start < self.start_frame
            || visible_start >= visible_end
            || i128::from(visible_end) > i128::from(self.start_frame) + i128::from(self.frames)
        {
            return Err(FrameFadeError::VisibleRange);
        }
        if fps.frame_at_ticks(Default::default()).is_none() {
            return Err(FrameFadeError::FrameTiming);
        }
        let origin = fps
            .frame_to_ticks(visible_start)
            .ok_or(FrameFadeError::FrameTiming)?
            .0;
        let end = fps
            .frame_to_ticks(visible_end)
            .ok_or(FrameFadeError::FrameTiming)?
            .0;
        Ticks::try_from(crate::SignedTicks(end - origin))
            .map_err(|_| FrameFadeError::FrameTiming)?;
        let mut track: Track<f64> = Vec::new();
        for frame in visible_start..visible_end {
            let value = self.opacity_at(frame).as_f64();
            if track.last().is_some_and(|key| key.value == value) {
                continue;
            }
            let at = fps
                .frame_to_ticks(frame)
                .ok_or(FrameFadeError::FrameTiming)?
                .0
                - origin;
            let at =
                Ticks::try_from(crate::SignedTicks(at)).map_err(|_| FrameFadeError::FrameTiming)?;
            track.push(Keyframe {
                at,
                value,
                easing: Easing::Hold,
            });
        }
        Ok(track)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opacity_validation_clamping_and_serde_are_explicit() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(Opacity::new(value), Err(OpacityError::NonFinite));
            assert_eq!(Opacity::clamped(value), Err(OpacityError::NonFinite));
        }
        for value in [-0.25, 1.5] {
            assert_eq!(Opacity::new(value), Err(OpacityError::OutOfRange));
        }
        assert_eq!(Opacity::clamped(-0.25), Ok(Opacity::TRANSPARENT));
        assert_eq!(Opacity::clamped(1.5), Ok(Opacity::OPAQUE));
        for value in [0.0, 0.25, 1.0] {
            let opacity = Opacity::new(value).unwrap();
            let json = serde_json::to_string(&opacity).unwrap();
            assert_eq!(serde_json::from_str::<Opacity>(&json).unwrap(), opacity);
        }
        assert!(serde_json::from_str::<Opacity>("1.5").is_err());
        assert!(serde_json::from_str::<Opacity>("-0.25").is_err());
    }

    #[test]
    fn fade_endpoints_overlap_and_disabled_ramps() {
        let fade = FrameFade {
            start_frame: 5,
            frames: 4,
            fade_in_frames: 3,
            fade_out_frames: 3,
        };
        let expected = [1.0f32 / 3.0, 2.0 / 3.0, 2.0 / 3.0, 1.0 / 3.0];
        for (index, value) in expected.into_iter().enumerate() {
            assert_eq!(
                fade.opacity_at(5 + index as i64).as_f32().to_bits(),
                value.to_bits()
            );
        }
        assert_eq!(fade.opacity_at(4), Opacity::TRANSPARENT);
        assert_eq!(fade.opacity_at(9), Opacity::TRANSPARENT);
        let no_fade = FrameFade {
            fade_in_frames: -1,
            fade_out_frames: 0,
            ..fade
        };
        assert_eq!(no_fade.opacity_at(6), Opacity::OPAQUE);
        let one = FrameFade {
            frames: 1,
            fade_in_frames: 1,
            fade_out_frames: 1,
            ..fade
        };
        assert_eq!(one.opacity_at(5), Opacity::OPAQUE);
    }

    #[test]
    fn fade_track_rejects_invalid_intervals_rates_and_overflow() {
        let fade = FrameFade {
            start_frame: 0,
            frames: 4,
            fade_in_frames: 2,
            fade_out_frames: 2,
        };
        for (start, end) in [(-1, 2), (2, 2), (0, 5)] {
            assert_eq!(
                fade.opacity_track(Rational::new(30, 1), start, end),
                Err(FrameFadeError::VisibleRange)
            );
        }
        for fps in [
            Rational::new(0, 1),
            Rational::new(1, 0),
            Rational::new(u32::MAX, 1),
        ] {
            assert_eq!(
                fade.opacity_track(fps, 0, 4),
                Err(FrameFadeError::FrameTiming)
            );
        }
        let huge = FrameFade {
            frames: i64::MAX,
            ..fade
        };
        assert_eq!(
            huge.opacity_track(Rational::new(1, u32::MAX), 0, i64::MAX),
            Err(FrameFadeError::FrameTiming)
        );
    }
}
