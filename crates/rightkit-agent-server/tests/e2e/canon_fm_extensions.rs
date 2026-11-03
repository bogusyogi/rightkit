//! Extension runtime journeys relevant to FM-093 and FM-105.

// These are real daemon journeys through plugin injection, recipes/skills,
// command precedence, and lifecycle hook dispatch.
#[path = "e2e_orch_extensions.rs"]
mod extension_journeys;

#[path = "e2e_orch_workflows_schedules.rs"]
mod workflow_journeys;
