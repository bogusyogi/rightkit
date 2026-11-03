//! Session handlers — all families (Tasks A–E): lifecycle, message/steering, SSE,
//! approvals/compaction, specs/orchestration/agents/taskgraph/tasks.
//! Extracted from api.rs (2026-06-21) using the established slice pattern:
//! a `sessions_routes()` free function merged into `build_router`. Behaviour
//! is unchanged — handler bodies moved verbatim into bounded modules under
//! `api_sessions/**`. Built incrementally:
//! Task A (lifecycle) → B (messages) → C (SSE) → D (approvals/compaction) → E (specs).

pub mod approvals;
pub mod credentials;
pub mod elicitation;
pub mod export;
mod idempotency;
pub mod lifecycle;
pub mod messages;
pub mod orchestration;
pub mod rewind;
pub mod router;
pub mod sse;
pub mod state;
pub mod validation;

#[path = "blueprint_init.rs"]
pub(crate) mod blueprint_init;

pub(crate) use router::sessions_routes;

// Re-export handler types for external callers that imported via `api_sessions::*`
