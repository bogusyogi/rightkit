use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MiddlewareDecisionEvidence {
    pub middleware_id: String,
    pub title: String,
    pub phase: String,
    pub effect: String,
    pub decision: String,
    pub reason: String,
}

/// A verification issue item inside `VerificationFinished`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationIssue {
    #[serde(default)]
    pub tool: String,
    #[serde(default)]
    pub file_path: String,
    pub line: Option<i32>,
    pub column: Option<i32>,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub repair_suggestion: String,
}

/// An orchestration observation attached to `OrchestrationObservationRecorded`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationObservation {
    pub id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub fields: std::collections::HashMap<String, String>,
    pub created_at: String,
}

/// One selectable option in a [`UserQuestion`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserQuestionOption {
    pub label: String,
    #[serde(default)]
    pub description: String,
}

/// A structured question the agent asks the operator (Claude-Code `AskUserQuestion`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserQuestion {
    pub question: String,
    /// Short chip/label for the question (e.g. "Auth method").
    #[serde(default)]
    pub header: String,
    pub options: Vec<UserQuestionOption>,
    #[serde(default)]
    pub multi_select: bool,
}

/// One sub-agent's live row in the session agent rail (F1). Accumulated by the
/// daemon from the child session's `UsageUpdated`/`ToolCallStarted` events.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentRosterRow {
    pub sub_session_id: String,
    pub role: String,
    /// running | done | failed | cancelled
    pub status: String,
    /// Model this agent is running on. The rail showed role and tool counts but
    /// never which model executed the work, so a user could not see that Pro's
    /// multi-model routing was doing anything — every agent looked identical to
    /// the free single-model case. Empty when the daemon has no route yet.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cache_read: u64,
    pub tool_uses: u32,
    pub elapsed_ms: u64,
    /// most-recent first, max 3
    pub last_tools: Vec<String>,
}

/// Aggregate token/cost totals across a session's sub-agents (F1 → F2 contract).
/// The budget meter for a Goal reads THIS (parent + all children), never the
/// parent engine's usage alone.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionUsageRollup {
    pub session_id: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    pub cost_usd: f64,
    pub updated_at: String,
}

impl SessionUsageRollup {
    pub fn from_rows(session_id: &str, rows: &[AgentRosterRow], updated_at: &str) -> Self {
        let mut r = SessionUsageRollup {
            session_id: session_id.to_string(),
            tokens_in: 0,
            tokens_out: 0,
            cache_read: 0,
            cache_creation: 0,
            cost_usd: 0.0,
            updated_at: updated_at.to_string(),
        };
        for a in rows {
            r.tokens_in += a.tokens_in;
            r.tokens_out += a.tokens_out;
            r.cache_read += a.cache_read;
        }
        r
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SelfReviewProposalKind {
    Memory,
    HarnessRule,
    Workflow,
    SkillTweak,
    Skill,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelfReviewProposalDraft {
    pub id: String,
    pub kind: SelfReviewProposalKind,
    pub summary: String,
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolEventMeta {
    pub session_id: String,
    pub seq: i64,
    pub created_at: String,
}

/// Pinned pricing snapshot carried on `ProtocolEvent::ModelRouteDecision`
/// (ADR `docs/canon/daemon-master-canon-atoms.md` §D5). Mirrors
/// the `(model, prompt_per_m, completion_per_m, source, fetched_at, hash)`
/// tuple named in §D4 for pricing refresh, but this crate carries no
/// dependency on `host_router`/`host_providers` (pure data crate) —
/// the seam that emits the event fills these fields from the live roster.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceSnapshot {
    pub prompt_per_m: f64,
    pub completion_per_m: f64,
    /// Where this pricing row came from — `"bundled"` in R1a (R1c adds
    /// `"openrouter"` / `"litellm"` for live-refreshed rows).
    pub source: String,
    pub fetched_at: String,
    /// Stable hash of the pricing row content, so a UI can detect that the
    /// snapshot backing an old chip has since changed.
    pub hash: String,
}

fn default_route_decision_schema_version() -> u32 {
    1
}

/// Typed failure categories for run governance (Lane B3 / C3). Emitted by the
/// engine when a run-side invariant rejects a tool dispatch. The variants here
/// are wire-stable so downstream consumers (desktop budget meter, error toasts,
/// telemetry) can react deterministically — every failure has a discriminator
/// and the call site that produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RunFailureKind {
    /// Per-run concurrent or cumulative sub-agent limit was hit.
    /// `concurrent` is the active-running count for the run; `cumulative` is
    /// the total accepted across the run's lifetime. The exact limit
    /// (whichever the call exceeded) is carried on the typed payload.
    BudgetExceeded,
    /// A caller tried to mutate, cancel, or read another run's state.
    /// Operations on `run_id` must come from a session that actually owns
    /// (or is delegated by) that run; cross-run access is rejected here
    /// rather than silently diverging.
    OwnershipMismatch,
    /// A durable `ToolCallStarted` origin record was required before the side
    /// effect fired, and the persistence seam returned an error. The engine
    /// must NOT execute the call in this state.
    OriginMissing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunBudgetExhausted {
    pub run_id: String,
    pub tool_call_id: String,
    pub concurrent_limit: u32,
    pub concurrent_in_flight: u32,
    pub cumulative_limit: u32,
    pub cumulative_used: u32,
    pub exceeded_kind: RunBudgetKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RunBudgetKind {
    Concurrent,
    Cumulative,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunOwnershipFailure {
    pub caller_run_id: String,
    pub target_run_id: String,
    pub action: RunOwnershipAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RunOwnershipAction {
    MutateAgent,
    CancelAgent,
    ReadState,
    DispatchTool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunOriginFailure {
    pub run_id: String,
    pub tool_call_id: String,
    /// What the persistence seam reported. Empty string means the call did
    /// not even start its persistence attempt — usually a `NotReady` signal
    /// from the origin sink.
    pub reason: String,
}

/// `ProtocolEvent::ModelRouteDecision` payload (ADR §D5 observability). Kept
/// as its own struct (not inlined into the enum variant) so
/// `host_router`'s `RouteDecision` and this wire type can evolve
/// independently — the seam maps one to the other explicitly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouteDecisionPayload {
    pub agent_role: String,
    pub model_id: String,
    /// `"cheap"` | `"capable"` — string, not `host_providers::ModelTier`,
    /// to keep this crate dependency-free.
    pub tier: String,
    pub reason: String,
    /// Opaque digest of the `TaskSignal` that produced this decision —
    /// enough for a hover tooltip / debug trail, not the raw signal (avoids
    /// leaking prompt text into the protocol log).
    pub signals_digest: String,
    pub price_snapshot: PriceSnapshot,
    pub est_cost_usd: f64,
    #[serde(default = "default_route_decision_schema_version")]
    pub schema_version: u32,
}

/// Typed, reaped terminal status of a shell child (foreground or background).
/// Reported only after the child has been waited on, so a consumer never sees
/// an exit status for a process that may still be running.
///
/// `signaled.signal` is `None` when the platform reported death without an
/// exit code but the adapter did not surface the signal number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum ShellExitStatus {
    #[serde(rename = "exited")]
    Exited { code: i32 },
    #[serde(rename = "signaled")]
    Signaled {
        #[serde(default)]
        signal: Option<i32>,
    },
    #[serde(rename = "killedByTimeout", rename_all = "camelCase")]
    KilledByTimeout { timeout_ms: u64 },
    #[serde(rename = "killedByCancel")]
    KilledByCancel,
    /// The child never ran to a reaped state because launch failed.
    #[serde(rename = "spawnFailed")]
    SpawnFailed { reason: String },
}

impl ShellExitStatus {
    /// Stable one-line label used in tool results and the process table.
    pub fn label(&self) -> String {
        match self {
            Self::Exited { code } => format!("exited (code {code})"),
            Self::Signaled { signal: Some(s) } => format!("signaled (signal {s})"),
            Self::Signaled { signal: None } => "signaled".to_string(),
            Self::KilledByTimeout { timeout_ms } => {
                format!("killed by timeout after {timeout_ms}ms")
            }
            Self::KilledByCancel => "killed by cancel".to_string(),
            Self::SpawnFailed { reason } => format!("spawn failed: {reason}"),
        }
    }
}
