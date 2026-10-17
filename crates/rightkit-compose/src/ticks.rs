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

    /// The start of frame `frames` at `fps`, rounded down. Exact whenever frame duration is a whole number of ticks. For CutRight
    /// half-down rounding use [`Ticks::from_frames_rounded`]. `None` for an
    /// invalid rate or overflow.
    pub fn from_frames(frames: u64, fps: Rational) -> Option<Ticks> {
        if fps.num == 0 || fps.den == 0 {
            return None;
        }
        let ticks = u128::from(frames) * u128::from(fps.den) * u128::from(TICKS_PER_SECOND)
            / u128::from(fps.num);
        u64::try_from(ticks).ok().map(Ticks)
    }

    /// Frame start rounded to nearest tick, with exact ties toward the lower
    /// integer, matching CutRight. `None` for an invalid rate or overflow.
    pub fn from_frames_rounded(frames: u64, fps: Rational) -> Option<Ticks> {
        if fps.num == 0 || fps.den == 0 {
            return None;
        }
        let n = u128::from(frames) * u128::from(fps.den) * u128::from(TICKS_PER_SECOND);
        let d = u128::from(fps.num);
        u64::try_from(n / d + u128::from(2 * (n % d) > d))
            .ok()
            .map(Ticks)
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
        if self.num == 0 || self.den == 0 || numerator % u128::from(self.num) != 0 {
            return None;
        }
        u64::try_from(numerator / u128::from(self.num)).ok()
    }
}

/// Signed source time. `i128` holds both negative CutRight `i64` source times
/// & every unsigned `Ticks` value. The wire form is a bare JSON integer, so
/// serialised values are bounded to `SignedTicks::MIN..=SignedTicks::MAX`
/// (`i64::MIN..=u64::MAX`); serialising or deserialising outside that range is
/// an error rather than a value that cannot be read back. Timeline positions &
/// durations remain unsigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct SignedTicks(pub i128);

impl SignedTicks {
    /// Smallest value the wire representation carries (`i64::MIN`).
    pub const MIN: SignedTicks = SignedTicks(i64::MIN as i128);
    /// Largest value the wire representation carries (`u64::MAX`).
    pub const MAX: SignedTicks = SignedTicks(u64::MAX as i128);

    /// True when the value fits the wire range.
    pub fn is_wire_representable(self) -> bool {
        (Self::MIN.0..=Self::MAX.0).contains(&self.0)
    }
}

impl Serialize for SignedTicks {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if let Ok(value) = i64::try_from(self.0) {
            serializer.serialize_i64(value)
        } else if let Ok(value) = u64::try_from(self.0) {
            serializer.serialize_u64(value)
        } else {
            Err(serde::ser::Error::custom(
                "source ticks outside i64::MIN..=u64::MAX",
            ))
        }
    }
}

// Internally tagged serde enums buffer integer fields as i64/u64 & do not
// implement deserialize_i128. Accept those via deserialize_any.
impl<'de> Deserialize<'de> for SignedTicks {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Integer;
        impl serde::de::Visitor<'_> for Integer {
            type Value = SignedTicks;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an integer source tick count within i64::MIN..=u64::MAX")
            }

            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(SignedTicks(i128::from(value)))
            }

            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(SignedTicks(i128::from(value)))
            }

            fn visit_i128<E: serde::de::Error>(self, value: i128) -> Result<Self::Value, E> {
                let value = SignedTicks(value);
                if value.is_wire_representable() {
                    Ok(value)
                } else {
                    Err(E::custom("source ticks outside i64::MIN..=u64::MAX"))
                }
            }
        }
        deserializer.deserialize_any(Integer)
    }
}

impl From<Ticks> for SignedTicks {
    fn from(value: Ticks) -> Self {
        Self(i128::from(value.0))
    }
}

impl TryFrom<SignedTicks> for Ticks {
    type Error = std::num::TryFromIntError;

    fn try_from(value: SignedTicks) -> Result<Self, Self::Error> {
        u64::try_from(value.0).map(Ticks)
    }
}

impl Rational {
    /// Signed frame start using CutRight's half-down rounding. Invalid rates
    /// return `None`; all `i64` frame indices fit in `SignedTicks`.
    pub fn frame_to_ticks(self, frame: i64) -> Option<SignedTicks> {
        if self.num == 0 || self.den == 0 {
            return None;
        }
        let n = i128::from(frame) * i128::from(self.den) * i128::from(TICKS_PER_SECOND);
        let d = i128::from(self.num);
        Some(SignedTicks(
            n.div_euclid(d) + i128::from(2 * n.rem_euclid(d) > d),
        ))
    }

    /// Nearest frame index, with exact ties toward the lower integer.
    /// Returns `None` for invalid rates or overflow.
    pub fn ticks_to_frame(self, ticks: SignedTicks) -> Option<i64> {
        if self.num == 0 || self.den == 0 {
            return None;
        }
        let n = ticks.0.checked_mul(i128::from(self.num))?;
        let d = i128::from(self.den) * i128::from(TICKS_PER_SECOND);
        i64::try_from(n.div_euclid(d) + i128::from(2 * n.rem_euclid(d) > d)).ok()
    }

    /// Frame owning `ticks`: `ft(f) <= ticks < ft(f + 1)`, where `ft` rounds
    /// half-down. Unlike `ticks_to_frame`, this keeps ownership until next
    /// frame start. Rates above one frame per tick are rejected.
    pub fn frame_at_ticks(self, ticks: SignedTicks) -> Option<i64> {
        let a = i128::from(self.den) * i128::from(TICKS_PER_SECOND);
        let b = i128::from(self.num);
        if b == 0 || a < b {
            return None;
        }
        let n = ticks.0.checked_mul(2)?.checked_add(1)?.checked_mul(b)?;
        i64::try_from(n.div_euclid(2 * a)).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_down_including_negative_ties_and_overflow() {
        let fps = Rational::new(1_411_200_000, 1);
        assert_eq!(fps.frame_to_ticks(1), Some(SignedTicks(0)));
        assert_eq!(fps.frame_to_ticks(-1), Some(SignedTicks(-1)));
        assert_eq!(fps.frame_to_ticks(3), Some(SignedTicks(1)));
        assert_eq!(fps.frame_to_ticks(-3), Some(SignedTicks(-2)));
        assert_eq!(
            Ticks::from_frames_rounded(1, Rational::new(11, 1)),
            Some(Ticks(64_145_455))
        );
        assert_eq!(
            Ticks::from_frames_rounded(u64::MAX, Rational::new(1, u32::MAX)),
            None
        );
        for fps in [Rational::new(0, 1), Rational::new(1, 0)] {
            assert_eq!(fps.frame_to_ticks(1), None);
            assert_eq!(fps.frame_at_ticks(SignedTicks(0)), None);
            assert_eq!(fps.ticks_to_frame(SignedTicks(0)), None);
            assert_eq!(Ticks::from_frames_rounded(1, fps), None);
        }
        assert_eq!(
            Rational::new(30, 1).frame_at_ticks(SignedTicks(i128::MAX)),
            None
        );
        assert_eq!(
            Ticks::try_from(SignedTicks(-1)),
            Err(u64::try_from(-1i128).unwrap_err())
        );
        assert_eq!(
            Ticks::try_from(SignedTicks::from(Ticks::MAX)),
            Ok(Ticks::MAX)
        );
    }

    #[test]
    fn wire_round_trips_at_bounds_and_rejects_outside() {
        for value in [
            SignedTicks::MIN,
            SignedTicks(-1),
            SignedTicks(0),
            SignedTicks::MAX,
        ] {
            let json = serde_json::to_string(&value).unwrap();
            assert_eq!(serde_json::from_str::<SignedTicks>(&json).unwrap(), value);
        }
        assert_eq!(
            serde_json::to_string(&SignedTicks::MAX).unwrap(),
            u64::MAX.to_string()
        );
        for value in [
            SignedTicks(SignedTicks::MIN.0 - 1),
            SignedTicks(SignedTicks::MAX.0 + 1),
        ] {
            assert!(serde_json::to_string(&value).is_err());
        }
        assert!(serde_json::from_str::<SignedTicks>("18446744073709551616").is_err());
        assert!(serde_json::from_str::<SignedTicks>("-9223372036854775809").is_err());
    }
}
