//! Process-wide limits that keep heavy file-system churn from turning into unbounded work:
//! a shared crawl-thread budget, a coalescing cooldown for full rescans, and a global token
//! bucket for subtree re-lists. Every time-dependent type takes `now` as an argument, so the
//! tests inject a clock and never sleep.

#![allow(dead_code)] // Some accessors are only used by the tests or by one platform.

use std::collections::BTreeSet;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Hard ceiling on crawl worker threads, per call and across the whole process.
pub const MAX_CRAWL_THREADS: usize = 8;

/// Cooldown after the first full rescan; doubles on each further rescan up to the maximum.
pub const RESCAN_COOLDOWN_BASE: Duration = Duration::from_secs(30);
/// Largest cooldown between two full rescans.
pub const RESCAN_COOLDOWN_MAX: Duration = Duration::from_secs(600);
/// A rescan this long after the previous one starts again from the base cooldown.
pub const RESCAN_QUIET_RESET: Duration = Duration::from_secs(30 * 60);

/// Subtree re-lists allowed in a burst across all folders.
pub const RELIST_BUDGET_BURST: u32 = 8;
/// One more subtree re-list is allowed this often once the burst is spent.
pub const RELIST_BUDGET_INTERVAL: Duration = Duration::from_secs(5);

/// Clamps a configured crawl thread count to `1..=MAX_CRAWL_THREADS`.
pub fn clamp_crawl_threads(n: usize) -> usize {
    n.clamp(1, MAX_CRAWL_THREADS)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------- shared crawl pool

/// A counting budget of worker threads. Crawls take permits before they spawn workers and
/// queue when none are free, so concurrent crawls never exceed the capacity together.
pub struct Permits {
    free: Mutex<usize>,
    cv: Condvar,
    cap: usize,
}

pub struct PermitGuard<'a> {
    pool: &'a Permits,
    n: usize,
}

impl Permits {
    pub const fn new(cap: usize) -> Permits {
        Permits {
            free: Mutex::new(cap),
            cv: Condvar::new(),
            cap,
        }
    }

    /// Takes up to `want` permits (at least one), blocking while none are free.
    pub fn acquire(&self, want: usize) -> PermitGuard<'_> {
        let want = want.clamp(1, self.cap.max(1));
        let mut free = lock(&self.free);
        while *free == 0 {
            free = self.cv.wait(free).unwrap_or_else(|e| e.into_inner());
        }
        let n = want.min(*free);
        *free -= n;
        PermitGuard { pool: self, n }
    }

    /// Like `acquire`, but returns `None` instead of blocking.
    pub fn try_acquire(&self, want: usize) -> Option<PermitGuard<'_>> {
        let want = want.clamp(1, self.cap.max(1));
        let mut free = lock(&self.free);
        if *free == 0 {
            return None;
        }
        let n = want.min(*free);
        *free -= n;
        Some(PermitGuard { pool: self, n })
    }

    pub fn available(&self) -> usize {
        *lock(&self.free)
    }
}

impl PermitGuard<'_> {
    /// Number of permits (worker threads) held.
    pub fn count(&self) -> usize {
        self.n
    }
}

impl Drop for PermitGuard<'_> {
    fn drop(&mut self) {
        *lock(&self.pool.free) += self.n;
        self.pool.cv.notify_all();
    }
}

static CRAWL_PERMITS: Permits = Permits::new(MAX_CRAWL_THREADS);

/// Takes crawl worker threads from the process-wide pool. The request is clamped first, so
/// the result is at most `MAX_CRAWL_THREADS` however the caller configured it.
pub fn acquire_crawl_threads(want: usize) -> PermitGuard<'static> {
    CRAWL_PERMITS.acquire(clamp_crawl_threads(want))
}

// ---------------------------------------------------------------- full rescan governor

/// Coalesces full rescans. Drop events only mark a rescan dirty; the rescan runs when the
/// cooldown allows, and one run answers every request that arrived before it. After each run
/// the cooldown doubles (base to max) until a quiet period resets it.
pub struct RescanGovernor {
    base: Duration,
    max: Duration,
    quiet: Duration,
    /// Cooldown that the next finished run will apply.
    backoff: Duration,
    /// Cooldown applied after the last run (zero before any).
    applied: Duration,
    next_allowed: Option<Instant>,
    last_finish: Option<Instant>,
    dirty: bool,
    coalesced: u64,
    runs: u64,
}

impl RescanGovernor {
    pub fn new(base: Duration, max: Duration, quiet: Duration) -> RescanGovernor {
        RescanGovernor {
            base,
            max: max.max(base),
            quiet,
            backoff: base,
            applied: Duration::ZERO,
            next_allowed: None,
            last_finish: None,
            dirty: false,
            coalesced: 0,
            runs: 0,
        }
    }

    pub fn with_defaults() -> RescanGovernor {
        RescanGovernor::new(
            RESCAN_COOLDOWN_BASE,
            RESCAN_COOLDOWN_MAX,
            RESCAN_QUIET_RESET,
        )
    }

    /// A drop event arrived. While a rescan is already pending this only counts it.
    pub fn request(&mut self) {
        if self.dirty {
            self.coalesced += 1;
        } else {
            self.dirty = true;
        }
    }

    /// Whether a rescan is wanted and the cooldown has passed. When true the caller must run
    /// it and then call `finish`.
    pub fn try_begin(&mut self, now: Instant) -> bool {
        if !self.dirty || self.next_allowed.is_some_and(|t| now < t) {
            return false;
        }
        self.dirty = false;
        self.runs += 1;
        if self
            .last_finish
            .is_some_and(|t| now.saturating_duration_since(t) >= self.quiet)
        {
            self.backoff = self.base;
        }
        true
    }

    /// The rescan started by `try_begin` ended. A failed one stays wanted and is retried
    /// after the cooldown.
    pub fn finish(&mut self, now: Instant, ok: bool) {
        self.applied = self.backoff;
        self.next_allowed = Some(now + self.applied);
        self.backoff = (self.backoff * 2).min(self.max);
        self.last_finish = Some(now);
        if !ok {
            self.dirty = true;
        }
    }

    /// A rescan is wanted but waiting for its cooldown (or not yet run).
    pub fn pending(&self) -> bool {
        self.dirty
    }

    /// When a pending rescan may run.
    pub fn next_due(&self) -> Option<Instant> {
        if self.dirty {
            self.next_allowed
        } else {
            None
        }
    }

    /// Requests absorbed into an already pending rescan.
    pub fn coalesced(&self) -> u64 {
        self.coalesced
    }

    /// Rescans started.
    pub fn runs(&self) -> u64 {
        self.runs
    }

    /// Cooldown applied after the last rescan, in milliseconds (0 before any).
    pub fn applied_ms(&self) -> u64 {
        self.applied.as_millis() as u64
    }
}

// ---------------------------------------------------------------- subtree re-list budget

fn within(path: &[u8], ancestor: &[u8]) -> bool {
    path.starts_with(ancestor)
        && (path.len() == ancestor.len() || ancestor == b"/" || path[ancestor.len()] == b'/')
}

/// A global token bucket for subtree re-lists. Requests beyond the budget merge into a pending
/// set that drains as tokens return; nothing is dropped. A pending folder absorbs pending
/// folders below it, since re-listing the folder covers them.
pub struct RelistBudget {
    burst: f64,
    interval: Duration,
    tokens: f64,
    last: Instant,
    pending: BTreeSet<Vec<u8>>,
    deferred: u64,
}

impl RelistBudget {
    pub fn new(burst: u32, interval: Duration, now: Instant) -> RelistBudget {
        RelistBudget {
            burst: f64::from(burst.max(1)),
            interval: interval.max(Duration::from_millis(1)),
            tokens: f64::from(burst.max(1)),
            last: now,
            pending: BTreeSet::new(),
            deferred: 0,
        }
    }

    pub fn with_defaults(now: Instant) -> RelistBudget {
        RelistBudget::new(RELIST_BUDGET_BURST, RELIST_BUDGET_INTERVAL, now)
    }

    fn refill(&mut self, now: Instant) {
        if now > self.last {
            let dt = now.duration_since(self.last).as_secs_f64();
            self.tokens = (self.tokens + dt / self.interval.as_secs_f64()).min(self.burst);
            self.last = now;
        }
    }

    /// Queues a subtree for re-listing.
    pub fn submit(&mut self, path: Vec<u8>) {
        if self.pending.iter().any(|p| within(&path, p)) {
            return;
        }
        self.pending.retain(|p| !within(p, &path));
        self.pending.insert(path);
    }

    /// Subtrees that fit the budget now, parents before children. The rest stay pending.
    pub fn drain(&mut self, now: Instant) -> Vec<Vec<u8>> {
        self.refill(now);
        let mut out = Vec::new();
        while self.tokens + 1e-6 >= 1.0 {
            let Some(p) = self.pending.pop_first() else {
                break;
            };
            self.tokens = (self.tokens - 1.0).max(0.0);
            out.push(p);
        }
        self.deferred += self.pending.len() as u64;
        out
    }

    /// Forgets pending subtrees. Only valid when something else has re-listed everything
    /// (a completed full rescan).
    pub fn clear_pending(&mut self) {
        self.pending.clear();
    }

    /// Subtrees waiting for a token.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// Times a pending subtree was left waiting by a `drain` (a count of deferrals).
    pub fn deferred_total(&self) -> u64 {
        self.deferred
    }

    /// When the next pending subtree can run, if any is waiting.
    pub fn next_due(&self) -> Option<Instant> {
        if self.pending.is_empty() {
            return None;
        }
        if self.tokens + 1e-6 >= 1.0 {
            return Some(self.last);
        }
        Some(self.last + self.interval.mul_f64(1.0 - self.tokens))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn the_clamp_holds() {
        assert_eq!(clamp_crawl_threads(0), 1);
        assert_eq!(clamp_crawl_threads(1), 1);
        assert_eq!(clamp_crawl_threads(8), 8);
        assert_eq!(clamp_crawl_threads(9), 8);
        assert_eq!(clamp_crawl_threads(usize::MAX), MAX_CRAWL_THREADS);
        let g = acquire_crawl_threads(1000);
        assert!(g.count() <= MAX_CRAWL_THREADS);
    }

    #[test]
    fn concurrent_crawls_share_one_budget() {
        let p = Permits::new(8);
        let a = p.try_acquire(8).unwrap();
        assert_eq!(a.count(), 8);
        assert_eq!(p.available(), 0);
        assert!(p.try_acquire(8).is_none(), "a second crawl queues");
        drop(a);
        let b = p.try_acquire(5).unwrap();
        let c = p.try_acquire(8).unwrap();
        assert_eq!(b.count() + c.count(), 8, "never more than the capacity");
        assert!(p.try_acquire(1).is_none());
        drop((b, c));
        assert_eq!(p.available(), 8);
    }

    #[test]
    fn a_dropped_event_storm_runs_at_most_one_rescan_per_cooldown() {
        let t0 = Instant::now();
        let mut g = RescanGovernor::with_defaults();
        let mut runs = 0u32;
        // A drop event every 100 ms for 10 minutes.
        for i in 0..6000u64 {
            let now = t0 + Duration::from_millis(i * 100);
            g.request();
            if g.try_begin(now) {
                runs += 1;
                g.finish(now, true);
            }
        }
        // The cooldown doubles 30, 60, 120, 240, 480 s: runs at 0, 30, 90, 210, 450 s.
        assert_eq!(runs, 5, "runs={runs}");
        assert_eq!(g.runs(), 5);
        assert!(g.coalesced() >= 5990, "coalesced={}", g.coalesced());
        assert_eq!(g.applied_ms(), 480_000);
    }

    #[test]
    fn requests_inside_the_cooldown_only_mark_dirty() {
        let t0 = Instant::now();
        let mut g = RescanGovernor::with_defaults();
        g.request();
        assert!(g.try_begin(t0));
        g.finish(t0, true);
        assert!(!g.pending());
        g.request();
        g.request();
        g.request();
        assert!(g.pending());
        assert!(!g.try_begin(t0 + s(29)));
        assert_eq!(g.next_due(), Some(t0 + s(30)));
        assert!(g.try_begin(t0 + s(30)));
        g.finish(t0 + s(30), true);
        assert!(!g.pending(), "one run answered every request");
        assert_eq!(g.coalesced(), 2);
        assert_eq!(g.applied_ms(), 30_000);
    }

    #[test]
    fn the_cooldown_caps_and_resets_after_a_quiet_period() {
        let t0 = Instant::now();
        let mut g = RescanGovernor::with_defaults();
        let mut now = t0;
        for _ in 0..12 {
            g.request();
            assert!(g.try_begin(now));
            g.finish(now, true);
            now += Duration::from_millis(g.applied_ms());
        }
        assert_eq!(g.applied_ms(), 600_000, "capped at ten minutes");
        // Quiet for longer than the reset window: back to the base cooldown.
        now += RESCAN_QUIET_RESET;
        g.request();
        assert!(g.try_begin(now));
        g.finish(now, true);
        assert_eq!(g.applied_ms(), 30_000);
    }

    #[test]
    fn a_failed_rescan_is_retried_after_the_cooldown() {
        let t0 = Instant::now();
        let mut g = RescanGovernor::with_defaults();
        g.request();
        assert!(g.try_begin(t0));
        g.finish(t0, false);
        assert!(g.pending());
        assert!(!g.try_begin(t0 + s(1)));
        assert!(g.try_begin(t0 + s(30)));
    }

    #[test]
    fn the_relist_budget_defers_and_never_drops() {
        let t0 = Instant::now();
        let mut b = RelistBudget::new(2, s(10), t0);
        for i in 0..5 {
            b.submit(format!("/r/d{i}").into_bytes());
        }
        let first = b.drain(t0);
        assert_eq!(first.len(), 2, "the burst is spent");
        assert_eq!(b.pending(), 3);
        assert!(b.drain(t0 + s(9)).is_empty());
        let due = b.next_due().unwrap();
        assert!(due >= t0 + s(10) - Duration::from_millis(1));
        assert!(due <= t0 + s(10) + Duration::from_millis(1));
        assert_eq!(b.drain(t0 + s(10)).len(), 1);
        assert_eq!(b.drain(t0 + s(20)).len(), 1);
        assert_eq!(b.drain(t0 + s(30)).len(), 1);
        assert_eq!(b.pending(), 0);
        assert!(b.next_due().is_none());
        assert!(b.deferred_total() > 0);
    }

    #[test]
    fn every_submitted_subtree_is_eventually_relisted() {
        let t0 = Instant::now();
        let mut b = RelistBudget::new(3, s(5), t0);
        let mut seen: Vec<Vec<u8>> = Vec::new();
        let mut now = t0;
        for i in 0..50 {
            b.submit(format!("/vol/dir{i}").into_bytes());
            seen.extend(b.drain(now));
            now += s(1);
        }
        while b.pending() > 0 {
            now = b.next_due().unwrap();
            seen.extend(b.drain(now));
        }
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 50);
    }

    #[test]
    fn a_pending_parent_absorbs_children_without_losing_coverage() {
        let t0 = Instant::now();
        let mut b = RelistBudget::new(1, s(10), t0);
        b.submit(b"/a/b/c".to_vec());
        b.submit(b"/a/b".to_vec());
        b.submit(b"/a/b/d".to_vec());
        b.submit(b"/a/bc".to_vec());
        assert_eq!(b.pending(), 2);
        assert_eq!(b.drain(t0), vec![b"/a/b".to_vec()]);
        assert_eq!(b.drain(t0 + s(10)), vec![b"/a/bc".to_vec()]);
    }
}
