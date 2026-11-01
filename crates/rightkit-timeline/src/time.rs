//! Rational time base and the single frame-rounding rule.
//!
//! All timeline times are integer [`Ticks`] at [`TICKS_PER_SECOND`]. That rate
//! is divisible by every common video rate (24, 25, 30, 50, 60 and their
//! 1000/1001 NTSC variants) and by 44.1 kHz and 48 kHz audio, so frame and
//! sample boundaries of those rates are exact.
//!
//! # The frame-rounding rule
//!
//! Every conversion between time and frames, and between ticks and any other
//! unit (audio samples, microseconds), goes through [`round_div_half_down`]:
//! the exact rational value is rounded to the nearest integer and an exact
//! tie rounds toward the lower integer (half-down). Preview, render, and
//! export must call these functions and nothing else.

use serde::{Deserialize, Serialize};

use crate::error::TimelineError;

/// Ticks per second of the timeline time base (see module docs).
pub const TICKS_PER_SECOND: i64 = 705_600_000;

/// A point or span of timeline/source time in ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Ticks(pub i64);

impl Ticks {
    pub const ZERO: Ticks = Ticks(0);

    /// Exact conversion from milliseconds.
    pub fn from_millis(ms: i64) -> Ticks {
        Ticks(ms * (TICKS_PER_SECOND / 1000))
    }

    /// Converts to a unit with `units_per_second` (e.g. audio samples) using the shared rule.
    pub fn to_units(self, units_per_second: i64) -> i64 {
        round_div_half_down(
            self.0 as i128 * units_per_second as i128,
            TICKS_PER_SECOND as i128,
        ) as i64
    }
}

/// `round(n / d)` with exact ties rounding toward the lower integer. `d` must be positive.
pub fn round_div_half_down(n: i128, d: i128) -> i128 {
    debug_assert!(d > 0);
    let q = n.div_euclid(d);
    let r = n.rem_euclid(d);
    if 2 * r > d {
        q + 1
    } else {
        q
    }
}

/// Project frame rate as an exact rational (`num/den` frames per second).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fps {
    pub num: u32,
    pub den: u32,
}

impl Fps {
    pub fn new(num: u32, den: u32) -> Result<Self, TimelineError> {
        if num == 0 || den == 0 {
            return Err(TimelineError::InvalidFps);
        }
        Ok(Self { num, den })
    }

    /// Start of frame `frame` in ticks.
    pub fn frame_to_ticks(self, frame: i64) -> Ticks {
        Ticks(round_div_half_down(
            frame as i128 * self.den as i128 * TICKS_PER_SECOND as i128,
            self.num as i128,
        ) as i64)
    }

    /// Frame index (or frame count for a duration) nearest to `t`; ties round down.
    pub fn ticks_to_frame(self, t: Ticks) -> i64 {
        round_div_half_down(
            t.0 as i128 * self.num as i128,
            self.den as i128 * TICKS_PER_SECOND as i128,
        ) as i64
    }

    /// Snaps a time onto the frame grid using the shared rule.
    pub fn snap(self, t: Ticks) -> Ticks {
        self.frame_to_ticks(self.ticks_to_frame(t))
    }

    pub fn is_aligned(self, t: Ticks) -> bool {
        self.snap(t) == t
    }

    /// `num/den` as FFmpeg expects it (`30000/1001`).
    pub fn ffmpeg_rational(self) -> String {
        format!("{}/{}", self.num, self.den)
    }
}

/// Gain or attenuation in thousandths of a dB (integer so JSON and equality are exact).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MilliDb(pub i32);

impl MilliDb {
    pub const ZERO: MilliDb = MilliDb(0);

    pub fn from_db(db: f64) -> MilliDb {
        MilliDb((db * 1000.0).round() as i32)
    }

    /// Fixed three-decimal dB text (`-12.500`) for FFmpeg `volume=...dB`.
    pub fn ffmpeg_db(self) -> String {
        let sign = if self.0 < 0 { "-" } else { "" };
        let a = self.0.unsigned_abs();
        format!("{sign}{}.{:03}dB", a / 1000, a % 1000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_rule_is_half_down() {
        assert_eq!(round_div_half_down(1, 2), 0);
        assert_eq!(round_div_half_down(3, 2), 1);
        assert_eq!(round_div_half_down(5, 2), 2);
        assert_eq!(round_div_half_down(7, 3), 2);
        assert_eq!(round_div_half_down(8, 3), 3);
        assert_eq!(round_div_half_down(0, 5), 0);
        assert_eq!(round_div_half_down(-1, 2), -1);
        assert_eq!(round_div_half_down(-3, 2), -2);
    }

    #[test]
    fn frame_boundaries_at_30fps() {
        let f = Fps::new(30, 1).unwrap();
        let frame = TICKS_PER_SECOND / 30;
        assert_eq!(f.ticks_to_frame(Ticks(0)), 0);
        assert_eq!(f.ticks_to_frame(Ticks(frame)), 1);
        // exactly half a frame past frame 1 ties down to 1
        assert_eq!(f.ticks_to_frame(Ticks(frame + frame / 2)), 1);
        // one tick past the tie rounds up to 2
        assert_eq!(f.ticks_to_frame(Ticks(frame + frame / 2 + 1)), 2);
        assert_eq!(f.ticks_to_frame(Ticks(frame + frame / 2 - 1)), 1);
        assert_eq!(f.frame_to_ticks(30), Ticks(TICKS_PER_SECOND));
    }

    #[test]
    fn ntsc_rates_are_exact() {
        for (n, d) in [
            (24000, 1001),
            (30000, 1001),
            (60000, 1001),
            (25, 1),
            (60, 1),
        ] {
            let f = Fps::new(n, d).unwrap();
            for k in [0i64, 1, 2, 7, 1000, 123_456] {
                let t = f.frame_to_ticks(k);
                assert_eq!(f.ticks_to_frame(t), k, "{n}/{d} frame {k}");
                assert!(f.is_aligned(t));
                // exact (no rounding needed)
                assert_eq!(
                    k as i128 * d as i128 * TICKS_PER_SECOND as i128 % n as i128,
                    0
                );
            }
        }
        let f = Fps::new(30000, 1001).unwrap();
        assert_eq!(f.frame_to_ticks(30000), Ticks(1001 * TICKS_PER_SECOND));
    }

    #[test]
    fn snap_is_idempotent_and_uses_same_rule() {
        let f = Fps::new(24, 1).unwrap();
        let t = Ticks(123_456_789);
        let s = f.snap(t);
        assert_eq!(f.snap(s), s);
        assert_eq!(s, f.frame_to_ticks(f.ticks_to_frame(t)));
    }

    #[test]
    fn sample_conversion_shares_rule() {
        // 1 frame at 30 fps = 1600 samples at 48 kHz exactly
        let f = Fps::new(30, 1).unwrap();
        assert_eq!(f.frame_to_ticks(1).to_units(48_000), 1600);
        // half-sample ties down: 1 tick * 48000 / 705.6e6 is tiny -> 0
        assert_eq!(Ticks(1).to_units(48_000), 0);
        assert_eq!(Ticks(14_700).to_units(48_000), 1);
        // 1.5 samples = 22_050 ticks -> ties down to 1
        assert_eq!(Ticks(22_050).to_units(48_000), 1);
        assert_eq!(Ticks(22_051).to_units(48_000), 2);
    }

    #[test]
    fn rejects_zero_fps() {
        assert_eq!(Fps::new(0, 1), Err(TimelineError::InvalidFps));
        assert_eq!(Fps::new(30, 0), Err(TimelineError::InvalidFps));
    }

    #[test]
    fn millidb_formatting() {
        assert_eq!(MilliDb::from_db(-12.5).ffmpeg_db(), "-12.500dB");
        assert_eq!(MilliDb(0).ffmpeg_db(), "0.000dB");
        assert_eq!(MilliDb(6).ffmpeg_db(), "0.006dB");
        assert_eq!(MilliDb(-6).ffmpeg_db(), "-0.006dB");
    }
}
