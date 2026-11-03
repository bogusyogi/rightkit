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
}
