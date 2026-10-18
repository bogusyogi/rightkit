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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_failures_degrade_and_five_fail() {
        let t0 = Instant::now();
        let mut h = RouteHealth::new(HealthPolicy::default());
        assert_eq!(h.state(t0), HealthState::Healthy);
        h.record_failure(t0);
        assert_eq!(h.state(t0), HealthState::Healthy);
        h.record_failure(t0);
        assert_eq!(h.state(t0), HealthState::Degraded);
        for _ in 0..3 {
            h.record_failure(t0);
        }
        assert_eq!(h.state(t0), HealthState::Failed);
        assert!(!h.state(t0).is_usable());
        assert_eq!(h.state(t0).weight(), 0.0);
    }

    #[test]
    fn failed_route_recovers_to_degraded_after_window_and_success_heals() {
        let t0 = Instant::now();
        let mut h = RouteHealth::new(HealthPolicy::default());
        for _ in 0..5 {
            h.record_failure(t0);
        }
        assert_eq!(h.state(t0 + Duration::from_secs(59)), HealthState::Failed);
        let later = t0 + Duration::from_secs(60);
        assert_eq!(h.state(later), HealthState::Degraded);
        assert_eq!(HealthState::Degraded.weight(), 0.70);
        h.record_success();
        assert_eq!(h.state(later), HealthState::Healthy);
        assert_eq!(h.consecutive_failures(), 0);
    }

    #[test]
    fn registry_tracks_routes_by_name() {
        let t0 = Instant::now();
        let name = "health-test-route-unique";
        assert_eq!(state_at(name, t0), HealthState::Healthy);
        for _ in 0..5 {
            record_failure_at(name, t0);
        }
        assert_eq!(state_at(name, t0), HealthState::Failed);
        assert!(snapshot()
            .iter()
            .any(|s| s.route == name && s.state == HealthState::Failed));
        record_success(name);
        assert_eq!(state_at(name, t0), HealthState::Healthy);
    }

    #[test]
    fn registry_churn_caps_slots_and_keeps_recent_failure_history() {
        let mut registry = Registry::new();
        let now = Instant::now();
        for i in 0..MAX_ROUTES {
            registry
                .route_mut(&format!("route-{i:04}"), true)
                .unwrap()
                .record_failure(now);
        }
        for _ in 0..4 {
            registry
                .route_mut("route-0000", true)
                .unwrap()
                .record_failure(now);
        }
        // A read also refreshes recency. All other original entries can be evicted.
        for i in MAX_ROUTES..MAX_ROUTES * 4 {
            assert_eq!(registry.state_at("route-0000", now), HealthState::Failed);
            registry
                .route_mut(&format!("route-{i:04}"), true)
                .unwrap()
                .record_success();
            assert_eq!(registry.len, MAX_ROUTES);
            assert!(registry.name_bytes <= MAX_ROUTE_NAME_BYTES);
        }
        assert_eq!(registry.state_at("route-0001", now), HealthState::Healthy);
        assert_eq!(registry.state_at("route-0000", now), HealthState::Failed);
        assert_eq!(
            registry
                .route_mut("route-0000", false)
                .unwrap()
                .consecutive_failures(),
            5
        );
    }

    #[test]
    fn registry_byte_pressure_evicts_oldest_and_bounds_exact_boxed_names() {
        let mut registry = Registry::new();
        let now = Instant::now();
        let first = "a".repeat(MAX_ROUTE_NAME_BYTES / 2);
        let second = "b".repeat(MAX_ROUTE_NAME_BYTES / 2);
        registry
            .route_mut(&first, true)
            .unwrap()
            .record_failure(now);
        registry
            .route_mut(&second, true)
            .unwrap()
            .record_failure(now);
        assert_eq!(registry.name_bytes, MAX_ROUTE_NAME_BYTES);
        registry.route_mut("new", true).unwrap().record_success();
        let rows = registry.snapshot(now);
        assert!(!rows.iter().any(|row| row.route == first));
        assert!(rows.iter().any(|row| row.route == second));
        assert_eq!(registry.len, 2);
        assert_eq!(registry.name_bytes, MAX_ROUTE_NAME_BYTES / 2 + 3);
        assert_eq!(
            registry.name_bytes,
            registry
                .entries
                .iter()
                .flatten()
                .map(|entry| entry.route.len())
                .sum::<usize>()
        );
    }

    #[test]
    fn registry_rejects_oversized_name_without_evicting_or_allocating_an_entry() {
        let mut registry = Registry::new();
        let now = Instant::now();
        let exact = "x".repeat(MAX_ROUTE_NAME_BYTES);
        registry
            .route_mut(&exact, true)
            .unwrap()
            .record_failure(now);
        let oversized = "x".repeat(MAX_ROUTE_NAME_BYTES + 1);
        assert!(registry.route_mut(&oversized, true).is_none());
        assert_eq!(registry.state_at(&oversized, now), HealthState::Healthy);
        assert_eq!(registry.len, 1);
        assert_eq!(registry.name_bytes, MAX_ROUTE_NAME_BYTES);
        assert_eq!(registry.snapshot(now)[0].route, exact);
    }

    #[test]
    fn registry_snapshot_is_sorted_does_not_refresh_recency_and_eviction_resets_state() {
        let mut registry = Registry::new();
        let now = Instant::now();
        for _ in 0..5 {
            registry
                .route_mut("oldest", true)
                .unwrap()
                .record_failure(now);
        }
        for i in 1..MAX_ROUTES {
            registry
                .route_mut(&format!("a-{i:04}"), true)
                .unwrap()
                .record_success();
        }
        let rows = registry.snapshot(now);
        assert!(rows.windows(2).all(|pair| pair[0].route < pair[1].route));
        registry.route_mut("newest", true).unwrap().record_success();
        assert_eq!(registry.state_at("oldest", now), HealthState::Healthy);
        assert!(!registry
            .snapshot(now)
            .iter()
            .any(|row| row.route == "oldest"));
        registry
            .route_mut("oldest", true)
            .unwrap()
            .record_failure(now);
        assert_eq!(
            registry
                .route_mut("oldest", false)
                .unwrap()
                .consecutive_failures(),
            1
        );
    }
}
