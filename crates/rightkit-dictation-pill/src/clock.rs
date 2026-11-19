//! Checked time arithmetic for numbers that come from the host.
//!
//! `Instant + Duration` panics when the sum overflows, and `Duration::from_millis` takes any
//! `u64`. Every place where a host number meets a clock goes through [`after`], which clamps the
//! delay and never panics.

use std::time::{Duration, Instant};

/// The longest delay the crate schedules from a host number: one day, in milliseconds. A host
/// asking for more gets this.
pub const MAX_DELAY_MS: u64 = 86_400_000;

/// `now + ms`, with `ms` clamped to [`MAX_DELAY_MS`]. When the clock cannot represent the result
/// the answer is `now` itself (an immediately due timer), never a panic.
pub(crate) fn after(now: Instant, ms: u64) -> Instant {
    now.checked_add(Duration::from_millis(ms.min(MAX_DELAY_MS)))
        .unwrap_or(now)
}

/// `now + seconds` for a duration the crate itself computed from a spring: non-finite or negative
/// values count as zero and the result is clamped to one minute.
pub(crate) fn after_secs(now: Instant, secs: f64) -> Instant {
    let secs = if secs.is_finite() {
        secs.clamp(0.0, 60.0)
    } else {
        0.0
    };
    now.checked_add(Duration::from_secs_f64(secs))
        .unwrap_or(now)
}

/// `now + bound`, never panicking.
pub(crate) fn after_duration(now: Instant, bound: Duration) -> Instant {
    now.checked_add(bound.min(Duration::from_millis(MAX_DELAY_MS)))
        .unwrap_or(now)
}
