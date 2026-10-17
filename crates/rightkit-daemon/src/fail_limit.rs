//! Failed-authentication limiter (issue #504), so a caller guessing a per-run secret cannot keep
//! reconnecting to hammer the host. Ported from MailRight's `agent/fail_limit.rs`; the values
//! are the ones that file uses (the issue text says 5 failures in 60 s for both buckets; the
//! source's global bucket is 30, and the constant below keeps the source's value).
//!
//! Two buckets: one per connection (5 failures in 60 s blocks that connection for 60 s) and one
//! across all connections (a caller that reconnects after every refusal still hits it). Both
//! count failed secrets only. The caller consults the limiter only after a secret has failed to
//! verify, through [`FailLimiter::refuses_unverified`]. A correctly authenticated hello or request
//! is never checked here, so a block caused by other callers cannot refuse it or end its
//! connection. The clock is passed in (`now`), never read here, so tests inject time. Bounded: at
//! most [`MAX_ENTRIES`] per-connection entries.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Length of the failure window.
pub const WINDOW: Duration = Duration::from_secs(60);
/// How long a bucket stays blocked once its limit trips.
pub const BLOCK: Duration = Duration::from_secs(60);
/// Failures from one connection inside [`WINDOW`] that block that connection.
pub const CONN_MAX_FAILS: u32 = 5;
/// Failures from all connections inside [`WINDOW`] that block every connection.
pub const GLOBAL_MAX_FAILS: u32 = 30;
/// Most per-connection entries kept at once.
pub const MAX_ENTRIES: usize = 256;

#[derive(Default)]
struct Bucket {
    start: Option<Instant>,
    fails: u32,
    blocked_until: Option<Instant>,
}

impl Bucket {
    fn blocked(&self, now: Instant) -> bool {
        self.blocked_until.is_some_and(|t| now < t)
    }

    fn window_open(&self, now: Instant) -> bool {
        self.start
            .is_some_and(|s| now.saturating_duration_since(s) < WINDOW)
    }

    /// Nothing to remember: not blocked and no failure inside the window.
    fn idle(&self, now: Instant) -> bool {
        !self.blocked(now) && !self.window_open(now)
    }

    fn fail(&mut self, now: Instant, max: u32) {
        if !self.window_open(now) {
            self.start = Some(now);
            self.fails = 0;
        }
        self.fails += 1;
        if self.fails >= max {
            self.blocked_until = Some(now + BLOCK);
            self.start = None;
            self.fails = 0;
        }
    }
}

/// The limiter. One instance per host, shared by its connection threads behind a lock.
#[derive(Default)]
pub struct FailLimiter {
    conns: HashMap<u64, Bucket>,
    global: Bucket,
}

impl FailLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// True when a secret that did not verify from `conn` must be refused at `now`: this
    /// connection is blocked, or all callers together are. Call this only for an attempt whose
    /// secret failed to verify; never for one that verified.
    pub fn refuses_unverified(&self, conn: u64, now: Instant) -> bool {
        self.global.blocked(now) || self.conns.get(&conn).is_some_and(|b| b.blocked(now))
    }

    /// Record one failed authentication (a secret that did not verify) from `conn`.
    pub fn record_failure(&mut self, conn: u64, now: Instant) {
        self.global.fail(now, GLOBAL_MAX_FAILS);
        if !self.conns.contains_key(&conn) && self.conns.len() >= MAX_ENTRIES {
            self.conns.retain(|_, b| !b.idle(now));
            if self.conns.len() >= MAX_ENTRIES {
                // Table full of live entries: the global bucket still counts.
                return;
            }
        }
        self.conns
            .entry(conn)
            .or_default()
            .fail(now, CONN_MAX_FAILS);
    }

    /// A connection closed. Its entry is dropped unless a block is still running.
    pub fn forget(&mut self, conn: u64, now: Instant) {
        if self.conns.get(&conn).is_some_and(|b| b.idle(now)) {
            self.conns.remove(&conn);
        }
    }

    /// Number of per-connection entries held (for tests).
    #[cfg(test)]
    pub fn entries(&self) -> usize {
        self.conns.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connection_is_blocked_after_its_limit_and_not_before() {
        let mut l = FailLimiter::new();
        let t0 = Instant::now();
        for _ in 0..CONN_MAX_FAILS - 1 {
            l.record_failure(1, t0);
        }
        assert!(!l.refuses_unverified(1, t0));
        l.record_failure(1, t0);
        assert!(l.refuses_unverified(1, t0));
        assert!(
            !l.refuses_unverified(2, t0),
            "another connection is not blocked by this one"
        );
    }

    #[test]
    fn the_block_lapses_after_block_duration() {
        let mut l = FailLimiter::new();
        let t0 = Instant::now();
        for _ in 0..CONN_MAX_FAILS {
            l.record_failure(1, t0);
        }
        assert!(l.refuses_unverified(1, t0 + BLOCK - Duration::from_millis(1)));
        assert!(!l.refuses_unverified(1, t0 + BLOCK));
    }

    #[test]
    fn failures_outside_the_window_do_not_add_up() {
        let mut l = FailLimiter::new();
        let t0 = Instant::now();
        for i in 0..(CONN_MAX_FAILS * 3) {
            l.record_failure(1, t0 + WINDOW * i);
        }
        assert!(!l.refuses_unverified(1, t0 + WINDOW * (CONN_MAX_FAILS * 3)));
    }

    #[test]
    fn the_global_bucket_refuses_unverified_attempts_from_every_connection() {
        let mut l = FailLimiter::new();
        let t0 = Instant::now();
        for conn in 0..GLOBAL_MAX_FAILS as u64 {
            l.record_failure(conn, t0);
        }
        assert!(
            l.refuses_unverified(10_000, t0),
            "a fresh connection's failed secret is refused too"
        );
        assert!(!l.refuses_unverified(10_000, t0 + BLOCK));
    }

    #[test]
    fn a_connection_block_does_not_count_toward_the_global_bucket() {
        let mut l = FailLimiter::new();
        let t0 = Instant::now();
        // One connection trips its own bucket: 5 failures, global count only 5 of 30.
        for _ in 0..CONN_MAX_FAILS {
            l.record_failure(1, t0);
        }
        assert!(l.refuses_unverified(1, t0));
        assert!(
            !l.refuses_unverified(2, t0),
            "one peer's bad secrets do not refuse other peers"
        );
    }

    #[test]
    fn forget_drops_an_idle_entry_only() {
        let mut l = FailLimiter::new();
        let t0 = Instant::now();
        l.record_failure(1, t0);
        assert_eq!(l.entries(), 1);
        // Inside the window the entry still counts, so it is kept.
        l.forget(1, t0);
        assert_eq!(l.entries(), 1);
        // Once the window has passed it is idle and goes.
        l.forget(1, t0 + WINDOW);
        assert_eq!(l.entries(), 0);
    }
}
