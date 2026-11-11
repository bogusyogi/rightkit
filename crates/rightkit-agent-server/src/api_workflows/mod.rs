//! REST API and OpenAPI surface for the autonomous workflow control
//! plane (WP-09 — ADR-0021).
//!
//! Every handler enforces the feature gate (`features.workflows_enabled`)
//! from `rightkit_agent_config::FeatureConfig::workflows_enabled()` and emits
//! `WorkflowApiError` on rejection. State-changing endpoints call the
//! harness (WP-03) and let it write through the journal writer; routes
//! never mutate `workflow_*` rows directly.
//!
//! V1 surface covers definitions, versions, triggers, runs, projections,
//! approvals, effects, review inbox, OpenAPI, and journal-backed SSE replay.

#![forbid(unsafe_code)]
#![allow(dead_code)]

pub mod error;
pub mod openapi;
pub mod routes;
pub mod state;

// Public re-exports for downstream callers. Production mounts the workflow
// surface exclusively through `state::build_router` (see
// `main_runtime_server.rs`, which builds a real `WorkflowApiState` and calls
// this); `routes::router`/a state-free deferred constructor are intentionally
// not re-exported here (ra-021) so `state::build_router` remains sole owner
// of the live typed workflow router rather than one of two competing router
// surfaces.
#[allow(unused_imports)]
pub use error::WorkflowApiError;
#[allow(unused_imports)]
pub use openapi::openapi_v1_schema;
#[allow(unused_imports)]
pub use state::{build_router, WorkflowApiState};
