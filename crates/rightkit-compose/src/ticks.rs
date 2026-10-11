//! Integer time on the CutRight timeline clock.
//!
//! [`Ticks`] counts [`TICKS_PER_SECOND`] per second. The rate is chosen so that
//! every common frame rate, including NTSC rates such as 30000/1001, has an
//! integer frame duration: one NTSC frame is exactly 23,543,520 ticks. A
//! millisecond is exactly 705,600 ticks, so legacy millisecond values convert
//! without rounding.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The CutRight timeline clock rate (CR cutright-timeline/src/time.rs).
pub const TICKS_PER_SECOND: u64 = 705_600_000;

/// Ticks in one millisecond. Exact: `TICKS_PER_SECOND / 1000`.
pub const TICKS_PER_MS: u64 = 705_600;

/// A time or duration in ticks of [`TICKS_PER_SECOND`]. Serialises as a bare
/// integer, under field names ending in `_ticks`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Ticks(pub u64);

impl Ticks {
    pub const ZERO: Ticks = Ticks(0);
    /// The largest representable time. Used as an open-ended source bound.
    pub const MAX: Ticks = Ticks(u64::MAX);

    /// Converts milliseconds exactly. Values beyond `u64::MAX` ticks, about
    /// 830 years, saturate to [`Ticks::MAX`].
    pub const fn from_ms(ms: u64) -> Ticks {
        Ticks(ms.saturating_mul(TICKS_PER_MS))
    }

    /// `num / den` seconds in ticks, rounded down. `None` when `den` is zero or
    /// the result does not fit in `u64`.
    pub fn from_seconds_rational(num: u64, den: u64) -> Option<Ticks> {
        if den == 0 {
            return None;
        }
        let ticks = u128::from(num) * u128::from(TICKS_PER_SECOND) / u128::from(den);
        u64::try_from(ticks).ok().map(Ticks)
    }

    /// The start of frame `frames` at `fps`, rounded down. Exact whenever the
    /// frame duration is a whole number of ticks, as for every rate CutRight
    /// emits. `None` when `fps.num` is zero or the result overflows.
    pub fn from_frames(frames: u64, fps: Rational) -> Option<Ticks> {
        if fps.num == 0 {
            return None;
        }
        let ticks = u128::from(frames) * u128::from(fps.den) * u128::from(TICKS_PER_SECOND)
            / u128::from(fps.num);
        u64::try_from(ticks).ok().map(Ticks)
    }

    /// The time in milliseconds as a float, for display and legacy consumers.
    pub fn to_ms_f64(self) -> f64 {
        self.0 as f64 / TICKS_PER_MS as f64
    }

    /// The time in seconds as a float.
    pub fn to_seconds_f64(self) -> f64 {
        self.0 as f64 / TICKS_PER_SECOND as f64
    }

    pub fn checked_add(self, other: Ticks) -> Option<Ticks> {
        self.0.checked_add(other.0).map(Ticks)
    }

    pub fn checked_sub(self, other: Ticks) -> Option<Ticks> {
        self.0.checked_sub(other.0).map(Ticks)
    }

    pub fn saturating_add(self, other: Ticks) -> Ticks {
        Ticks(self.0.saturating_add(other.0))
    }
}

impl fmt::Display for Ticks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ticks", self.0)
    }
}

/// A rational frame rate, `num / den` frames per second. NTSC 29.97 fps is
/// `30000 / 1001`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rational {
    pub num: u32,
    pub den: u32,
}

impl Rational {
    pub const fn new(num: u32, den: u32) -> Self {
        Self { num, den }
    }

    /// Frames per second as a float, for display only. Never used for timing.
    pub fn as_f64(self) -> f64 {
        f64::from(self.num) / f64::from(self.den)
    }

    /// Ticks in one frame at this rate, if it is a whole number.
    pub fn frame_ticks(self) -> Option<u64> {
        let numerator = u128::from(self.den) * u128::from(TICKS_PER_SECOND);
        if self.num == 0 || numerator % u128::from(self.num) != 0 {
            return None;
        }
        u64::try_from(numerator / u128::from(self.num)).ok()
    }
}
