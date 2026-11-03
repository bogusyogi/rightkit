//! WP-10 — MCP Tasks mapping for the autonomous workflow control plane.
//!
//! ADR-0021 constraint: every external interface that observes a workflow
//! run MUST be a **read-only projection**. External clients (MCP, A2A,
//! agent UIs) may never widen authority, alter lifecycle semantics, or
//! short-circuit policy/approvals/effects.
//!
//! This module is the boundary between the durable workflow journal
//! and the MCP Tasks surface. It performs three jobs:
//!
//! 1. Project an internal run row onto the closed [`McpTaskView`] shape
//!    that MCP clients can consume.
//! 2. Map the internal [`LifecyclePhase`] taxonomy onto the closed
//!    [`McpTaskStatus`] set with deterministic, total semantics — every
//!    internal phase maps to exactly one MCP status, and every MCP
//!    status has at least one source phase.
//! 3. Reject any external "create task" request that attempts to skip
//!    policy, approvals, or effects. The MCP layer is allowed to
//!    *trigger* a run; it is never allowed to *bypass* it.
//!
//! All functions are pure: no I/O, no clock, no global state.
#![allow(clippy::module_name_repetitions)]

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::control_plane::domain::LifecyclePhase;

// ─── Row projection ──────────────────────────────────────────────────────────

/// Read-only projection of a workflow run row used for external
/// (MCP / A2A / agent UI) task observation.
///
/// The crate-local definition mirrors only the columns of the durable
/// `workflow_runs` row that an external observer is allowed to see.
/// Callers at the daemon boundary (WP-03 / WP-09) build one of these
/// from `storage::WorkflowRunRow`; the pure-domain functions in this
/// module consume the projection and never the raw row.
///
/// This is **not** the same struct as `rightkit_agent_engine_storage::WorkflowRunRow`.
/// Re-using that struct would either drag a SQLite dependency into the
/// pure-domain crate, or push the projection behind a trait object —
/// both of which violate WP-00's "no driver_host/SQLite dep" rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowRunRow {
    /// Run id (`wfr_*`). Mirrors `workflow_runs.id`.
    pub id: String,
    /// Workflow definition id (`wfd_*`). Mirrors `workflow_runs.workflow_id`.
    pub workflow_id: String,
    /// Workflow version id (`wfv_*`). Mirrors `workflow_runs.workflow_version_id`.
    pub workflow_version_id: String,
    /// Lifecycle phase as the durable string (`pending` | `running` |
    /// `suspended` | `cancelled` | `succeeded` | `failed` | `degraded`).
    /// Stored as a string to keep forward compatibility with new
    /// phases that may be added by a later WP without forcing a
    /// storage migration.
    pub lifecycle_phase: String,
    /// Optimistic revision. Mirrors `workflow_runs.lifecycle_revision`.
    pub lifecycle_revision: i64,
    /// Run start instant as Unix milliseconds. Mirrors the parsed
    /// `workflow_runs.started_at` RFC3339 timestamp.
    pub started_at_unix_ms: i64,
    /// Last journal-derived update instant as Unix milliseconds. The
    /// daemon derives this from the most recent `workflow_journal` row.
    pub last_updated_at_unix_ms: i64,
    /// Terminal instant as Unix milliseconds when the run reached a
    /// terminal phase; `None` otherwise.
    pub terminal_at_unix_ms: Option<i64>,
    /// Display title for the MCP view. The daemon composes this from
    /// the workflow name + run id short suffix.
    pub title: String,
}

impl WorkflowRunRow {
    /// Convenience constructor with sensible defaults for fields the
    /// caller may not have. Useful in tests.
    pub fn for_test(id: &str, lifecycle_phase: &str) -> Self {
        Self {
            id: id.to_string(),
            workflow_id: "wfd_test".to_string(),
            workflow_version_id: "wfv_test".to_string(),
            lifecycle_phase: lifecycle_phase.to_string(),
            lifecycle_revision: 0,
            started_at_unix_ms: 0,
            last_updated_at_unix_ms: 0,
            terminal_at_unix_ms: None,
            title: format!("workflow run {id}"),
        }
    }
}

// ─── MCP Tasks types ─────────────────────────────────────────────────────────

/// The closed set of MCP task statuses. Mirrors the v1 MCP Tasks
/// spec subset that the control plane actually uses; any status not
/// in this enum is rejected at the boundary.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum McpTaskStatus {
    /// Run is admitted and at least one node is in-flight (or about
    /// to be). Equivalent to internal `Pending` or `Running` or
    /// `Degraded`.
    Working,
    /// Run is suspended waiting for an external input (approval,
    /// wait-event payload, or wait-timer expiry + external resume).
    InputRequired,
    /// Run reached `Succeeded`. Terminal.
    Completed,
    /// Run reached `Failed`. Terminal.
    Failed,
    /// Run reached `Cancelled`. Terminal.
    Cancelled,
}

impl McpTaskStatus {
    /// Stable lowercase identifier for logging, JSON envelopes, and
    /// the MCP wire format.
    pub const fn as_str(&self) -> &'static str {
        match self {
            McpTaskStatus::Working => "working",
            McpTaskStatus::InputRequired => "input_required",
            McpTaskStatus::Completed => "completed",
            McpTaskStatus::Failed => "failed",
            McpTaskStatus::Cancelled => "cancelled",
        }
    }
}

/// Read-only task view exposed via MCP Tasks. Stable on the wire; the
/// pure-domain functions in this module produce it from [`WorkflowRunRow`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpTaskView {
    /// Stable external task id. Equal to the run id (`wfr_*`) so
    /// MCP clients can correlate without a separate mapping table.
    pub task_id: String,
    /// Current mapped MCP status.
    pub status: McpTaskStatus,
    /// Source run id (same as `task_id` today, but kept as a separate
    /// field so a future rename of `task_id` does not silently break
    /// clients that key off `run_id`).
    pub run_id: String,
    /// Display title. Human-readable; not part of the canonical
    /// identity.
    pub title: String,
    /// Run start instant as Unix milliseconds.
    pub created_at_unix_ms: i64,
    /// Last journal-derived update instant as Unix milliseconds.
    pub last_updated_at_unix_ms: i64,
}

// ─── Mapping ─────────────────────────────────────────────────────────────────

/// Deterministic, total mapping from the internal lifecycle phase to
/// the closed MCP task status set.
///
/// The mapping is intentionally **non-injective on the Working side**
/// (`Pending`, `Running`, and `Degraded` all collapse to `Working`):
/// MCP clients do not need to distinguish "admitted but not started"
/// from "in flight" from "degraded but still live". The internal
/// reducer still keeps the three phases distinct.
pub fn map_status(internal: &LifecyclePhase) -> McpTaskStatus {
    match internal {
        LifecyclePhase::Pending | LifecyclePhase::Running | LifecyclePhase::Degraded => {
            McpTaskStatus::Working
        }
        LifecyclePhase::Suspended => McpTaskStatus::InputRequired,
        LifecyclePhase::Succeeded => McpTaskStatus::Completed,
        LifecyclePhase::Failed => McpTaskStatus::Failed,
        LifecyclePhase::Cancelled => McpTaskStatus::Cancelled,
    }
}

/// Parse the durable `lifecycle_phase` string into the typed
/// [`LifecyclePhase`]. The string set is closed by the storage
/// writer (see `WorkflowJournalWriter` in `rightkit_agent_engine_storage`); any
/// unrecognized value is treated as `Pending` so a future storage
/// migration cannot silently widen the projection surface.
///
/// Callers that need to distinguish "unknown string" from "actually
/// pending" should pre-validate against the storage writer's enum
/// table; this helper's job is to be total.
fn parse_lifecycle_phase(s: &str) -> LifecyclePhase {
    match s {
        "pending" => LifecyclePhase::Pending,
        "running" => LifecyclePhase::Running,
        "suspended" => LifecyclePhase::Suspended,
        "cancelled" => LifecyclePhase::Cancelled,
        "succeeded" => LifecyclePhase::Succeeded,
        "failed" => LifecyclePhase::Failed,
        "degraded" => LifecyclePhase::Degraded,
        // Forward-compatible default: unknown strings are reported as
        // Pending so a new internal phase added by a later WP cannot
        // accidentally be projected as `Completed` or `Failed`.
        _ => LifecyclePhase::Pending,
    }
}

/// Project a workflow run row onto the MCP Tasks view.
///
/// This function is the **only** path through which internal run state
/// becomes visible to external MCP clients. It must remain read-only
/// and total.
#[must_use]
pub fn project_run_to_mcp(run: &WorkflowRunRow) -> McpTaskView {
    let phase = parse_lifecycle_phase(&run.lifecycle_phase);
    let status = map_status(&phase);
    tracing::trace!(
        run_id = %run.id,
        workflow_id = %run.workflow_id,
        phase = %run.lifecycle_phase,
        mcp_status = status.as_str(),
        "rightkit-agent-workflow: projected run to MCP task view"
    );
    McpTaskView {
        task_id: run.id.clone(),
        status,
        run_id: run.id.clone(),
        title: run.title.clone(),
        created_at_unix_ms: run.started_at_unix_ms,
        last_updated_at_unix_ms: run.last_updated_at_unix_ms,
    }
}

// ─── Boundary validation ─────────────────────────────────────────────────────

/// Typed failure for the MCP-side "create task" boundary. Replaces
/// free-form rejection strings with a closed enum so the daemon can
/// map every refusal onto a stable MCP error code.
#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
pub enum McpBoundaryError {
    /// Request attempted to set `skip_policy`, `skip_approvals`, or
    /// `skip_effects`. The MCP layer is allowed to trigger runs but
    /// is never allowed to widen authority.
    #[error("mcp task create request attempted to bypass authority (forbidden field `{0}`)")]
    ForbiddenSkipField(String),
    /// Request body was not a JSON object. The MCP layer requires a
    /// typed payload; bare scalars are rejected.
    #[error("mcp task create request body must be a JSON object")]
    NotAnObject,
    /// MCP projections cannot set outer workflow authority or lifecycle.
    #[error("mcp task request attempted to set authority field `{0}`")]
    AuthorityField(String),
}

/// Validate an external "create task" request payload. The MCP layer
/// is allowed to trigger a workflow run; it is **never** allowed to
/// skip the control plane's policy, approval, or effect gates.
///
/// The validation is deliberately strict: any field whose name starts
/// with `skip_` is treated as an authority-bypass attempt and rejected
/// with [`McpBoundaryError::ForbiddenSkipField`]. This is the smallest
/// possible surface that catches the common bypass patterns
/// (`skip_policy`, `skip_approvals`, `skip_effects`, future
/// `skip_retry`, etc.) without enumerating every specific name.
pub fn validate_external_task_create_request(req: &Value) -> Result<(), McpBoundaryError> {
    let Some(obj) = req.as_object() else {
        return Err(McpBoundaryError::NotAnObject);
    };
    for key in obj.keys() {
        if key.starts_with("skip_") {
            tracing::warn!(
                forbidden_field = %key,
                "rightkit-agent-workflow: rejected MCP task create request that tried to bypass authority"
            );
            return Err(McpBoundaryError::ForbiddenSkipField(key.clone()));
        }
        if matches!(
            key.as_str(),
            "capabilities"
                | "budget"
                | "workspace_root"
                | "lifecycle_phase"
                | "approval"
                | "effect"
        ) {
            return Err(McpBoundaryError::AuthorityField(key.clone()));
        }
    }
    Ok(())
}
