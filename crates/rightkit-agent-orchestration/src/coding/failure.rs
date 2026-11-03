//! Failure taxonomy for agent task results.
//!
//! Maps to Microsoft AI Red Team v2.0 failure dimensions (Jun 2026) and
//! AgentRx auto-diagnosis categories (arXiv 2602.02475).
//! Do NOT invent new types — use these vocabularies so failures are
//! comparable across tools and teams.

use serde::{Deserialize, Serialize};

/// Top-level failure dimension (MS Red Team v2.0, 5 dimensions).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureDimension {
    /// Agent did not accomplish the intended task.
    Efficacy,
    /// Agent behaved unsafely or harmfully.
    Safety,
    /// Agent violated privacy, confidentiality, or data-handling rules.
    Privacy,
    /// Agent produced or assisted with disinformation or deceptive outputs.
    Truthfulness,
    /// Agent failed to remain within authorized scope or escalated privileges.
    Containment,
}

/// Specific failure kind within a dimension.
/// Mapped to AgentRx execution-trace categories (arXiv 2602.02475).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    // ── Efficacy failures ────────────────────────────────────────────────
    /// Task timed out or stalled without progress.
    Timeout,
    /// Agent looped on the same action (MOP — meltdown onset point).
    MopLoop,
    /// Agent produced an output but it was wrong / failed verification.
    WrongOutput,
    /// External tool call returned an error that was not recovered.
    ToolError,
    /// Agent ran out of context budget before completing.
    ContextExhausted,
    /// Maximum attempts exhausted without success.
    MaxRetriesExhausted,
    /// Task was explicitly aborted by the orchestrator.
    Aborted,

    // ── Safety / containment failures ───────────────────────────────────
    /// Agent attempted a destructive or irreversible action.
    DestructiveAction,
    /// Agent exceeded authorized scope or called a disallowed tool.
    ScopeViolation,
    /// Agent produced output that violated the content policy.
    ContentPolicy,

    // ── Infra / I/O failures ─────────────────────────────────────────────
    /// Provider / network error that was not recovered.
    ProviderError,
    /// Worker process crashed or panicked.
    WorkerPanic,

    /// Catch-all for failures that don't map to any specific kind.
    Unknown,
}

impl FailureKind {
    /// Returns the MS Red Team v2.0 dimension this failure belongs to.
    pub fn dimension(&self) -> FailureDimension {
        match self {
            FailureKind::Timeout
            | FailureKind::MopLoop
            | FailureKind::WrongOutput
            | FailureKind::ToolError
            | FailureKind::ContextExhausted
            | FailureKind::MaxRetriesExhausted
            | FailureKind::Aborted
            | FailureKind::ProviderError
            | FailureKind::WorkerPanic
            | FailureKind::Unknown => FailureDimension::Efficacy,
            FailureKind::DestructiveAction | FailureKind::ScopeViolation => {
                FailureDimension::Containment
            }
            FailureKind::ContentPolicy => FailureDimension::Safety,
        }
    }

    /// Infer a `FailureKind` from the worker output string (heuristic).
    pub fn infer_from_output(output: &str, mop_fired: bool, timed_out: bool) -> FailureKind {
        if mop_fired {
            return FailureKind::MopLoop;
        }
        if timed_out {
            return FailureKind::Timeout;
        }
        let lower = output.to_lowercase();
        if lower.contains("maximum attempts") || lower.contains("max retries") {
            FailureKind::MaxRetriesExhausted
        } else if lower.contains("context") && lower.contains("exhaust") {
            FailureKind::ContextExhausted
        } else if lower.contains("provider") || lower.contains("network") || lower.contains("http")
        {
            FailureKind::ProviderError
        } else if lower.contains("destructive") || lower.contains("disallowed") {
            FailureKind::DestructiveAction
        } else if lower.contains("scope") || lower.contains("unauthorized") {
            FailureKind::ScopeViolation
        } else if lower.contains("panic") || lower.contains("crash") {
            FailureKind::WorkerPanic
        } else if lower.contains("tool error") || lower.contains("tool call") {
            FailureKind::ToolError
        } else {
            FailureKind::Unknown
        }
    }
}
