//! Plan and result types for the conductor.

use crate::failure::FailureKind;

/// The true completion state of a worker's run, distinguishing a clean
/// success from work that was cut off by a rate-limit/context ceiling or
/// an unrecovered provider/API error. A conductor that only inspects
/// `WorkerResult::success` cannot tell "the model said it's done" apart
/// from "the provider hung up mid-task" — this type makes masking a
/// worker failure as a successful result structurally impossible: there
/// is no boolean to silently flip, only these three explicit shapes.
///
/// Mirrors Claude Code 2.1.199's subagent error-propagation fix: a worker
/// cut off by a rate-limit or server error must report its PARTIAL work
/// to the parent and surface the error as an error, never as success.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum WorkerOutcome {
    /// The worker ran to completion; `WorkerResult::success` reflects the
    /// real outcome and `output` is the worker's full final output.
    #[default]
    Complete,
    /// The worker was cut off by a rate limit, context ceiling, wall-clock
    /// ceiling, or similar resource limit before it finished. `output`
    /// (on `WorkerResult`) carries whatever partial work the worker had
    /// produced before the cutoff; `limit_kind` names which limit fired
    /// (e.g. `"rate_limited"`, `"context_exhausted"`, `"wall_clock"`).
    /// A `PartialWithLimit` worker never counts as satisfied for
    /// spec-completion or jury purposes, even though it may carry
    /// non-empty output.
    PartialWithLimit { limit_kind: String },
    /// The worker's underlying provider/API call failed and was not
    /// recovered (network, auth, 5xx, or an unclassified provider error).
    /// `error` is the provider-facing error message. `partial_output`, if
    /// present, is whatever work the worker had produced before the
    /// error — the jury/synthesis step gets to see it, marked as partial,
    /// rather than only the fact that something failed.
    ApiError {
        error: String,
        partial_output: Option<String>,
    },
}

impl WorkerOutcome {
    /// `true` only for the [`WorkerOutcome::Complete`] variant. Used by
    /// the conductor/orchestrator to gate spec-completion and jury
    /// consumption: a `Partial`/`ApiError` worker is never treated as
    /// satisfied even if `WorkerResult::success` was (incorrectly, or by
    /// a stale caller) set to `true`.
    pub fn counts_as_complete(&self) -> bool {
        matches!(self, WorkerOutcome::Complete)
    }

    /// Short machine-readable label for protocol events / logs.
    pub fn label(&self) -> &'static str {
        match self {
            WorkerOutcome::Complete => "complete",
            WorkerOutcome::PartialWithLimit { .. } => "partial_with_limit",
            WorkerOutcome::ApiError { .. } => "api_error",
        }
    }
}

/// A unit of work dispatched to a worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerTask {
    pub task_id: String,
    pub agent: String,
    pub payload: String,
    /// The version of the agent that ran this task, if known.
    pub agent_version: Option<String>,
    /// Stable-prefix cache key inherited from the conductor run, if any.
    ///
    /// `None` on the single-worker (direct) path — the worker derives
    /// its own stable key via `SessionEngine`. `Some(shared_key)` when
    /// the conductor spawns this task as a sibling in a multi-worker
    /// plan: every sibling carries the same string, so each worker
    /// cache-reads against the parent's slot instead of independently
    /// breaking the cache. The handoff brief (per-worker CONTEXT tier)
    /// is excluded by construction — only the conductor identity +
    /// UTC day enter the hash.
    pub conductor_stable_cache_key: Option<String>,
}

/// The result a worker produces for a [`WorkerTask`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerResult {
    pub task_id: String,
    pub agent: String,
    pub success: bool,
    pub output: String,
    pub input_tokens: u32,
    pub mop_fired: bool,
    /// Typed failure kind when `success == false`. None on success.
    pub failure_kind: Option<FailureKind>,
    /// The version of the agent that produced this result, if known.
    pub agent_version: Option<String>,
    /// If the task failed and a trajectory rollback point was identified,
    /// this is the index of the last good step. The orchestrator can use this
    /// to seed the next attempt with context about where to resume.
    pub rollback_step: Option<usize>,
    /// The worker's true completion state (parity P-2). Defaults to
    /// [`WorkerOutcome::Complete`] for every existing construction site —
    /// this field is purely additive and does not change any Complete-path
    /// behavior. Only the two new failure paths (rate-limit/ceiling cutoff,
    /// unrecovered API error) populate the other variants.
    pub outcome: WorkerOutcome,
}

/// Results that have been fully collected from a worker batch.
///
/// Keeping this as a distinct type makes the collection-to-synthesis boundary
/// explicit: downstream dispatch receives results only after the batch has
/// resolved, including typed panic results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerCollection {
    results: Vec<WorkerResult>,
}

impl WorkerCollection {
    pub(crate) fn new(results: Vec<WorkerResult>) -> Self {
        Self { results }
    }

    /// Borrow all collected results in their completion order.
    pub fn as_slice(&self) -> &[WorkerResult] {
        &self.results
    }

    /// Consume the collection and return its results.
    pub fn into_results(self) -> Vec<WorkerResult> {
        self.results
    }

    /// Number of worker results collected.
    pub fn len(&self) -> usize {
        self.results.len()
    }

    /// Whether no worker result was collected.
    pub fn is_empty(&self) -> bool {
        self.results.is_empty()
    }
}

/// A plan describing a batch of worker tasks and how to run them.
///
/// `conductor_identity` + `conductor_day` (when both `Some`) drive the
/// cross-worker prefix-cache key. The conductor computes the key once
/// at dispatch time and stamps it onto every sibling task. Setting
/// either to `None` (or leaving them as their defaults) disables the
/// conductor-run key — single-worker / direct execution paths leave
/// `WorkerTask::conductor_stable_cache_key` as `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConductorPlan {
    pub tasks: Vec<WorkerTask>,
    pub parallel: bool,
    /// Stable conductor identity (e.g. `"host_conductor-v1"`).
    /// `None` means "this plan is single-worker / direct" — no shared
    /// stable cache key is propagated.
    pub conductor_identity: Option<String>,
    /// UTC day bucket (`YYYY-MM-DD`). `None` disables shared-key
    /// stamping.
    pub conductor_day: Option<String>,
}

impl ConductorPlan {
    /// Backwards-compatible constructor for plans that don't carry
    /// conductor-run identity. Single-worker / direct execution paths
    /// use this; their tasks get `conductor_stable_cache_key = None`
    /// and each worker derives its own stable key.
    pub fn new(tasks: Vec<WorkerTask>, parallel: bool) -> Self {
        Self {
            tasks,
            parallel,
            conductor_identity: None,
            conductor_day: None,
        }
    }

    /// Build a plan that propagates a shared conductor-stable cache
    /// key onto every sibling task. Same `(identity, day)` →
    /// bit-identical key; differing identity or day → cache break.
    pub fn with_conductor_identity(
        tasks: Vec<WorkerTask>,
        parallel: bool,
        identity: impl Into<String>,
        day: impl Into<String>,
    ) -> Self {
        Self {
            tasks,
            parallel,
            conductor_identity: Some(identity.into()),
            conductor_day: Some(day.into()),
        }
    }

    /// `true` when this plan will stamp the conductor-run stable key
    /// onto every sibling task.
    pub fn shares_conductor_cache_key(&self) -> bool {
        self.conductor_identity.is_some() && self.conductor_day.is_some()
    }
}
