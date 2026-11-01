use super::{
    types::{
        AgentRosterRow, MiddlewareDecisionEvidence, ModelRouteDecisionPayload,
        OrchestrationObservation, ProtocolEventMeta, SelfReviewProposalDraft,
    },
    UserQuestion, VerificationIssue,
};
use crate::message::TranscriptMessage;
use crate::task::TaskNode;
use crate::tool::ToolCall;
use crate::trust::TrustEnvelope;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A verified, daemon-owned step in a goal plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GoalStepV1 {
    pub id: String,
    pub label: String,
    pub weight: i64,
    pub state: String,
    pub verification_state: String,
}

/// Canonical goal truth shared by SSE and session hydration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GoalSnapshotV1 {
    pub goal_id: String,
    pub objective: String,
    pub status: String,
    pub started_at: String,
    pub closed_at: Option<String>,
    pub dismissed_at: Option<String>,
    pub decomposition_state: String,
    pub plan_revision: i64,
    pub completion_percent: Option<i64>,
    pub now: Option<String>,
    pub next: Option<String>,
    pub steps: Vec<GoalStepV1>,
    pub verification_state: String,
    pub budget_tokens: i64,
    pub tokens_used: i64,
    pub continuation_count: i64,
    pub no_progress_count: i64,
}

/// Selection receipt carried inside a compaction-federation decision:
/// which items the decision covers and a content hash Membrane can use to
/// detect a stale or substituted selection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CompactionFederationSelectionReceiptV1 {
    pub selected_item_ids: Vec<String>,
    pub selection_hash: String,
}

/// CRA-09 joint wire contract (`interface-handoffs.json` row
/// `compaction-federation-v1`): the one compatible public shape the host app's
/// compactor-consumer and Membrane's federation-catalog both code against.
///
/// Carries identity/fence/scope, the decision, its selection receipt, and a
/// bounded **server-issued** expiry. The expiry is expressed and checked as
/// monotonic elapsed milliseconds (`issued_monotonic_elapsed_ms` +
/// `expires_after_elapsed_ms`), never wall-clock time — a wall clock can be
/// adjusted (NTP, user change, DST) and would silently defeat the bound.
/// `restart_epoch` is the process/session monotonic-clock epoch the decision
/// was issued under; a consumer whose current epoch no longer matches must
/// treat any pending decision as invalidated by the restart, independent of
/// whether the elapsed-time expiry would otherwise still be live.
/// Wire contract-version tag for [`CompactionFederationDecisionV1`], matching
/// `interface-handoffs.json` row `compaction-federation-v1`. Membrane should
/// reference this constant rather than the bare string literal.
pub const COMPACTION_FEDERATION_SCHEMA_V1: &str = "compaction-federation-v1";

fn compaction_federation_schema_v1() -> String {
    COMPACTION_FEDERATION_SCHEMA_V1.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CompactionFederationDecisionV1 {
    /// Wire contract-version tag; always serialized as `$schema`.
    #[serde(rename = "$schema", default = "compaction_federation_schema_v1")]
    pub schema: String,
    /// Stable identity of the decision (idempotency key for one-use commit).
    pub identity: String,
    /// Fence token binding this decision to one session/turn boundary.
    pub fence: String,
    /// The federation scope this decision governs (e.g. a context/archive id).
    pub scope: String,
    /// The decision outcome: `"approved"`, `"deferred"`, or `"denied"`.
    pub decision: String,
    pub selection_receipt: CompactionFederationSelectionReceiptV1,
    /// Monotonic elapsed milliseconds, on the issuing server's own monotonic
    /// clock, at the moment this decision was issued.
    pub issued_monotonic_elapsed_ms: u64,
    /// Bounded server-issued expiry window, in elapsed milliseconds from
    /// `issued_monotonic_elapsed_ms`. Never a wall-clock timestamp.
    pub expires_after_elapsed_ms: u64,
    /// Monotonic-clock epoch (e.g. process/session boot generation) this
    /// decision was issued under.
    pub restart_epoch: u64,
}

impl CompactionFederationDecisionV1 {
    /// Whether the decision is still live: same monotonic epoch (no restart
    /// since issuance) and still inside its server-issued expiry window,
    /// measured strictly against monotonic elapsed time. Must be checked
    /// before any one-use local commit is honored.
    pub fn is_live(&self, now_monotonic_elapsed_ms: u64, current_restart_epoch: u64) -> bool {
        if current_restart_epoch != self.restart_epoch {
            return false;
        }
        match now_monotonic_elapsed_ms.checked_sub(self.issued_monotonic_elapsed_ms) {
            Some(elapsed) => elapsed <= self.expires_after_elapsed_ms,
            None => false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "$kind")]
pub enum ProtocolEvent {
    #[serde(rename = "sessionStarted", rename_all = "camelCase")]
    SessionStarted {
        session_id: String,
        seq: i64,
        created_at: String,
        title: Option<String>,
    },

    #[serde(rename = "sessionTitleUpdated", rename_all = "camelCase")]
    SessionTitleUpdated {
        session_id: String,
        seq: i64,
        created_at: String,
        title: String,
    },

    #[serde(rename = "assistantDelta", rename_all = "camelCase")]
    AssistantDelta {
        session_id: String,
        seq: i64,
        created_at: String,
        text: String,
        is_reasoning: bool,
    },

    #[serde(rename = "assistantMessageCompleted", rename_all = "camelCase")]
    AssistantMessageCompleted {
        session_id: String,
        seq: i64,
        created_at: String,
        message: TranscriptMessage,
    },

    #[serde(rename = "toolCallStarted", rename_all = "camelCase")]
    ToolCallStarted {
        session_id: String,
        seq: i64,
        created_at: String,
        call: ToolCall,
    },

    #[serde(rename = "toolApprovalRequested", rename_all = "camelCase")]
    ToolApprovalRequested {
        session_id: String,
        seq: i64,
        created_at: String,
        call: ToolCall,
        risk_label: String,
        proposed_change: Option<String>,
        policy_source: Option<String>,
        #[serde(default)]
        middleware_decisions: Vec<MiddlewareDecisionEvidence>,
    },

    #[serde(rename = "toolApprovalResolved", rename_all = "camelCase")]
    ToolApprovalResolved {
        session_id: String,
        seq: i64,
        created_at: String,
        tool_call_id: String,
        approved: bool,
    },

    /// Typed verdict of the approval review stage for one mutating tool call,
    /// emitted durably before the normal permission/approval gate. `verdict`
    /// is `allow` (reviewer echoed the exact request identity; the normal
    /// approval still applies), `block` (reviewer blocked, errored, or answered
    /// uncorrelated; the call is denied), or `unavailable` (no reviewer is
    /// configured; nothing is auto-approved and the human gate decides).
    /// `correlation_id` is the same token the approval and denial records use
    /// for this tool call.
    #[serde(rename = "toolApprovalReviewed", rename_all = "camelCase")]
    ToolApprovalReviewed {
        session_id: String,
        seq: i64,
        created_at: String,
        tool_call_id: String,
        tool_name: String,
        correlation_id: String,
        verdict: String,
        reason: String,
        reviewer_configured: bool,
    },

    #[serde(rename = "toolResultAdded", rename_all = "camelCase")]
    ToolResultAdded {
        session_id: String,
        seq: i64,
        created_at: String,
        result: TranscriptMessage,
    },

    /// A turn reached a typed terminal state.  Consumers can distinguish a
    /// completed turn from a transport disconnect without inferring it from
    /// presentation events.
    #[serde(rename = "sessionTurnFinished", rename_all = "camelCase")]
    SessionTurnFinished {
        session_id: String,
        seq: i64,
        created_at: String,
        turn_id: String,
        status: String,
    },

    /// The requested replay cursor predates contiguous conduit coverage.
    /// Drivers must resync from durable session state before reconnecting.
    #[serde(rename = "streamResyncRequired", rename_all = "camelCase")]
    StreamResyncRequired {
        session_id: String,
        seq: i64,
        created_at: String,
        after_seq: i64,
        oldest_available_seq: i64,
        high_water_mark: i64,
    },

    /// Session persistence latched as failed: later durable facts are refused and
    /// in-flight tools are cancelled. Live-only by design (never persisted,
    /// since persistence is what failed); `reason` is the first latch reason.
    #[serde(rename = "persistenceFailed", rename_all = "camelCase")]
    PersistenceFailed {
        session_id: String,
        seq: i64,
        created_at: String,
        reason: String,
    },

    /// Admission rejected work before it could consume an unbounded queue.
    #[serde(rename = "overloaded", rename_all = "camelCase")]
    Overloaded {
        session_id: String,
        seq: i64,
        created_at: String,
        scope: String,
        limit: u64,
        current: u64,
        #[serde(default)]
        retry_after_ms: u64,
    },

    /// A provider call failed transiently and the adapter is backing off
    /// before retry `attempt` of `max_attempts`.
    #[serde(rename = "providerRetrying", rename_all = "camelCase")]
    ProviderRetrying {
        session_id: String,
        seq: i64,
        created_at: String,
        provider: String,
        reason: String,
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
    },

    /// Context compaction began; the matching `sessionCompacted` follows on
    /// success. `trigger` is `automatic`, `manual` or `overflow`.
    #[serde(rename = "sessionCompactionStarted", rename_all = "camelCase")]
    SessionCompactionStarted {
        session_id: String,
        seq: i64,
        created_at: String,
        #[serde(default)]
        trigger: String,
        #[serde(default)]
        command_id: String,
    },

    #[serde(rename = "verificationFinished", rename_all = "camelCase")]
    VerificationFinished {
        session_id: String,
        seq: i64,
        created_at: String,
        summary: String,
        passed: bool,
        #[serde(default)]
        tool: String,
        #[serde(default)]
        issue_count: i32,
        #[serde(default)]
        issues: Vec<VerificationIssue>,
    },

    #[serde(rename = "taskGraphUpdated", rename_all = "camelCase")]
    TaskGraphUpdated {
        session_id: String,
        seq: i64,
        created_at: String,
        summary: String,
        #[serde(default)]
        tasks: Vec<TaskNode>,
    },

    #[serde(rename = "taskStatusUpdated", rename_all = "camelCase")]
    TaskStatusUpdated {
        session_id: String,
        seq: i64,
        created_at: String,
        task_id: String,
        context_id: String,
        state: String,
        task: Value,
    },

    #[serde(rename = "taskArtifactUpdated", rename_all = "camelCase")]
    TaskArtifactUpdated {
        session_id: String,
        seq: i64,
        created_at: String,
        task_id: String,
        context_id: String,
        artifact_id: String,
        artifact: Value,
    },

    #[serde(rename = "artifactLineageUpdated", rename_all = "camelCase")]
    ArtifactLineageUpdated {
        session_id: String,
        seq: i64,
        created_at: String,
        task_id: String,
        context_id: String,
        artifact_id: String,
        lineage: Value,
    },

    #[serde(rename = "runInterrupted", rename_all = "camelCase")]
    RunInterrupted {
        session_id: String,
        seq: i64,
        created_at: String,
        reason: Option<String>,
    },

    #[serde(rename = "runFailed", rename_all = "camelCase")]
    RunFailed {
        session_id: String,
        seq: i64,
        created_at: String,
        error: String,
        /// Failure class: `network` | `timeout` | `rate_limited` | `auth` |
        /// `provider_error` | `budget` | `other`
        #[serde(default)]
        kind: String,
        #[serde(default)]
        hint: String,
    },

    #[serde(rename = "sessionCompacted", rename_all = "camelCase")]
    SessionCompacted {
        session_id: String,
        seq: i64,
        created_at: String,
        summary: String,
        #[serde(default)]
        dropped_count: i32,
        /// Compaction tier that produced this event: `summary` (tier 2,
        /// model-authored structured session-state summary), `overflow`
        /// (tier 3, reactive provider-rejection recovery) or `manual`.
        #[serde(default)]
        tier: String,
        /// Estimated model-visible context tokens before compaction.
        #[serde(default)]
        tokens_before: u64,
        /// Estimated model-visible context tokens after compaction.
        #[serde(default)]
        tokens_after: u64,
        /// Session compaction epoch after this compaction (HCTX-004). Each
        /// successful compaction is an expected prompt-cache break and
        /// invalidates host-loaded context identities from earlier epochs.
        #[serde(default)]
        epoch: u64,
        #[serde(default)]
        command_id: String,
        /// Number of most-recent complete user turns the live pass retained
        /// verbatim (`recent_turn_message_count` argument; 0 for the
        /// under-pressure pass). Restart replay uses it to reproduce the same
        /// retained cut.
        retained_turns: u32,
    },

    /// A completed sub-trajectory was replaced by a recoverable fold marker.
    /// Marker strings are persisted so restart replay applies the same structural
    /// replacement to the model-facing and visible transcripts.
    #[serde(rename = "sessionContextFolded", rename_all = "camelCase")]
    SessionContextFolded {
        session_id: String,
        seq: i64,
        created_at: String,
        subtask_id: String,
        start_marker: String,
        end_marker: String,
        fold: TranscriptMessage,
    },

    #[serde(rename = "usageUpdated", rename_all = "camelCase")]
    UsageUpdated {
        session_id: String,
        seq: i64,
        created_at: String,
        prompt_tokens: i32,
        completion_tokens: i32,
        cost_usd: f64,
        context_tokens: i32,
        context_percent: f64,
        #[serde(default)]
        cache_read_tokens: i32,
        #[serde(default)]
        cache_creation_tokens: i32,
        /// Whether `cost_usd` came from curated pricing for the active model
        /// (vs. a conservative estimate for an unpriced model). Consumers that
        /// surface cost to operators (session rows, `agents --json`) must
        /// treat `false` as "cost unknown" — report null, not this number.
        /// Defaults to `true` for older senders that predate this field, to
        /// preserve their existing (assumed-correct) cost display.
        #[serde(default = "default_cost_known")]
        cost_known: bool,
    },

    /// Per-model-call usage for live prompt-cache / context displays. Emitted
    /// once per provider response, in addition to the cumulative
    /// `usageUpdated`. `inputTokens` is the FULL prompt size
    /// (uncached + cached + cache-write); `cachedInputTokens` is the
    /// cache-read subset. `reasoningTokens` is a subset of `outputTokens`.
    /// `contextWindow` is 0 when the model's window is unknown, in which case
    /// `contextUsedPct` is 0. `contextUsedPct` is 0.0-100.0.
    #[serde(rename = "turnUsage", rename_all = "camelCase")]
    TurnUsage {
        session_id: String,
        seq: i64,
        created_at: String,
        input_tokens: i64,
        cached_input_tokens: i64,
        #[serde(default)]
        cache_creation_tokens: i64,
        output_tokens: i64,
        #[serde(default)]
        reasoning_tokens: i64,
        #[serde(default)]
        context_window: i64,
        #[serde(default)]
        context_used_tokens: i64,
        #[serde(default)]
        context_used_pct: f64,
        /// This response's prompt-cache hit rate, 0.0-100.0:
        /// `cachedInputTokens / inputTokens` (`inputTokens` already includes
        /// cache reads and cache writes). 0 when the response had no input.
        #[serde(default)]
        cache_hit_pct: f64,
        /// Session-aggregate prompt-cache hit rate through this response,
        /// 0.0-100.0: cache reads / (uncached + cache reads + cache writes).
        #[serde(default)]
        session_cache_hit_pct: f64,
    },

    /// A provider response whose cache read fell materially below what the
    /// previous request's cached prefix promised (donor
    /// `promptCacheBreakDetection.ts`). `reason` is the primary attributed
    /// cause (`system`, `tools`, `model`, `params`, `cacheControl`,
    /// `messagesEdited`, `compaction`, `ttlExpiry1h`, `ttlExpiry5m`,
    /// `serverSide`, `unknown`); `detail` lists every observed change.
    /// `expected` is true for breaks the host app caused deliberately
    /// (compaction, model switch, explicit tool-set change, idle TTL expiry)
    /// and false for everything that is silently burning money.
    /// `estimatedCostLost` is absent when the active model has no curated
    /// pricing. `turn` is the 1-based provider request number in the session.
    #[serde(rename = "promptCacheBreak", rename_all = "camelCase")]
    PromptCacheBreak {
        session_id: String,
        seq: i64,
        created_at: String,
        reason: String,
        expected: bool,
        tokens_lost: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        estimated_cost_lost: Option<f64>,
        turn: i64,
        #[serde(default)]
        detail: String,
        #[serde(default)]
        previous_cache_read_tokens: i64,
        #[serde(default)]
        cache_read_tokens: i64,
        #[serde(default)]
        cache_creation_tokens: i64,
    },

    /// Live output chunk from a running shell/exec tool. Coalesced by the
    /// emitter (at most one event per stream per ~100ms); `toolResultAdded`
    /// remains the authoritative final output.
    #[serde(rename = "toolOutputDelta", rename_all = "camelCase")]
    ToolOutputDelta {
        session_id: String,
        seq: i64,
        created_at: String,
        call_id: String,
        /// `stdout` or `stderr`.
        stream: String,
        text: String,
    },

    /// A background shell reached a terminal state. Emitted once per shell,
    /// only after the child was reaped, so consumers are notified instead of
    /// polling `get_shell_output` (which keeps working). Durable: it is the
    /// settled fact that the process is gone.
    #[serde(rename = "backgroundShellExited", rename_all = "camelCase")]
    BackgroundShellExited {
        session_id: String,
        seq: i64,
        created_at: String,
        shell_id: String,
        #[serde(default)]
        command: String,
        status: super::types::ShellExitStatus,
        #[serde(default)]
        output_bytes: u64,
    },

    #[serde(rename = "budgetExceeded", rename_all = "camelCase")]
    BudgetExceeded {
        session_id: String,
        seq: i64,
        created_at: String,
        #[serde(default)]
        scope: String,
        current_usd: f64,
        ceiling_usd: f64,
        #[serde(default)]
        hint: String,
    },

    #[serde(rename = "providerRouteUpdated", rename_all = "camelCase")]
    ProviderRouteUpdated {
        session_id: String,
        seq: i64,
        created_at: String,
        #[serde(default)]
        state: String,
        #[serde(default)]
        provider: String,
        #[serde(default)]
        model: String,
        #[serde(default)]
        failure_kind: String,
        #[serde(default)]
        detail: String,
    },

    /// A vendor account reported rate-limit telemetry. `status` is
    /// `allowed`, `allowed_warning` or `rejected`; when `rejected`, the UI
    /// shows "account X capped until `resetsAt`" and offers a manual
    /// "continue on another account". the host app never switches accounts
    /// automatically.
    #[serde(rename = "accountRateLimited", rename_all = "camelCase")]
    AccountRateLimited {
        session_id: String,
        seq: i64,
        created_at: String,
        #[serde(default)]
        provider: String,
        /// Account display label: the user's label, else email, else the profile id.
        #[serde(default)]
        account: String,
        /// Stable account id (`subscription/<provider>/<label>` or `api_key/<provider>/<slug>`).
        #[serde(default)]
        account_id: Option<String>,
        /// `subscription` or `api_key`.
        #[serde(default)]
        account_kind: Option<String>,
        /// Present only when the vendor CLI reported an email; never a label.
        #[serde(default)]
        email: Option<String>,
        #[serde(default)]
        organization: Option<String>,
        #[serde(default)]
        status: String,
        #[serde(default)]
        rate_limit_type: Option<String>,
        /// Unix seconds at which the window resets.
        #[serde(default)]
        resets_at: Option<u64>,
        #[serde(default)]
        utilization: Option<f64>,
        #[serde(default)]
        overage_status: Option<String>,
        #[serde(default)]
        error_code: Option<String>,
    },

    /// Per-turn cost and token usage reported by a vendor CLI, attributed to
    /// the account that ran the turn.
    #[serde(rename = "accountUsageReported", rename_all = "camelCase")]
    AccountUsageReported {
        session_id: String,
        seq: i64,
        created_at: String,
        #[serde(default)]
        provider: String,
        #[serde(default)]
        account: String,
        #[serde(default)]
        account_id: Option<String>,
        #[serde(default)]
        account_kind: Option<String>,
        #[serde(default)]
        email: Option<String>,
        #[serde(default)]
        total_cost_usd: Option<f64>,
        #[serde(default)]
        duration_ms: Option<u64>,
        #[serde(default)]
        num_turns: Option<u64>,
        #[serde(default)]
        input_tokens: u64,
        #[serde(default)]
        output_tokens: u64,
        #[serde(default)]
        cache_read_tokens: u64,
        #[serde(default)]
        cache_creation_tokens: u64,
        #[serde(default)]
        models: Vec<String>,
    },

    #[serde(rename = "dispatchDecided", rename_all = "camelCase")]
    DispatchDecided {
        session_id: String,
        seq: i64,
        created_at: String,
        strategy: String,
        rationale: String,
    },

    #[serde(rename = "orchestrationObservationRecorded", rename_all = "camelCase")]
    OrchestrationObservationRecorded {
        session_id: String,
        seq: i64,
        created_at: String,
        observation: OrchestrationObservation,
    },

    #[serde(rename = "agentQueryIssued", rename_all = "camelCase")]
    AgentQueryIssued {
        session_id: String,
        seq: i64,
        created_at: String,
        query_id: String,
        from_role: String,
        to_role: String,
        query_type: String,
        expected_format: String,
    },

    #[serde(rename = "agentQueryResponded", rename_all = "camelCase")]
    AgentQueryResponded {
        session_id: String,
        seq: i64,
        created_at: String,
        query_id: String,
        from_role: String,
        to_role: String,
        confidence: f64,
    },

    #[serde(rename = "humanSteeringInjected", rename_all = "camelCase")]
    HumanSteeringInjected {
        session_id: String,
        seq: i64,
        created_at: String,
        injected_by: String,
        content_preview: String,
        /// The actual steering/reprompt text (bounded at the interjection
        /// byte cap before it is stored). `None` for rows written before this
        /// field existed; consumers must keep treating the event as an opaque
        /// steering marker in that case.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },

    /// Emitted on the **parent** session when the orchestrator spawns a sub-agent.
    #[serde(rename = "subAgentSpawned", rename_all = "camelCase")]
    SubAgentSpawned {
        session_id: String,
        seq: i64,
        created_at: String,
        sub_session_id: String,
        role: String,
        task_title: String,
    },

    /// Emitted on the parent session when a sub-agent finishes (success or failure).
    #[serde(rename = "subAgentCompleted", rename_all = "camelCase")]
    SubAgentCompleted {
        session_id: String,
        seq: i64,
        created_at: String,
        sub_session_id: String,
        role: String,
        success: bool,
        summary: String,
        /// The sub-agent's true completion state (parity P-2):
        /// `"complete"` | `"partial_with_limit"` | `"api_error"`. Defaults
        /// to `"complete"` for older senders that predate this field, so a
        /// rolling deploy never fabricates a false partial/error state —
        /// only new senders that actually detected a cutoff/error set it.
        #[serde(default = "default_worker_outcome")]
        outcome: String,
        /// Set only when `outcome != "complete"`: the limit that fired
        /// (`"rate_limited"`, `"timeout"`, `"wall_clock"`, `"budget"`) or
        /// the provider/API error text. Empty for a clean completion.
        #[serde(default)]
        outcome_detail: String,
    },

    /// Emitted on the parent session when a sub-agent is stopped before completing.
    /// `cancelled_by`: "user" | "budget" | "governance" | "error"
    #[serde(rename = "subAgentCancelled", rename_all = "camelCase")]
    SubAgentCancelled {
        session_id: String,
        seq: i64,
        created_at: String,
        sub_session_id: String,
        role: String,
        cancelled_by: String,
    },

    /// A user/operator turn was submitted to the session. Emitted so the event
    /// log is the single source of truth for replay (the user side of the
    /// conversation, not just the assistant/tool side).
    #[serde(rename = "userMessageSubmitted", rename_all = "camelCase")]
    UserMessageSubmitted {
        session_id: String,
        seq: i64,
        created_at: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nonce: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trust: Option<TrustEnvelope>,
        /// Metadata-only references attached to this user turn. Defaulting
        /// preserves replay compatibility with events written before rich
        /// references existed.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<crate::ReferenceMeta>,
    },

    /// The agent is blocking on a structured question to the operator. The UI
    /// renders the choices and POSTs answers back, keyed by `question_id`.
    #[serde(rename = "userQuestionAsked", rename_all = "camelCase")]
    UserQuestionAsked {
        session_id: String,
        seq: i64,
        created_at: String,
        question_id: String,
        questions: Vec<UserQuestion>,
    },

    /// An MCP stdio server issued a bounded elicitation request to the client.
    /// The operator response is correlated by `request.id` and returned to the
    /// waiting server request; form values remain opaque JSON data.
    #[serde(rename = "mcpElicitationRequested", rename_all = "camelCase")]
    McpElicitationRequested {
        session_id: String,
        seq: i64,
        created_at: String,
        request: crate::approvals::ElicitationRequestEvent,
    },

    /// A running tool needs one provider credential before it can continue.
    /// Carries request identity and provider metadata only; secret input is
    /// returned through the correlated response channel, never this event.
    #[serde(rename = "credentialRequested", rename_all = "camelCase")]
    CredentialRequested {
        session_id: String,
        seq: i64,
        created_at: String,
        tool_call_id: String,
        provider: String,
    },

    #[serde(rename = "credentialResolved", rename_all = "camelCase")]
    CredentialResolved {
        session_id: String,
        seq: i64,
        created_at: String,
        tool_call_id: String,
        outcome: String,
    },

    /// The operator answered (or the question was otherwise resolved).
    #[serde(rename = "userQuestionAnswered", rename_all = "camelCase")]
    UserQuestionAnswered {
        session_id: String,
        seq: i64,
        created_at: String,
        question_id: String,
    },

    /// The agent presented a plan via `exit_plan_mode` and is blocking on
    /// approval. The UI renders the plan and POSTs the decision back keyed by
    /// `plan_id`; on approval the session leaves Plan mode.
    #[serde(rename = "planPresented", rename_all = "camelCase")]
    PlanPresented {
        session_id: String,
        seq: i64,
        created_at: String,
        plan_id: String,
        plan: String,
    },

    /// Per-sub-agent roster snapshot for the session agent rail (F1).
    #[serde(rename = "agentRosterUpdated", rename_all = "camelCase")]
    AgentRosterUpdated {
        session_id: String,
        seq: i64,
        created_at: String,
        agents: Vec<AgentRosterRow>,
    },

    /// A deterministic pre-gate check finished (F3). `jury_skipped` is true when
    /// this (failing) check short-circuited the jury for the run.
    #[serde(rename = "checkRunCompleted", rename_all = "camelCase")]
    CheckRunCompleted {
        session_id: String,
        seq: i64,
        created_at: String,
        check_id: String,
        command: String,
        exit_code: i32,
        attempts: u32,
        duration_ms: u64,
        passed: bool,
        jury_skipped: bool,
    },

    /// A persistent Goal advanced or closed (F2). Emitted on every continuation
    /// and on close; `status` is the GoalState status string.
    #[serde(rename = "goalProgressUpdated", rename_all = "camelCase")]
    GoalProgressUpdated {
        session_id: String,
        seq: i64,
        created_at: String,
        #[serde(flatten)]
        snapshot: GoalSnapshotV1,
    },

    /// A self-review proposal generated by the background self-review fork.
    #[serde(rename = "selfReviewProposalReceived", rename_all = "camelCase")]
    SelfReviewProposalReceived {
        session_id: String,
        seq: i64,
        created_at: String,
        proposal: SelfReviewProposalDraft,
    },

    /// The capability/cost router (Lane R1a-L3/L4, ADR §D5) picked a model
    /// for a sub-agent spawn. Emitted at the `router_seam.rs` spawn seam
    /// alongside the existing `tracing::info!` line — never in place of it.
    /// `est_cost_usd` is a PRE-CALL estimate (the UI chip's "est." label is
    /// mandatory, never implied actuals); actual spend still reconciles
    /// through `UsageUpdated`.
    #[serde(rename = "modelRouteDecision", rename_all = "camelCase")]
    ModelRouteDecision {
        session_id: String,
        seq: i64,
        created_at: String,
        #[serde(flatten)]
        decision: ModelRouteDecisionPayload,
    },

    /// Run-governance typed failure. Emitted when the engine rejects a
    /// tool dispatch on a budget, ownership, or origin invariant (Lane B3/C3).
    /// The variant's payload is the typed counterexample — both the desktop
    /// UI and the event log can render the discriminator plus context.
    #[serde(rename = "runGovernanceRejected", rename_all = "camelCase")]
    RunGovernanceRejected {
        session_id: String,
        seq: i64,
        created_at: String,
        kind: super::types::RunFailureKind,
        tool_call_id: String,
        #[serde(default)]
        budget: Option<super::types::RunBudgetExhausted>,
        #[serde(default)]
        ownership: Option<super::types::RunOwnershipFailure>,
        #[serde(default)]
        origin: Option<super::types::RunOriginFailure>,
    },
}

fn default_cost_known() -> bool {
    true
}

fn default_worker_outcome() -> String {
    "complete".to_string()
}

mod methods;
