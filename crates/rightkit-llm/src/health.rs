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
//! `Healthy`. [`RouteHealth`] is the per-route state machine on its own, for callers that keep
//! their own state.

use std::collections::BTreeMap;
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

static REGISTRY: Mutex<BTreeMap<String, RouteHealth>> = Mutex::new(BTreeMap::new());

fn with_registry<T>(f: impl FnOnce(&mut BTreeMap<String, RouteHealth>) -> T) -> T {
    let mut g = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut g)
}

/// Record a success for `route` in the process registry.
pub fn record_success(route: &str) {
    with_registry(|r| {
        r.entry(route.to_owned())
            .or_insert_with(|| RouteHealth::new(HealthPolicy::default()))
            .record_success();
    });
}

/// Record a failure for `route` in the process registry, at the current time.
pub fn record_failure(route: &str) {
    record_failure_at(route, Instant::now());
}

/// Record a failure for `route` at `at`. Used by tests and by callers with their own clock.
pub fn record_failure_at(route: &str, at: Instant) {
    with_registry(|r| {
        r.entry(route.to_owned())
            .or_insert_with(|| RouteHealth::new(HealthPolicy::default()))
            .record_failure(at);
    });
}

/// The state of `route` now. A route that has not reported is `Healthy`.
pub fn state(route: &str) -> HealthState {
    state_at(route, Instant::now())
}

/// The state of `route` at `now`.
pub fn state_at(route: &str, now: Instant) -> HealthState {
    with_registry(|r| r.get(route).map_or(HealthState::Healthy, |h| h.state(now)))
}

/// Every route that has reported, sorted by name, with its state now.
pub fn snapshot() -> Vec<RouteSnapshot> {
    let now = Instant::now();
    with_registry(|r| {
        r.iter()
            .map(|(route, h)| RouteSnapshot {
                route: route.clone(),
                state: h.state(now),
                consecutive_failures: h.consecutive_failures(),
            })
            .collect()
    })
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
}
