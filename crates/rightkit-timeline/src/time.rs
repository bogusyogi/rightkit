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
