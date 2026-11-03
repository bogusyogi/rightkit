//! Route health.
//!
//! The Healthy / Degraded / Failed state machine, the recovery window and the
//! process-wide route registry are `crate::health`. This module re-exports
//! the types and keeps one the host app-shaped reader, [`live_route_health`], for
//! the router's triage (`host_router`), which consumes a
//! `HashMap<String, HealthState>`.
//!
//! Publishers call `crate::health::{record_success, record_failure}`. No
//! live path publishes today, so the registry is empty and every route reads as
//! healthy (a route that never reported is `Healthy`).

use std::collections::HashMap;

pub use crate::health::{HealthPolicy, HealthState, RouteHealth};

/// Snapshot the process-wide route health registry for one routing decision.
/// Unknown (or evicted) routes are intentionally absent and treated as healthy.
pub fn live_route_health() -> HashMap<String, HealthState> {
    crate::health::snapshot()
        .into_iter()
        .map(|row| (row.route, row.state))
        .collect()
}
