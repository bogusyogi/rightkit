//! Wall-clock source for bundle timestamps, heartbeats and stale-session
//! recovery. Frame timing never uses this clock: it uses the helper's host
//! timestamps through the clock map in clock.json.

use std::cell::Cell;
use std::time::{SystemTime, UNIX_EPOCH};

pub trait Clock {
    fn unix_ms(&self) -> u64;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn unix_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

/// A clock the test sets by hand.
#[derive(Debug, Clone, Default)]
pub struct ManualClock {
    now: Cell<u64>,
}

impl ManualClock {
    pub fn new(unix_ms: u64) -> Self {
        ManualClock {
            now: Cell::new(unix_ms),
        }
    }

    pub fn set(&self, unix_ms: u64) {
        self.now.set(unix_ms);
    }

    pub fn advance(&self, ms: u64) {
        self.now.set(self.now.get() + ms);
    }
}

impl Clock for ManualClock {
    fn unix_ms(&self) -> u64 {
        self.now.get()
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` in UTC, the timestamp form record-session.v1 accepts.
pub fn iso8601_utc(unix_ms: u64) -> String {
    let unix_secs = (unix_ms / 1000) as i64;
    let days = unix_secs.div_euclid(86_400);
    let secs = unix_secs.rem_euclid(86_400);
    // Days-from-civil inverse (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_instants() {
        assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601_utc(951_782_400_000), "2000-02-29T00:00:00Z");
        assert_eq!(iso8601_utc(951_868_800_999), "2000-03-01T00:00:00Z");
        assert_eq!(iso8601_utc(1_791_417_600_000), "2026-10-08T00:00:00Z");
    }
}
