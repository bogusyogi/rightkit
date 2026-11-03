//! An injectable wall clock. [`DateTime::now_utc`] and [`unix_millis_now`] read the system
//! clock by default; the `_with` forms take any [`Clock`], so a caller can replace the clock
//! (for example a fixed clock in an end-to-end harness) without touching the call site (#992).
use crate::DateTime;
use std::time::{SystemTime, UNIX_EPOCH};

/// A source of the current time, as milliseconds since the Unix epoch in UTC.
pub trait Clock: Send + Sync {
    fn unix_millis(&self) -> i64;
}

/// The operating system's wall clock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn unix_millis(&self) -> i64 {
        let since = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        since.as_millis() as i64
    }
}

/// Milliseconds since the Unix epoch, read from the system clock.
pub fn unix_millis_now() -> i64 {
    unix_millis_with(&SystemClock)
}

/// Milliseconds since the Unix epoch, read from `clock`.
pub fn unix_millis_with(clock: &dyn Clock) -> i64 {
    clock.unix_millis()
}

impl DateTime {
    /// The current UTC time from the system clock, with sub-second nanoseconds.
    pub fn now_utc() -> DateTime {
        DateTime::now_utc_with(&SystemClock)
    }

    /// The current UTC time read from `clock`, with sub-second nanoseconds.
    pub fn now_utc_with(clock: &dyn Clock) -> DateTime {
        let ms = clock.unix_millis();
        let secs = ms.div_euclid(1_000);
        let nanos = (ms.rem_euclid(1_000) as u32) * 1_000_000;
        // Offset 0 is always a valid offset, so `from_unix` cannot return `None` here.
        DateTime {
            nanos,
            ..DateTime::from_unix(secs, 0).expect("offset 0 is always valid")
        }
    }
}
