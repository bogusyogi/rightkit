//! Public route health with a recovery window and a snapshot registry
//! (Orthic-Labs/rightkit#660).
//!
//! Each route is `Healthy`, `Degraded` or `Failed`. Two consecutive failures make it
//! `Degraded`; five make it `Failed`. A `Failed` route becomes `Degraded` again once
//! [`HealthPolicy::recovery`] has passed since its last failure, so it gets a trial call. Any
//! success returns a route to `Healthy`. A router weights routes with [`HealthState::weight`]
//! (1.0, 0.70 and 0.0) and skips a route whose [`HealthState::is_usable`] is `false`.
//!
//! The registry is process-wide and keyed by route name: [`record_success`],
//! [`record_failure`], [`state`] and [`snapshot`]. A route that has never reported is
//! `Healthy`. It retains at most 256 routes and 32 KiB of route-name bytes. Recording an
//! outcome or reading a route's state makes it most recently used; [`snapshot`] does not.
//! New entries evict least recently used routes until both limits fit. An evicted route is
//! `Healthy` until it reports again; a name longer than 32 KiB is never retained. Snapshots
//! contain only retained routes. [`RouteHealth`] is the lossless per-route state machine for
//! callers that keep their own state. This registry does not own the independent breakers
//! inside [`crate::failover::FailoverChain`].

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Thresholds for the route state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HealthPolicy {
    /// Consecutive failures that make a route `Degraded`. Default 2.
    pub degraded_after: u32,
    /// Consecutive failures that make a route `Failed`. Default 5.
    pub failed_after: u32,
    /// Time after the last failure before a `Failed` route is retried (shown as `Degraded`).
    /// Default 60 s.
    pub recovery: Duration,
}

impl Default for HealthPolicy {
    fn default() -> Self {
        Self {
            degraded_after: 2,
            failed_after: 5,
            recovery: Duration::from_secs(60),
        }
    }
}

/// The health of one route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthState {
    Healthy,
    Degraded,
    Failed,
}

impl HealthState {
    /// Routing weight: `Healthy` 1.0, `Degraded` 0.70, `Failed` 0.0.
    pub fn weight(self) -> f64 {
        match self {
            HealthState::Healthy => 1.0,
            HealthState::Degraded => 0.70,
            HealthState::Failed => 0.0,
        }
    }

    /// `false` only for `Failed`.
    pub fn is_usable(self) -> bool {
        self != HealthState::Failed
    }
}

/// Health of one route. Feed it outcomes with [`RouteHealth::record_success`] and
/// [`RouteHealth::record_failure`], passing the time of the outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteHealth {
    policy: HealthPolicy,
    consecutive_failures: u32,
    last_failure: Option<Instant>,
}

impl RouteHealth {
    pub fn new(policy: HealthPolicy) -> Self {
        Self {
            policy,
            consecutive_failures: 0,
            last_failure: None,
        }
    }

    /// Consecutive failures since the last success.
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.last_failure = None;
    }

    pub fn record_failure(&mut self, at: Instant) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.last_failure = Some(at);
    }

    /// The state at time `now`.
    pub fn state(&self, now: Instant) -> HealthState {
        let n = self.consecutive_failures;
        if n == 0 {
            return HealthState::Healthy;
        }
        if n >= self.policy.failed_after {
            let recovered = self
                .last_failure
                .is_some_and(|t| now.duration_since(t) >= self.policy.recovery);
            return if recovered {
                HealthState::Degraded
            } else {
                HealthState::Failed
            };
        }
        if n >= self.policy.degraded_after {
            HealthState::Degraded
        } else {
            HealthState::Healthy
        }
    }
}

/// One row of [`snapshot`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteSnapshot {
    pub route: String,
    pub state: HealthState,
    pub consecutive_failures: u32,
}

// Owned-layout design budget, not an RSS measurement: 64 KiB, matching the existing
// CLI stderr/default tool-result cap (provider/cli.rs, tools.rs). Reserve half
// for names and half for fixed slots. 256 slots exceed ten times the 23 bundled
// catalogue model rows; this is a cardinality policy, not observed route usage.
const MAX_ROUTES: usize = 256;
const MAX_ROUTE_NAME_BYTES: usize = 32 * 1024;
const MAX_REGISTRY_INLINE_BYTES: usize = 32 * 1024;

struct RegistryEntry {
    // Box<str> retains precisely len bytes, without String capacity overhang.
    route: Box<str>,
    health: RouteHealth,
}

struct Registry {
    // Packed oldest to newest: moving a slot updates LRU order without a counter
    // that can wrap, a second allocation, or a duplicate copy of a route name.
    entries: [Option<RegistryEntry>; MAX_ROUTES],
    len: usize,
    name_bytes: usize,
}

// Includes the mutex and array layout. Allocator bookkeeping is not an RSS claim;
// there are at most MAX_ROUTES owned heap allocations, each an exact Box<str>.
const _: () = assert!(std::mem::size_of::<Mutex<Registry>>() <= MAX_REGISTRY_INLINE_BYTES);

impl Registry {
    const fn new() -> Self {
        Self {
            entries: [const { None }; MAX_ROUTES],
            len: 0,
            name_bytes: 0,
        }
    }

    fn route_mut(&mut self, route: &str, insert: bool) -> Option<&mut RouteHealth> {
        if let Some(index) = self.entries[..self.len].iter().position(|entry| {
            entry
                .as_ref()
                .is_some_and(|entry| entry.route.as_ref() == route)
        }) {
            self.entries[index..self.len].rotate_left(1);
        } else {
            if !insert || route.len() > MAX_ROUTE_NAME_BYTES {
                return None;
            }
            while self.len == MAX_ROUTES || self.name_bytes > MAX_ROUTE_NAME_BYTES - route.len() {
                let oldest = self.entries[0].take().expect("nonempty registry");
                self.name_bytes -= oldest.route.len();
                self.entries[..self.len].rotate_left(1);
                self.len -= 1;
            }
            self.entries[self.len] = Some(RegistryEntry {
                route: route.into(),
                health: RouteHealth::new(HealthPolicy::default()),
            });
            self.len += 1;
            self.name_bytes += route.len();
        }
        self.entries[self.len - 1]
            .as_mut()
            .map(|entry| &mut entry.health)
    }

    fn state_at(&mut self, route: &str, now: Instant) -> HealthState {
        self.route_mut(route, false)
            .map_or(HealthState::Healthy, |health| health.state(now))
    }

    fn snapshot(&self, now: Instant) -> Vec<RouteSnapshot> {
        let mut rows: Vec<_> = self.entries[..self.len]
            .iter()
            .filter_map(Option::as_ref)
            .map(|entry| RouteSnapshot {
                route: entry.route.to_string(),
                state: entry.health.state(now),
                consecutive_failures: entry.health.consecutive_failures(),
            })
            .collect();
        rows.sort_unstable_by(|left, right| left.route.cmp(&right.route));
        rows
    }
}

static REGISTRY: Mutex<Registry> = Mutex::new(Registry::new());

fn with_registry<T>(f: impl FnOnce(&mut Registry) -> T) -> T {
    let mut g = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut g)
}

/// Record a success for `route` in the bounded process registry.
/// Oversized names are ignored; inserting a new route may evict older observations.
pub fn record_success(route: &str) {
    with_registry(|r| {
        if let Some(health) = r.route_mut(route, true) {
            health.record_success();
        }
    });
}

/// Record a failure for `route` in the process registry, at the current time.
pub fn record_failure(route: &str) {
    record_failure_at(route, Instant::now());
}

/// Record a failure for `route` at `at`. Used by tests and by callers with their own clock.
/// Oversized names are ignored; inserting a new route may evict older observations.
pub fn record_failure_at(route: &str, at: Instant) {
    with_registry(|r| {
        if let Some(health) = r.route_mut(route, true) {
            health.record_failure(at);
        }
    });
}

/// The state of `route` now. A route that has not reported is `Healthy`.
pub fn state(route: &str) -> HealthState {
    state_at(route, Instant::now())
}

/// The state of `route` at `now`.
pub fn state_at(route: &str, now: Instant) -> HealthState {
    with_registry(|r| r.state_at(route, now))
}

/// Currently retained routes, sorted by name, with their state now.
/// Does not change recency. Evicted and oversized names are absent.
pub fn snapshot() -> Vec<RouteSnapshot> {
    let now = Instant::now();
    with_registry(|r| r.snapshot(now))
}
