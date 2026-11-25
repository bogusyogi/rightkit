//! The crash-loop latch of a supervisor over a [`crate::PrivateChild`] (Pulse #570, ES-14).
//!
//! A restart budget ([`crate::supervisor::SupervisorEvent::Exhausted`]) that runs out is not the
//! end of the supervisor: it pauses ([`EXHAUST_PAUSE`]) and supervises again. [`AFTER`] exhaustions
//! inside [`WINDOW_MS`] latch it: it stays alive and idle for [`COOLDOWN_MS`] after the last one
//! and then supervises again, so a binary that cannot start is not relaunched at budget speed all
//! day. A child that stays ready for [`CLEARS_AFTER`] clears the latch.
//!
//! The decisions are made from the supervisor process's own list ([`CrashLatch`]). The record in
//! `engine-latch.json` (`version`, `pid`, `exhaustedMs`, `latchedUntilMs`) is written for a reader
//! (a status command) to show and is never read back to hold a start back: a stale file from an
//! older build or an earlier session cannot refuse a child. [`CrashLatch::new`] deletes any file
//! already there.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

/// Exhaustions inside this window count toward the latch.
pub const WINDOW_MS: u64 = 30 * 60 * 1000;
/// This many exhaustions inside the window latch the supervisor.
pub const AFTER: usize = 3;
/// A latched supervisor stays alive and idle this long after the last exhaustion, then
/// supervises again.
pub const COOLDOWN_MS: u64 = 15 * 60 * 1000;
/// A child that stays ready this long clears the latch.
pub const CLEARS_AFTER: Duration = Duration::from_secs(600);
/// The pause after an exhaustion that does not latch, before supervising again.
pub const EXHAUST_PAUSE: Duration = Duration::from_secs(30);
/// How often an idle (latched or pausing) supervisor logs that it is still waiting.
pub const IDLE_LOG_EVERY: Duration = Duration::from_secs(60);
/// The record's layout version. A file without it was written by a build that ran under a
/// LaunchAgent or logon task, which no longer exist; it never holds anything back.
pub const FILE_VERSION: u64 = 2;

/// What one exhausted budget did to the latch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exhaustion {
    /// Exhaustions inside the window, this one included.
    pub count: usize,
    /// When the latch lets go (Unix ms), `None` when this one did not latch.
    pub until_ms: Option<u64>,
}

impl Exhaustion {
    /// How long to stay idle when this one latched.
    pub fn cooldown(&self) -> Option<Duration> {
        self.until_ms.map(|_| Duration::from_millis(COOLDOWN_MS))
    }
}

/// The exhausted restart budgets of this supervisor process.
#[derive(Debug, Default)]
pub struct CrashLatch {
    exhaustions: Vec<u64>,
}

impl CrashLatch {
    /// A fresh session's latch with no history.
    pub fn new() -> Self {
        Self::default()
    }

    /// The exhaustion times (Unix ms) inside the window, oldest first.
    pub fn exhaustions(&self) -> &[u64] {
        &self.exhaustions
    }

    /// Records one exhausted budget at `now_ms`; the answer says whether it latched.
    pub fn record_exhaustion(&mut self, now_ms: u64) -> Exhaustion {
        self.exhaustions
            .retain(|at| now_ms.saturating_sub(*at) <= WINDOW_MS);
        self.exhaustions.push(now_ms);
        let latched = self.exhaustions.len() >= AFTER;
        Exhaustion {
            count: self.exhaustions.len(),
            until_ms: latched.then(|| now_ms.saturating_add(COOLDOWN_MS)),
        }
    }

    /// A child stayed ready long enough: the streak is over. Returns whether there was one.
    pub fn clear(&mut self) -> bool {
        let had = !self.exhaustions.is_empty();
        self.exhaustions.clear();
        had
    }

    /// The record's JSON for process `pid`, with the time the latch lets go when latched.
    pub fn record_json(&self, pid: u32, until_ms: Option<u64>) -> String {
        serde_json::json!({
            "version": FILE_VERSION,
            "pid": pid,
            "exhaustedMs": self.exhaustions,
            "latchedUntilMs": until_ms,
        })
        .to_string()
    }

    /// Writes the record to `path` (atomic, not durable: a lost record only loses the report).
    pub fn write(&self, path: &Path, until_ms: Option<u64>) -> Result<(), String> {
        let body = self.record_json(std::process::id(), until_ms);
        let options = rightkit_fs::AtomicOptions {
            durable: false,
            ..Default::default()
        };
        rightkit_fs::atomic_write_with(path, options, |file| file.write_all(body.as_bytes()))
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn third_exhaustion_inside_the_window_latches() {
        let mut latch = CrashLatch::new();
        assert_eq!(latch.record_exhaustion(1_000).until_ms, None);
        assert_eq!(latch.record_exhaustion(2_000).until_ms, None);
        let third = latch.record_exhaustion(3_000);
        assert_eq!(third.count, 3);
        assert_eq!(third.until_ms, Some(3_000 + COOLDOWN_MS));
        assert_eq!(third.cooldown(), Some(Duration::from_millis(COOLDOWN_MS)));
    }

    #[test]
    fn old_exhaustions_fall_out_of_the_window() {
        let mut latch = CrashLatch::new();
        latch.record_exhaustion(0);
        latch.record_exhaustion(1);
        let late = latch.record_exhaustion(WINDOW_MS + 10);
        assert_eq!(late.count, 1);
        assert_eq!(late.until_ms, None);
    }

    #[test]
    fn clear_reports_whether_anything_was_there() {
        let mut latch = CrashLatch::new();
        assert!(!latch.clear());
        latch.record_exhaustion(5);
        assert!(latch.clear());
        assert!(latch.exhaustions().is_empty());
    }
}
