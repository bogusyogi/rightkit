//! Process-wide limits that keep heavy file-system churn from turning into unbounded work:
//! a shared crawl-thread budget, a coalescing cooldown for full rescans, and a global token
//! bucket for subtree re-lists. Every time-dependent type takes `now` as an argument, so the
//! tests inject a clock and never sleep.

// Some accessors are only used by the tests. Off macOS, lib.rs already allows dead code here.
#![cfg_attr(target_os = "macos", allow(dead_code))]

use std::collections::{BTreeSet, VecDeque};
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

/// The rolling window `WriteBudget` accounts over.
pub const WRITE_WINDOW: Duration = Duration::from_secs(3600);
/// A periodic commit that the write budget holds back is forced through once this long has
/// passed since the last commit, so a burst of changes never stays uncommitted indefinitely.
pub const COMMIT_DEFER_MAX: Duration = Duration::from_secs(600);
/// Oversized rebuilds cannot wait longer than two hours from their first request.
pub const RESCAN_WRITE_DEFER_MAX: Duration = Duration::from_secs(2 * 3600);

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
    requested_at: Option<Instant>,
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
            requested_at: None,
            coalesced: 0,
            runs: 0,
        }
    }

    /// A rescan is wanted and its cooldown has passed. Unlike `try_begin` it changes nothing,
    /// so the caller can still hold the rescan back (`defer_until`) without losing the request.
    pub fn can_begin(&self, now: Instant) -> bool {
        self.dirty && !self.next_allowed.is_some_and(|t| now < t)
    }

    /// Holds the wanted rescan back until `t` (never earlier than the cooldown already set).
    /// The request stays pending; no run is counted and the cooldown does not grow.
    pub fn defer_until(&mut self, t: Instant) {
        self.next_allowed = Some(self.next_allowed.map_or(t, |c| c.max(t)));
    }

    pub fn with_defaults() -> RescanGovernor {
        RescanGovernor::new(
            RESCAN_COOLDOWN_BASE,
            RESCAN_COOLDOWN_MAX,
            RESCAN_QUIET_RESET,
        )
    }

    /// A drop event arrived. While a rescan is already pending this only counts it.
    pub fn requested_at(&self) -> Option<Instant> {
        self.requested_at
    }

    pub fn request(&mut self) {
        self.request_at(Instant::now());
    }

    pub fn request_at(&mut self, now: Instant) {
        if self.dirty {
            self.coalesced += 1;
        } else {
            self.dirty = true;
            self.requested_at = Some(now);
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
        } else {
            self.requested_at = None;
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

// ---------------------------------------------------------------- disk write budget

/// Rolling accounting of the bytes the index writes to its state files, with an optional cap
/// per window (one hour). Every time-dependent method takes `now`, so tests inject a clock.
/// With no cap it only counts.
pub struct WriteBudget {
    max: Option<u64>,
    window: Duration,
    /// `(time, bytes)`, oldest first. Records less than a second apart are merged.
    log: VecDeque<(Instant, u64)>,
    in_window: u64,
    total: u64,
}

impl WriteBudget {
    /// A budget of `max_per_hour` bytes per `WRITE_WINDOW`. `None` or zero: count only.
    pub fn new(max_per_hour: Option<u64>) -> WriteBudget {
        WriteBudget::with_window(max_per_hour, WRITE_WINDOW)
    }

    pub fn with_window(max: Option<u64>, window: Duration) -> WriteBudget {
        WriteBudget {
            max: max.filter(|&m| m > 0),
            window,
            log: VecDeque::new(),
            in_window: 0,
            total: 0,
        }
    }

    fn prune(&mut self, now: Instant) {
        while let Some(&(t, b)) = self.log.front() {
            if now.saturating_duration_since(t) < self.window {
                break;
            }
            self.in_window = self.in_window.saturating_sub(b);
            self.log.pop_front();
        }
    }

    /// Whether a cap is set.
    pub fn is_capped(&self) -> bool {
        self.max.is_some()
    }

    /// The cap per window, if any.
    pub fn max_per_window(&self) -> Option<u64> {
        self.max
    }

    /// Bytes written since the budget was created, whatever the window.
    pub fn total_written(&self) -> u64 {
        self.total
    }

    /// Adds `bytes` written at `now`.
    pub fn record(&mut self, now: Instant, bytes: u64) {
        if bytes == 0 {
            return;
        }
        self.prune(now);
        self.in_window = self.in_window.saturating_add(bytes);
        self.total = self.total.saturating_add(bytes);
        if let Some(last) = self.log.back_mut() {
            if now.saturating_duration_since(last.0) < Duration::from_secs(1) {
                last.1 = last.1.saturating_add(bytes);
                return;
            }
        }
        self.log.push_back((now, bytes));
    }

    /// Bytes written in the window ending at `now`.
    pub fn bytes_in_window(&mut self, now: Instant) -> u64 {
        self.prune(now);
        self.in_window
    }

    /// Bytes still available in the window; `None` without a cap.
    pub fn headroom(&mut self, now: Instant) -> Option<u64> {
        let used = self.bytes_in_window(now);
        self.max.map(|m| m.saturating_sub(used))
    }

    /// Whether writing `bytes` more now would pass the cap. Always false without a cap.
    pub fn would_exceed(&mut self, now: Instant, bytes: u64) -> bool {
        let used = self.bytes_in_window(now);
        self.max.is_some_and(|m| used.saturating_add(bytes) > m)
    }

    /// The earliest time at which `bytes` fit under the cap, as old records age out of the
    /// window; `now` when they fit already (and always without a cap). A request larger than
    /// the whole cap waits until the window is empty and is then allowed: it could never fit,
    /// and refusing it forever would starve the work.
    pub fn fits_at(&mut self, now: Instant, bytes: u64) -> Instant {
        let Some(max) = self.max else {
            return now;
        };
        self.prune(now);
        let need = bytes.min(max);
        let mut used = self.in_window;
        if used.saturating_add(need) <= max {
            return now;
        }
        for &(t, b) in &self.log {
            used = used.saturating_sub(b);
            if used.saturating_add(need) <= max {
                return t + self.window;
            }
        }
        now
    }

    /// Oversized rebuilds run once usage falls to one quarter of the cap, or two hours
    /// after the first request, whichever happens first. New writes never reset that deadline.
    pub fn rescan_fits_at(&mut self, now: Instant, bytes: u64, requested: Instant) -> Instant {
        let Some(max) = self.max.filter(|max| bytes > *max) else {
            return self.fits_at(now, bytes);
        };
        self.prune(now);
        let deadline = requested + RESCAN_WRITE_DEFER_MAX;
        if now >= deadline || self.in_window <= max / 4 {
            return now;
        }
        let mut used = self.in_window;
        for &(at, bytes) in &self.log {
            used = used.saturating_sub(bytes);
            if used <= max / 4 {
                return (at + self.window).min(deadline);
            }
        }
        deadline
    }

    /// Whether a periodic commit of `pending` bytes should wait: it would pass the cap and
    /// the last commit is less than `COMMIT_DEFER_MAX` ago.
    pub fn defer_commit(
        &mut self,
        now: Instant,
        pending: u64,
        since_last_commit: Duration,
    ) -> bool {
        since_last_commit < COMMIT_DEFER_MAX && self.would_exceed(now, pending)
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
    /// False: no limit, `drain` returns everything pending.
    enabled: bool,
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
            enabled: true,
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

    /// Builds the budget from `Config::relist_budget`: `None`, a zero burst or a zero interval
    /// means unlimited.
    pub fn from_config(cfg: Option<(u32, Duration)>, now: Instant) -> RelistBudget {
        match cfg {
            Some((burst, interval)) if burst > 0 && !interval.is_zero() => {
                RelistBudget::new(burst, interval, now)
            }
            _ => RelistBudget {
                enabled: false,
                ..RelistBudget::new(1, Duration::from_secs(1), now)
            },
        }
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
        if !self.enabled {
            return std::mem::take(&mut self.pending).into_iter().collect();
        }
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
