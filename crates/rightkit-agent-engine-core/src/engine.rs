//! `SessionEngine` — the agent loop.

use crate::conduit::Conduit;
use crate::default_compaction::build_default_summarizer_with_route_resolver;
use crate::engine::iteration_budget::IterationBudget;
use crate::failure::classify_failure;
use crate::goal_continuation::{update_goal_state, GOAL_CONTINUE_NUDGE};
use crate::guardian::{
    GuardianRequest, GuardianReviewLifecycle, GuardianReviewer, GuardianVerdict,
};
use crate::llm::{LlmClient, LlmRequestMetadata};
use crate::read_cache::ReadCache;
use crate::runtime_middleware_plumbing::{
    apply_middleware_arg_patch, merge_runtime_attributes, middleware_effect_key,
    middleware_matches_call, middleware_phase_key, synthetic_runtime_middleware,
    AppliedMiddlewareAction, AppliedMiddlewareDecision,
};
use crate::self_review::{global_self_review_fork, RestrictedSelfReviewFork, SelfReviewInput};
use crate::skill_plumbing::{
    skill_effect_denial_for_tool_call, skill_toolset_ceiling_observations, triggered_skill_hint,
};
use crate::tool_dispatch_plumbing::{
    enforce_generation_completeness_gate, parse_tool_arguments_attributed,
    strip_safety_terminated_tool_calls,
};
use crate::tool_policy::{
    evaluate_tool_guardrails, extract_command_arg, extract_path_arg, pre_tool_approval_reason,
    rule_target_for_call, ToolGuardrailAction, ToolGuardrailDecision,
};
use crate::tool_result_store::ToolResultStore;
use crate::transcript::{tool_result_trust, Transcript};
use crate::transcript_plumbing::{
    extract_view_images, inject_pending_view_images_with_state, is_worker_spawning_tool_call,
    repair_dangling_tool_calls, PendingViewImage,
};
use crate::usage::UsageAccount;
use crate::{cache::today_day, CacheStatus, CacheTracker};
// Re-export the helper functions from `engine_helpers` so child
// modules (which use `use super::*;`) can still call them by their
// short names. FEATURE-MATRIX par §#141 extraction.
#[allow(unused_imports)]
pub(crate) use engine_helpers::{now_iso, now_nanos, pinned_system_prefix_as_stable};
use rightkit_agent_engine_protocol::{
    MiddlewareDecisionEvidence, OutputRef, ProtocolEvent, ToolCall, TranscriptMessage,
};
use rightkit_agent_engine_runtime_middleware::{
    MiddlewareDefinition, MiddlewareEffect, MiddlewarePhase, RuntimeMiddlewareRegistry,
    RuntimeObservation, RuntimeSpan,
};
use rightkit_agent_policy::{
    parse_capability, PermissionDecision, PermissionEngine, PermissionMode, PermissionVerdict,
    RuleSource, RuleStore, WorkspaceTrust,
};
use rightkit_agent_tools::coding_tools::{
    SubagentSpawner, SubagentSupervisor, ToolContext, ToolRegistry, WebSearchConfig,
};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

fn canonical_json(value: serde_json::Value) -> String {
    serde_json::to_string(&value).expect("serde_json::Value serialization is infallible")
}

fn canonical_rule_revision(rules: &RuleStore) -> String {
    let mut records = rules
        .all_rules()
        .iter()
        .map(|rule| {
            canonical_json(serde_json::json!([
                "rule-v1",
                format!("{:?}", rule.source),
                format!("{:?}", rule.effect),
                rule.pattern,
                rule.literal,
            ]))
        })
        .collect::<Vec<_>>();
    records.sort_unstable();
    canonical_json(serde_json::json!(["rules-v1", records]))
}

#[path = "engine_config.rs"]
mod config;
#[path = "engine_loop.rs"]
mod engine_loop;
#[path = "engine_loop_completion.rs"]
mod engine_loop_completion;
#[path = "engine_loop_dispatch.rs"]
mod engine_loop_dispatch;
#[path = "engine_loop_max_iterations.rs"]
mod engine_loop_max_iterations;
#[path = "engine_loop_tool_activity.rs"]
mod engine_loop_tool_activity;
#[path = "iteration_budget.rs"]
pub(crate) mod iteration_budget;
#[path = "loop_detector_config.rs"]
pub(crate) mod loop_detector_config;
#[path = "engine_session_controls.rs"]
mod session_controls;
#[path = "tool_loop_idempotency.rs"]
mod tool_loop_idempotency;
use config::normalize_mcp_channel;
pub use config::SessionEngineConfig;
pub use loop_detector_config::LoopDetectorConfig;
pub use session_controls::retire_session_compaction_state;
pub(crate) use tool_loop_idempotency::IdempotencyAxis;
#[path = "engine_composition.rs"]
pub(crate) mod composition;
#[path = "engine_helpers.rs"]
mod engine_helpers;
#[path = "engine_goal_flow.rs"]
mod goal_flow;
#[path = "engine_permission_flow.rs"]
mod permission_flow;
#[path = "engine_prompt_cache.rs"]
mod prompt_cache_plumbing;
#[path = "engine_runtime_recording.rs"]
mod runtime_recording;
#[path = "stall_guardrail.rs"]
mod stall_guardrail;
#[path = "engine_tool_dispatch.rs"]
mod tool_dispatch;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("llm error: {0}")]
    Llm(String),
    #[error("max tool iterations exceeded ({0})")]
    MaxIterations(usize),
    #[error("run was interrupted")]
    Interrupted,
    #[error("compaction failed: {0}")]
    Compaction(String),
    #[error("compaction made no gain: context {tokens_before} -> {tokens_after} tokens; context retained")]
    CompactionNoGain {
        tokens_before: u64,
        tokens_after: u64,
    },
    #[error("run-change baseline capture failed: {0}")]
    RunChanges(String),
    #[error("assurance contract failed: {0}")]
    Assurance(String),
}

/// Result returned by the shared manual/automatic context-compaction path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionOutcome {
    pub compacted: bool,
    pub summary: String,
    pub dropped_count: usize,
    pub archive: Option<crate::compaction_archive::CompactionArchiveReceipt>,
}

/// ENG-73: demand-driven lifetime coordinator.
///
/// An on-device or daemon-hosted engine must exist only for owned local
/// sessions with an active turn or an open UI/session lease — nothing runs
/// indefinitely just because it was once opened. This coordinator holds the
/// facts (`is_idle`, `should_teardown`, `time_until_teardown`); it does not
/// itself tear anything down or restart anything. The embedding host (daemon
/// session-retention loop, Fly Machine autostop/autostart, or the iOS
/// standalone runtime) polls these facts and acts — restart-on-demand is
/// simply reconstructing a fresh `SessionEngine` on the next owned turn,
/// which this type neither performs nor prevents.
///
/// Kept intentionally free of any I/O, timers, or async so it stays testable
/// with `Instant` alone and safe to call from any lock-holding context.
#[derive(Debug, Clone)]
pub struct IdleGraceCoordinator {
    idle_grace: std::time::Duration,
    last_activity: std::time::Instant,
    active_leases: HashSet<String>,
    active_turn: bool,
}

impl IdleGraceCoordinator {
    /// Short bounded idle window before an owned-but-quiescent engine becomes
    /// eligible for teardown. Chosen short deliberately: ENG-73 requires
    /// "short idle grace", not a long-lived cache — a UI reattach or the next
    /// user turn simply reconstructs the engine, so holding it longer only
    /// costs memory/daemon-lifetime with no benefit.
    pub const DEFAULT_IDLE_GRACE: std::time::Duration = std::time::Duration::from_secs(90);

    pub fn new(idle_grace: std::time::Duration) -> Self {
        Self {
            idle_grace,
            last_activity: std::time::Instant::now(),
            active_leases: HashSet::new(),
            active_turn: false,
        }
    }

    /// Record activity now without changing turn/lease state — used for any
    /// engine-observable progress (e.g. a tool call landing) that should
    /// postpone teardown even mid-turn, where `begin_turn` already fired.
    pub fn note_activity(&mut self) {
        self.last_activity = std::time::Instant::now();
    }

    /// Mark a turn as started. Call at the start of every owned user turn;
    /// pair with `end_turn` when the turn concludes (success, failure, or
    /// interruption all count as concluding).
    pub fn begin_turn(&mut self) {
        self.active_turn = true;
        self.note_activity();
    }

    /// Mark the active turn as concluded. Idempotent when no turn is active.
    pub fn end_turn(&mut self) {
        self.active_turn = false;
        self.note_activity();
    }

    /// Acquire a named UI/session lease (e.g. a foreground view observing
    /// this session, or a daemon connection holding it open). While any
    /// lease is held the engine is never eligible for teardown, regardless
    /// of turn state.
    pub fn acquire_lease(&mut self, lease_id: impl Into<String>) {
        self.active_leases.insert(lease_id.into());
        self.note_activity();
    }

    /// Release a previously acquired lease. Releasing an unknown or already
    /// released id is a no-op.
    pub fn release_lease(&mut self, lease_id: &str) {
        self.active_leases.remove(lease_id);
        self.note_activity();
    }

    pub fn has_active_lease(&self) -> bool {
        !self.active_leases.is_empty()
    }

    pub fn is_active_turn(&self) -> bool {
        self.active_turn
    }

    /// True when there is no active turn and no held lease — the only state
    /// from which the idle-grace clock can expire.
    pub fn is_idle(&self) -> bool {
        !self.active_turn && self.active_leases.is_empty()
    }

    /// True once the engine has been continuously idle for at least
    /// `idle_grace`. Never true while a turn is active or a lease is held,
    /// however long ago the last edge into that state was.
    pub fn should_teardown(&self) -> bool {
        self.is_idle() && self.last_activity.elapsed() >= self.idle_grace
    }

    /// Remaining idle budget before teardown becomes eligible. `None` while
    /// not idle (turn active or lease held) — there is no meaningful
    /// countdown in that state. `Some(Duration::ZERO)` once the grace period
    /// has already elapsed.
    pub fn time_until_teardown(&self) -> Option<std::time::Duration> {
        if !self.is_idle() {
            return None;
        }
        let elapsed = self.last_activity.elapsed();
        if elapsed >= self.idle_grace {
            Some(std::time::Duration::ZERO)
        } else {
            Some(self.idle_grace - elapsed)
        }
    }
}

impl Default for IdleGraceCoordinator {
    fn default() -> Self {
        Self::new(Self::DEFAULT_IDLE_GRACE)
    }
}

/// Drives a single agent session.
pub struct SessionEngine {
    pub session_id: String,
    /// Stable identity for the currently executing user turn. A session can
    /// contain many runs; review inventory must never collapse them together.
    active_run_id: String,
    llm: Arc<dyn LlmClient>,
    tools: Arc<ToolRegistry>,
    conduit: Arc<Conduit>,
    transcript: Transcript,
    usage: UsageAccount,
    permission_mode: PermissionMode,
    workspace_trust: WorkspaceTrust,
    workspace_root: std::path::PathBuf,
    /// The host guarantees shell commands run under an enforcing OS sandbox (or not at all):
    /// AutoSafe then treats interpreters and test runners as routine. Set by the daemon.
    os_sandbox_enforced: bool,
    /// Session-latched highest-risk source that can influence a subsequent
    /// model-authored durable write. Kept outside the compacted transcript so
    /// summarization cannot launder provenance.
    content_provenance: rightkit_agent_policy::ContentProvenance,
    /// Fractional iteration budget (FEATURE-MATRIX par §#141). A
    /// non-programmatic turn costs 1.0; a programmatic turn
    /// (read-only tool chaining with no user-visible text) costs
    /// 0.25. When the budget hits zero the engine attempts one
    /// graceful summary turn before falling through to
    /// `RunFailed`. See `iteration_budget.rs` for the full design
    /// note and refund semantics.
    iteration_budget: IterationBudget,
    /// When true, run `lsp diagnostics` on edited files after each turn's edits
    /// and inject any error-level problems into the next turn. Default on; no-op
    /// without a language server. See `post_edit_diagnostics`.
    auto_post_edit_diagnostics: bool,
    max_worker_tool_calls: usize,
    budget_ceiling_usd: f64,
    interrupt_rx: Option<tokio::sync::watch::Receiver<bool>>,
    /// Live permission-mode override. When set, the engine refreshes its mode
    /// from this channel at the start of each turn so the UI can switch between
    /// Plan / Ask / Full-access without restarting the session.
    permission_mode_rx: Option<tokio::sync::watch::Receiver<PermissionMode>>,
    /// Live per-session permission rules. These refine the coarse permission
    /// mode without restarting the engine.
    permission_rules: Option<Arc<Mutex<RuleStore>>>,
    /// Live active-model compaction capability. The daemon updates this whenever
    /// a session model or provider fallback changes.
    compaction_policy_rx:
        Option<tokio::sync::watch::Receiver<crate::compaction_policy::CompactionModelPolicy>>,
    compaction_policy: crate::compaction_policy::CompactionModelPolicy,
    pending_approvals: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>>,
    pending_approval_contexts:
        Arc<Mutex<HashMap<String, rightkit_agent_engine_protocol::PendingApprovalContext>>>,
    /// EFF-002: the effect digest each granted approval actually authorized,
    /// keyed by tool-call id. Recomputed immediately before execution; a
    /// mismatch means the executable payload changed after the operator
    /// approved it, so the old approval can no longer authorize it.
    approved_effect_digests: Arc<Mutex<HashMap<String, String>>>,
    /// Call ids whose grant came from an approval prompt (not a policy rule).
    /// Such a grant must present its digest at execution; absence denies.
    approval_required_calls: Arc<Mutex<std::collections::HashSet<String>>>,
    external_approval_times: HashMap<String, std::time::Instant>,
    /// Session-owned deferred tool resolutions (ENG-86).
    deferred_tool_calls:
        Arc<rightkit_agent_tools::coding_tools::deferred_resolution::DeferredToolCallRegistry>,
    /// Capability retained by out-of-turn producers. It is bound to this
    /// session lifetime, so callbacks from a retired engine cannot reach a
    /// later engine that reuses the same visible session id.
    async_inbox_producer: rightkit_agent_tools::coding_tools::async_inbox::AsyncInboxProducer,
    /// Session-scoped registry of in-flight mid-turn credential requests
    /// (ENG-94). Mirrors `pending_approvals` exactly: constructed the same
    /// way, held behind the same `Arc<Mutex<..>>` shape, and intended to be
    /// shared with the daemon/FFI layer via `set_credential_request_store`
    /// (see `engine_session_controls.rs`) the same way `set_approval_store`
    /// shares `pending_approvals`.
    pending_credential_requests: Arc<Mutex<crate::credential_request::PendingCredentialRequests>>,
    cancel_tokens: Arc<Mutex<HashMap<String, tokio::sync::watch::Sender<bool>>>>,
    /// Steering messages are injected at the start of the next engine iteration.
    steer_rx: Option<tokio::sync::mpsc::Receiver<String>>,
    provider_boundary_rx: Option<crate::provider_turn_boundary::ProviderBoundaryReceiver>,
    provider_boundary_projection: crate::provider_turn_boundary::ProviderBoundaryProjection,
    compaction_snapshot_feed:
        Option<Arc<std::sync::RwLock<crate::compactor::CompactionSnapshotV1>>>,
    compaction_revision_values: Mutex<HashMap<String, (Option<String>, u64)>>,
    compaction_revision_notifier: Option<Arc<dyn Fn() + Send + Sync>>,
    compaction_decision_provider:
        Option<Arc<dyn crate::compaction_archive::CompactionDecisionProvider>>,
    /// Persistent goal context injected before every LLM call (shared with API).
    goal: Arc<Mutex<Option<String>>>,
    /// Shared budget-bounded goal state (F2). Always-present container (mirrors
    /// `goal`) so the daemon can set/clear the goal live without reconstructing
    /// the engine. When the inner `Some` is present + `continuation_enabled`, the
    /// engine continues toward the goal at turn-end instead of stopping, bounded
    /// by `GoalState::may_continue`. `tokens_used`/`cost_usd` are written by the
    /// daemon (rollup totals); read here.
    goal_state: Arc<Mutex<Option<crate::goal_state::GoalState>>>,
    /// Global kill switch for the goal continuation loop (daemon setting).
    continuation_enabled: bool,
    /// Optional sub-agent spawner for the `spawn_subagent` tool.
    spawner: Option<Arc<dyn SubagentSpawner>>,
    /// Host-owned identity-preserving child lifecycle control for wait & exact
    /// subtree interruption.
    subagent_supervisor: Option<Arc<dyn SubagentSupervisor>>,
    /// Provider/model attribution for repair telemetry (master-change 5.3).
    /// The engine seam itself only knows tool / repair kind / validator
    /// error / success; the daemon adapter that owns the session route
    /// injects the provider + model here so repair counters carry all six
    /// dimensions. `None` per slot records as unattributed.
    repair_attribution: Option<(Option<String>, Option<String>)>,
    /// Optional operator-question callback for the `ask_user` tool.
    questioner: Option<Arc<dyn rightkit_agent_tools::coding_tools::UserQuestioner>>,
    /// Per-session registry of background shells (shared into every ToolContext).
    #[cfg(feature = "host")]
    background_shells: rightkit_agent_tools::coding_tools::background_shell::BgStore,
    /// Per-session file-read receipts shared into every ToolContext.
    file_service: Arc<rightkit_agent_tools::coding_tools::FileService>,
    /// Per-session registry of browser sessions (shared into every ToolContext).
    #[cfg(feature = "host")]
    browser_sessions: rightkit_agent_tools::coding_tools::browser::BrowserStore,
    /// Per-session registry of durable/background task records.
    task_store: rightkit_agent_tools::coding_tools::task_tools::TaskStore,
    /// Optional plan-exit callback for the `exit_plan_mode` tool.
    plan_exit: Option<Arc<dyn rightkit_agent_tools::coding_tools::PlanExit>>,
    /// Optional live message dispatcher for the `send_message` tool.
    message_dispatcher: Option<Arc<dyn rightkit_agent_tools::coding_tools::MessageDispatcher>>,
    /// Optional live team directory for team tools.
    team_directory: Option<Arc<dyn rightkit_agent_tools::coding_tools::TeamDirectory>>,
    /// Authenticated host adapter for run_program's Membrane operations & Pull projection.
    #[cfg(feature = "host")]
    context_tool_dispatcher:
        Option<Arc<dyn rightkit_agent_tools::coding_tools::code_mode::ContextToolDispatcher>>,
    /// Optional web-search provider credentials shared into web_search tool contexts.
    web_search_config: Option<WebSearchConfig>,
    /// Image-provider keys supplied by a trusted host, keyed by image route
    /// provider slug. Kept outside text-provider routing and never exposed to
    /// model-visible tool input/output.
    image_provider_keys: HashMap<String, String>,
    /// Optional Cloudflare account credential shared into native deployment tool contexts.
    cloudflare_config: Option<rightkit_agent_tools::coding_tools::CloudflareConfig>,
    /// Optional forge credential shared into GitHub API & git push tool contexts.
    github_token: Option<String>,
    /// Optional MCP manager for the dynamic MCP tools.
    mcp: Option<Arc<rightkit_mcp::agent_mcp::McpManager>>,
    /// Active surface/channel used for MCP server policy checks.
    mcp_channel: String,
    /// Active named toolset profile for context-sensitive tool guards.
    toolset: Option<String>,
    /// Selected execution backend for process-spawning tools.
    execution_backend: Option<String>,
    /// Selected execution workspace id for backend-routed process-spawning tools.
    execution_workspace_id: Option<String>,
    /// Materialized execution workspace path for backend-routed process tools.
    execution_workspace_path: Option<std::path::PathBuf>,
    /// Selected network profile for process-spawning tools.
    execution_network_profile: Option<String>,
    /// Resource caps selected for backend-routed process tools.
    execution_resource_limits: Option<rightkit_agent_tools::coding_tools::ExecutionResourceLimits>,
    /// Catalog of configured execution backends so process tools can resolve an
    /// arbitrary backend id to its kind and refuse disabled backends.
    #[cfg(feature = "host")]
    execution_backend_registry:
        Option<Arc<rightkit_agent_tools::coding_tools::ExecutionBackendRegistry>>,
    /// Local skill directories used by the `invoke_skill` tool.
    skill_dirs: Vec<std::path::PathBuf>,
    /// Optional lifecycle hook bus. When set, the engine publishes
    /// UserPromptSubmit / PreToolUse / PostToolUse so subscribers (audit,
    /// user-configured hooks) can observe and react.
    hook_bus: Option<rightkit_hooks::agent_hooks::HookBus>,
    /// Set for sub-agent engines: completion runs `SubagentStop` hooks.
    subagent_role: Option<String>,
    /// SessionStart hooks fire once per engine, on the first user turn.
    session_start_hooks_fired: bool,
    /// Host-owned H2 receipt projection. Sentinel/Beacon consumes this
    /// append-only projection; the model cannot write to it.
    assurance_broker: Option<Arc<Mutex<crate::assurance_broker::AssuranceBroker>>>,
    assurance_context_injected: bool,
    sentinel_client: Option<Arc<tokio::sync::Mutex<crate::sentinel_client::SentinelClient>>>,
    sentinel_run_id: Option<String>,
    /// Optional per-tool guardian LLM reviewer (FEATURE-MATRIX §#87). OFF by
    /// default (`None`). When set, every MUTATING tool call dispatched through
    /// the sequential single-call path is reviewed exactly once before
    /// execution; a `Block` verdict denies the call through the same
    /// `record_tool_error` path used by guardrail/middleware denials.
    /// Read-only tools never reach the guardian.
    guardian: Option<Arc<dyn GuardianReviewer>>,
    /// Durable store for large tool results.
    tool_results: ToolResultStore,
    /// Directory backing durable large tool-result retrieval.
    tool_result_dir: std::path::PathBuf,
    /// Read-dedup cache to elide repeated unchanged file reads.
    read_cache: ReadCache,
    /// Full image data captured from image-capable tool results, injected only
    /// after the containing assistant tool-call block has been fully paired.
    pending_view_images: Vec<PendingViewImage>,
    /// Tracks stable system-prefix cache changes for cache-break observability.
    cache_tracker: CacheTracker,
    /// Donor-ported prompt-cache break detector (atom VG-001): snapshots every
    /// request, then attributes a cache-read collapse on the response.
    prompt_cache_break: crate::prompt_cache_break::PromptCacheBreakDetector,
    /// Optional shared runtime middleware registry used for local spans and
    /// operator-visible middleware evidence.
    runtime_middleware: Option<Arc<Mutex<RuntimeMiddlewareRegistry>>>,
    /// Reasoning effort hint passed to the LLM provider ("low" | "medium" | "high").
    pub effort: Option<String>,
    /// Active-model summarizer for authoritative context compaction. This is
    /// fallible by design: model failure must not silently discard context.
    summarizer: Arc<dyn crate::compactor::FallibleSummarizer>,
    /// Optional memory-block provider. When set, the compactor invokes it
    /// at every authoritative compaction and injects the fresh block
    /// immediately after the post-compaction continuation marker. The
    /// provider is invoked fresh every time — output is never cached.
    /// The bin wires its `session_memory_block` builder here so live
    /// compaction re-injects the same durable-memory view that
    /// turn-0 already established.
    memory_block_provider: Option<Arc<dyn crate::compactor::MemoryBlockProvider>>,
    /// Exact pre-compaction transcript sink. The daemon installs its durable
    /// SQLite implementation; archive failure aborts before context mutation.
    compaction_archive_sink: Option<Arc<dyn crate::compaction_archive::CompactionArchiveSink>>,
    /// FM-178: live session compaction lineage (ported codex `RolloutLineage`).
    /// Each real compaction pass appends the exact messages compacted away, so
    /// full visible history can be reconstructed independently of the
    /// compacted context.
    compaction_lineage: crate::compaction_archive::CompactionRolloutLineage,
    /// HCTX-004: monotonically increasing compaction epoch. Every successful
    /// compaction is an EXPECTED prompt-cache break and invalidates host-loaded
    /// context identities from earlier epochs; it is reported on the
    /// `sessionCompacted` event so cache accounting and Membrane can tell an
    /// expected break from an unexpected one.
    compaction_epoch: u64,
    /// Restricted async post-turn reviewer. It can only file quarantined
    /// learning proposals through the sink contract.
    self_review_fork: Option<Arc<dyn RestrictedSelfReviewFork>>,
    /// Completed visible turns since the last background self-review fork.
    turns_since_self_review: usize,
    /// Spawn the background self-review fork when `turns_since_self_review`
    /// reaches this threshold. Defaults to 5.
    self_review_turn_interval: usize,
    /// UTC day bucket for the stable-tier cache key (Hermes pattern).
    /// Captured once at session construction so the stable key stays
    /// invariant across turns on the same day. Tests inject a fixed day via
    /// `SessionEngineConfig::day_key` to keep test paths clock-free.
    pub day_key: String,
    /// Cross-iteration `repeated_call` counter (sessions rule 32): the fourth
    /// identical tool call in a row is an Ask. The flat `MAX_NO_PROGRESS=3`
    /// global counter stays as a separate goal-level safety net.
    pub stall_guardrail: crate::engine::stall_guardrail::StallGuardrail,
    /// Call ids the counter flagged as the fourth-or-later identical call in a
    /// row; the permission path turns each into an Ask and removes it.
    pub repeated_call_ids: std::collections::HashSet<String>,
    /// Context folding store for archiving sub-trajectories. When enabled,
    /// fold(subtask_id, summary) replaces sub-trajectory messages with the
    /// outcome summary; full sub-trajectory archived to disk for on-demand retrieval.
    /// OFF by default behind config until measured.
    context_fold: crate::context_fold::FoldStore,
    /// Whether context folding is enabled for this session.
    context_fold_enabled: bool,
    /// Unified loop-detector thresholds (parity P-3). All three
    /// loop-guard axes read from this single config block.
    pub loop_detector_config: LoopDetectorConfig,
    /// Idempotency-aware third axis of the loop guard (parity P-3).
    /// Owned by the engine so it persists across turns; reset on
    /// context-folding or a fresh session.
    pub idempotency_axis: IdempotencyAxis,
    /// Durable origin seam (Lane B3/C3 task 1). When set, every dispatched
    /// tool call must produce a successful origin write before the side
    /// effect fires. None means the engine refuses dispatch on every call —
    /// the bin wires a real sink before exposing the engine to user turns.
    origin_sink: Option<Arc<dyn crate::run_origin::RunOriginSink>>,
    /// Durable post-effect capture for the desktop review surface. Tool
    /// mutations are recorded before their visible result is published.
    run_changes_sink: Option<Arc<dyn crate::run_changes::RunChangesSink>>,
    /// Host-bound native Diagnostics transaction sink. Every production
    /// workspace mutation is bracketed by this sink before visible success.
    mutation_diagnostics: Option<Arc<dyn crate::mutation_diagnostics::MutationDiagnostics>>,
    /// Running count of typed `WorkspaceMutation`s this engine has produced
    /// on successful tool calls across its lifetime. The unbacked-victory
    /// gate in `finalize_goal` reads this so a victory claim without typed
    /// mutations is refused — even when the `CompletionInput` happens to
    /// carry `verification_approved = true`. Incremented by
    /// `record_single_tool_result` for every successful mutation; reset on
    /// session construction so a daemon restart starts a fresh count.
    typed_mutations_count: u32,
    /// Optional shared counter the engine bumps alongside its own internal
    /// count. The daemon installs one of these via
    /// `set_typed_mutations_counter` so `finalize_goal` can read the live
    /// count from `LiveSession::typed_mutations` without owning the engine.
    typed_mutations_shared: Option<std::sync::Arc<std::sync::atomic::AtomicU32>>,
    /// Shared denial ledger — every `PermissionVerdict::Deny` records
    /// tool/reason/session/timestamp synchronously via `DenialTracker`.
    denial_tracker: Option<Arc<Mutex<rightkit_agent_policy::DenialTracker>>>,
    /// ABAC PDP (FM-026). When `Some`, consulted before `RuleStore` at
    /// `engine_permission_flow.rs:32` (`permission_rules` seam). ABAC is
    /// first-match-wins via `from_yaml_like`, surfacing `abac:<rule-id>`.
    abac_pdp: Option<Arc<rightkit_agent_policy::PolicyDecisionPoint>>,
    /// Effective deny-glob lists (defaults + configured); `None` = defaults.
    deny_globs: Option<Arc<rightkit_agent_policy::DenyGlobSet>>,
    /// Read-time wire projection (P2-2 / mechanism 10). Borrows canonical
    /// history and returns the owned, possibly-elided payload that actually
    /// goes to the provider; canonical history is never mutated. Defaults to
    /// [`crate::compaction_archive::IdentityProjection`]; the daemon installs
    /// a [`crate::compaction_archive::DecayingObservationProjection`] for
    /// rolling observation decay.
    wire_projection: std::sync::Arc<dyn crate::compaction_archive::WireProjection>,
    internal_composition_call_ids: std::collections::HashSet<String>,
    #[cfg(feature = "host")]
    sandbox_retry_grants: HashMap<
        String,
        (
            String,
            rightkit_agent_tools::coding_tools::sandbox::SandboxGrant,
        ),
    >,
    /// ENG-73: demand-driven lifetime facts for this engine. Updated from
    /// the turn-boundary points already owned by this file
    /// (`begin_run_changes`); UI/session leases are acquired and released by
    /// the embedding host via `acquire_ui_lease`/`release_ui_lease`.
    idle_grace: IdleGraceCoordinator,
}

impl SessionEngine {
    pub fn set_compaction_snapshot_feed(
        &mut self,
        feed: Arc<std::sync::RwLock<crate::compactor::CompactionSnapshotV1>>,
    ) {
        self.compaction_snapshot_feed = Some(feed);
    }

    /// Attach the host's single retry-wake authority. The callback is invoked
    /// only after a failure-relevant revision actually changes.
    pub fn set_compaction_revision_notifier(&mut self, notifier: Arc<dyn Fn() + Send + Sync>) {
        self.compaction_revision_notifier = Some(notifier);
    }

    /// Record host-observed canonical authority state. Revisions advance only
    /// when supplied value changes; absent values remain unavailable.
    pub fn observe_host_revision(&self, kind: &str, value: Option<&str>) {
        let Some(feed) = self.compaction_snapshot_feed.as_ref() else {
            return;
        };
        let changed = {
            let Ok(mut snapshot) = feed.write() else {
                return;
            };
            let slot = match kind {
                "pin" => &mut snapshot.pin_revision,
                "model_route" => &mut snapshot.model_route_revision,
                "tool_catalog" => &mut snapshot.tool_catalog_revision,
                "promotion" => &mut snapshot.promotion_revision,
                "capacity" => &mut snapshot.capacity_revision,
                "reserve" => &mut snapshot.reserve_revision,
                "accounting" => &mut snapshot.accounting_revision,
                "federation" => &mut snapshot.federation_revision,
                "binding" => &mut snapshot.binding_revision,
                "h9" => &mut snapshot.h9_revision,
                "policy" => &mut snapshot.policy_revision,
                "repository_scope" => &mut snapshot.repository_scope_revision,
                _ => return,
            };
            let Ok(mut observed) = self.compaction_revision_values.lock() else {
                *slot = None;
                return;
            };
            let entry = observed.entry(kind.to_owned()).or_insert((None, 0));
            let value_changed = entry.0.as_deref() != value;
            if value_changed {
                let Some(next) = entry.1.checked_add(1) else {
                    *slot = None;
                    return;
                };
                entry.0 = value.map(str::to_owned);
                entry.1 = next;
            }
            let next_slot = value.map(|_| entry.1);
            let slot_changed = *slot != next_slot;
            *slot = next_slot;
            value_changed || slot_changed
        };
        if changed {
            if let Some(notifier) = &self.compaction_revision_notifier {
                notifier();
            }
        }
    }
    pub(super) fn observe_compaction_accounting(&self) {
        let accounting = canonical_json(serde_json::json!([
            "compaction-accounting-v1",
            self.usage.prompt_tokens,
            self.usage.completion_tokens,
            self.usage.context_tokens,
            self.usage.cache_read_tokens,
            self.usage.cache_creation_tokens,
            self.usage.window_tokens,
            self.usage.cost_usd.to_bits(),
            self.usage.is_priced(),
            self.budget_ceiling_usd,
        ]));
        self.observe_host_revision("accounting", Some(&accounting));
    }

    pub(super) fn bind_compaction_federation_revision(
        &self,
        expected: &crate::compactor::CompactionSnapshotV1,
        decision: &rightkit_agent_engine_protocol::CompactionFederationDecisionV1,
    ) -> Result<crate::compactor::CompactionSnapshotV1, String> {
        let key = serde_json::to_string(decision).map_err(|error| error.to_string())?;
        let feed = self
            .compaction_snapshot_feed
            .as_ref()
            .ok_or("compaction snapshot authority unavailable")?;
        let (snapshot, changed) = {
            let mut current = feed
                .write()
                .map_err(|_| "compaction snapshot authority poisoned")?;
            if !expected.failure_relevant_eq(&current) {
                return Err("compaction snapshot changed during federation admission".into());
            }
            let mut revisions = self
                .compaction_revision_values
                .lock()
                .map_err(|_| "compaction revision authority poisoned")?;
            let entry = revisions.entry("federation".into()).or_insert((None, 0));
            let value_changed = entry.0.as_deref() != Some(key.as_str());
            if value_changed {
                entry.1 = entry
                    .1
                    .checked_add(1)
                    .ok_or("compaction revision exhausted")?;
                entry.0 = Some(key);
            }
            let slot_changed = current.federation_revision != Some(entry.1);
            current.federation_revision = Some(entry.1);
            (current.clone(), value_changed || slot_changed)
        };
        if changed {
            if let Some(notifier) = &self.compaction_revision_notifier {
                notifier();
            }
        }
        Ok(snapshot)
    }

    /// Build the authoritative failure-relevant compaction snapshot from live
    /// session state. Revisions are content-addressed, so every material
    /// change advances its relevant field without inventing a second authority.
    pub fn compaction_snapshot(&self) -> crate::compactor::CompactionSnapshotV1 {
        let messages = self.transcript.messages();
        let digest = crate::compaction_archive::transcript_digest(messages);
        crate::compactor::CompactionSnapshotV1 {
            request_generation: self.conduit.current_high_water().max(0) as u64,
            transcript_high_water: self.conduit.current_high_water(),
            transcript_digest: digest.clone(),
            pin_revision: None,
            model_route_revision: None,
            tool_catalog_revision: Some(self.provider_boundary_projection.tool_catalog_revision()),
            promotion_revision: None,
            capacity_revision: None,
            reserve_revision: None,
            accounting_revision: None,
            federation_revision: None,
            // Canonical Membrane binding is host-owned and distinct from MCP
            // manager lifecycle; leave unavailable until host feed binds it.
            binding_revision: None,
            h9_revision: None,
            policy_revision: None,
            repository_scope_revision: None,
            decision_expires_at_unix_ms: u64::MAX,
        }
    }
    /// Install host-composed mutation authority shared by engine tools & undo.
    pub fn set_file_service(
        &mut self,
        file_service: Arc<rightkit_agent_tools::coding_tools::FileService>,
    ) {
        self.file_service = file_service;
    }

    /// Expose the host assembled mutation authority for adjacent daemon
    /// projections that must share its locks, revisions, & receipts.
    pub fn file_service(&self) -> Arc<rightkit_agent_tools::coding_tools::FileService> {
        self.file_service.clone()
    }

    pub fn set_self_review_fork(&mut self, fork: Arc<dyn RestrictedSelfReviewFork>) {
        self.self_review_fork = Some(fork);
    }

    pub fn set_self_review_turn_interval(&mut self, turns: usize) {
        self.self_review_turn_interval = turns.max(1);
    }

    /// Install the durable origin seam (Lane B3/C3 task 1). The engine calls
    /// this from `dispatch_tool_call` BEFORE the tool side effect, so any
    /// call that doesn't produce a durable origin row cannot reach the side
    /// effect. Tests install an `InMemoryOriginSink`; the bin wires a
    /// SQLite-backed sink at session-creation time.
    pub fn set_run_origin_sink(&mut self, sink: Arc<dyn crate::run_origin::RunOriginSink>) {
        self.origin_sink = Some(sink);
    }

    /// Returns the currently-installed origin sink, if any.
    pub fn origin_sink(&self) -> Option<Arc<dyn crate::run_origin::RunOriginSink>> {
        self.origin_sink.clone()
    }

    /// Install the daemon-owned run-change recorder. Unlike origin capture,
    /// this runs after a tool's side effect and records the typed mutations the
    /// tool proved actually landed.
    pub fn set_run_changes_sink(&mut self, sink: Arc<dyn crate::run_changes::RunChangesSink>) {
        self.run_changes_sink = Some(sink);
    }

    pub fn set_mutation_diagnostics(
        &mut self,
        diagnostics: Arc<dyn crate::mutation_diagnostics::MutationDiagnostics>,
    ) {
        self.mutation_diagnostics = Some(diagnostics);
    }

    pub fn active_run_id(&self) -> &str {
        &self.active_run_id
    }

    fn begin_run_changes(&mut self) -> Result<(), String> {
        self.active_run_id = format!(
            "{}--{}",
            self.session_id,
            rightkit_agent_engine_protocol::uuid_v7()
        );
        // ENG-73: a new run beginning is the clearest "active turn" edge this
        // file owns directly; postpones idle-grace teardown eligibility for
        // the duration of the run. The caller marks the run's end via
        // `end_active_run` once the turn concludes (success, failure, or
        // interruption all count).
        self.idle_grace.begin_turn();
        let Some(sink) = self.run_changes_sink.as_ref() else {
            return Err("no run-changes sink wired".to_string());
        };
        sink.begin_run(&self.session_id, &self.active_run_id, &self.workspace_root)
    }

    /// Mark the currently active run/turn as concluded for idle-grace
    /// purposes (ENG-73). Callers in `engine_loop.rs` should call this at
    /// every turn-exit point (`Ok`, `Err`, and interruption) once wired; see
    /// the lane report for the exact call sites this file's allowlist does
    /// not include.
    pub fn end_active_run(&mut self) {
        self.idle_grace.end_turn();
    }

    /// A196: when another session recently edited a file this call changed,
    /// return a one-line note for the tool result (the TUI host surfaces the
    /// same line as a system message). Also logs and records a runtime span.
    pub(crate) fn file_collision_note(
        &self,
        call: &rightkit_agent_engine_protocol::ToolCall,
        result: &rightkit_agent_tools::coding_tools::ToolResult,
    ) -> Option<String> {
        if result.is_error || result.mutations.is_empty() {
            return None;
        }
        let mut lines = Vec::new();
        for m in &result.mutations {
            for collision in crate::file_collision::FileCollisionGuard::global()
                .record_edit(&self.session_id, &m.path)
            {
                let message = collision.message();
                tracing::warn!(target: "file_collision", "{message}");
                self.record_runtime_span(
                    MiddlewarePhase::PostToolCall,
                    "file_collision",
                    Some(&call.id),
                    None,
                    serde_json::json!({
                        "surface": "file_collision",
                        "path": collision.path.display().to_string(),
                        "otherSession": collision.other_session,
                        "message": message,
                    }),
                );
                lines.push(format!("[{message}]"));
            }
        }
        if lines.is_empty() {
            None
        } else {
            Some(lines.join("\n"))
        }
    }

    fn record_run_changes(
        &self,
        call: &rightkit_agent_engine_protocol::ToolCall,
        result: &rightkit_agent_tools::coding_tools::ToolResult,
    ) -> Result<(), String> {
        if result.mutations.is_empty() {
            return Ok(());
        }
        let Some(sink) = self.run_changes_sink.as_ref() else {
            return Err("no run-changes sink wired".to_string());
        };
        let outcome = sink.record_mutations(
            &self.session_id,
            &self.active_run_id,
            &self.workspace_root,
            &call.id,
            &result.mutations,
            result.is_error,
        );
        // EFF-001: retain one composition trace edge from governed tool
        // execution into the daemon-owned mutation/review surface. The same
        // run/tool identities are consumed by downstream capture & undo.
        self.record_runtime_span(
            MiddlewarePhase::PostToolCall,
            if outcome.is_ok() {
                "mutation_captured"
            } else {
                "mutation_capture_error"
            },
            Some(&call.id),
            None,
            serde_json::json!({
                "surface": "run_changes",
                "runId": self.active_run_id(),
                "toolCallId": call.id,
                "mutationCount": result.mutations.len(),
                "toolFailed": result.is_error,
                "captureError": outcome.as_ref().err(),
            }),
        );
        outcome
    }

    /// Number of typed `WorkspaceMutation`s the engine has produced on
    /// successful tool calls across its lifetime. The unbacked-victory
    /// gate in `finalize_goal` consults this via the engine handle stored
    /// on `LiveSession`, so a victory claim without typed mutations is
    /// refused — even when the `CompletionInput` happens to carry
    /// `verification_approved = true`. Counts only `is_error == false`
    /// mutations; an errored tool call contributes zero.
    pub fn typed_mutations_count(&self) -> u32 {
        self.typed_mutations_count
    }

    /// Increment the typed-mutation counter by `n`. Called from the
    /// post-execution result recorder for every successful mutation so the
    /// gate sees the same evidence the post-edit diagnostic does. When the
    /// daemon has installed a shared counter via `set_typed_mutations_counter`,
    /// that shared counter is incremented in lockstep so the daemon's
    /// `finalize_goal` can read the live count without owning the engine.
    pub(crate) fn add_typed_mutations(&mut self, n: u32) {
        self.typed_mutations_count = self.typed_mutations_count.saturating_add(n);
        if let Some(shared) = self.typed_mutations_shared.as_ref() {
            shared.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Install a shared counter the engine bumps alongside its own internal
    /// count on every successful mutation. The daemon reads the shared
    /// counter from `LiveSession::typed_mutations` in `finalize_goal` so the
    /// unbacked-victory gate sees real evidence instead of a hardcoded 0.
    /// `None` (default) is fine — only the in-process counter is then
    /// updated, which is the right behavior for tests that don't need the
    /// cross-task seam.
    pub fn set_typed_mutations_counter(
        &mut self,
        shared: std::sync::Arc<std::sync::atomic::AtomicU32>,
    ) {
        // Sync the shared counter to the engine's current count so a daemon
        // restart that reads the counter before any new mutation sees the
        // same number the engine does.
        self.typed_mutations_count = self
            .typed_mutations_count
            .max(shared.load(std::sync::atomic::Ordering::Relaxed));
        shared.store(
            self.typed_mutations_count,
            std::sync::atomic::Ordering::Relaxed,
        );
        self.typed_mutations_shared = Some(shared);
    }

    pub fn set_denial_tracker(
        &mut self,
        tracker: Arc<Mutex<rightkit_agent_policy::DenialTracker>>,
    ) {
        self.denial_tracker = Some(tracker);
    }

    pub fn denial_tracker(&self) -> Option<Arc<Mutex<rightkit_agent_policy::DenialTracker>>> {
        self.denial_tracker.clone()
    }

    pub fn set_abac_pdp(&mut self, pdp: Arc<rightkit_agent_policy::PolicyDecisionPoint>) {
        self.abac_pdp = Some(pdp);
    }

    pub fn abac_pdp(&self) -> Option<Arc<rightkit_agent_policy::PolicyDecisionPoint>> {
        self.abac_pdp.clone()
    }

    /// The capability class of a call: the tool's declared class, else name inference only when
    /// the tool declares none. A renamed or newly added tool is never mis-gated by the name table.
    pub(crate) fn capability_for_call(
        &self,
        call: &ToolCall,
    ) -> rightkit_agent_policy::CapabilityClass {
        self.tools
            .get(&call.name)
            .and_then(|tool| {
                serde_json::from_str(&call.arguments_json)
                    .ok()
                    .and_then(|args| tool.capability_class_for(&args))
                    .or_else(|| tool.capability_class())
            })
            .unwrap_or_else(|| parse_capability(Some(&call.name)))
    }

    /// Await a durable origin write for `call`. Returns the seam's typed
    /// outcome; the dispatch site uses [`crate::run_origin::decide_after_origin`]
    /// to choose `Execute`, `DeferUntilReady`, or `Abort`. When no sink is
    /// installed the seam refuses (deterministic `Failed`) so the dispatch
    /// site can fail closed.
    pub fn record_run_origin(
        &self,
        call: &rightkit_agent_engine_protocol::ToolCall,
    ) -> crate::run_origin::OriginWriteResult {
        if crate::run_origin::is_noop_probe(call) {
            return crate::run_origin::OriginWriteResult::Skipped;
        }
        let Some(sink) = self.origin_sink.as_ref() else {
            return crate::run_origin::OriginWriteResult::Failed(
                "no origin sink wired".to_string(),
            );
        };
        sink.record_origin(call)
    }

    pub async fn record_run_origin_event(
        &self,
        event: &rightkit_agent_engine_protocol::ProtocolEvent,
    ) -> crate::run_origin::OriginWriteResult {
        let Some(sink) = self.origin_sink.as_ref() else {
            return crate::run_origin::OriginWriteResult::Failed(
                "no origin sink wired".to_string(),
            );
        };
        sink.record_event_async(event).await
    }

    /// Set provider/model attribution for repair telemetry (master-change
    /// 5.3). Called by the daemon adapter that owns the session route; the
    /// repair seam stamps every recorded repair/validation outcome with it.
    /// `None` in either slot records as unattributed for that dimension.
    pub fn set_repair_attribution(&mut self, provider: Option<String>, model: Option<String>) {
        self.repair_attribution = match (&provider, &model) {
            (None, None) => None,
            _ => Some((provider, model)),
        };
    }

    fn maybe_spawn_self_review_fork(
        &mut self,
        assistant_text: String,
        stable_system_cache_key: Option<String>,
    ) {
        self.turns_since_self_review = self.turns_since_self_review.saturating_add(1);
        if self.turns_since_self_review < self.self_review_turn_interval {
            return;
        }
        self.turns_since_self_review = 0;

        let Some(fork) = self.self_review_fork.clone() else {
            return;
        };
        let input = SelfReviewInput::from_transcript(
            self.session_id.clone(),
            &self.transcript,
            assistant_text,
            stable_system_cache_key,
        );
        tokio::spawn(async move {
            fork.review_and_propose(input);
        });
    }

    pub fn new(
        llm: Arc<dyn LlmClient>,
        tools: Arc<ToolRegistry>,
        conduit: Arc<Conduit>,
        config: SessionEngineConfig,
    ) -> Self {
        // A195: the provider only ever sees placeholders for secrets; replies
        // are restored before the engine (and the user) see them.
        let llm: Arc<dyn LlmClient> = Arc::new(crate::secret_redaction::RedactingLlm::new(
            llm,
            Arc::new(crate::secret_redaction::SecretRedactor::new()),
        ));
        let async_inbox_producer =
            rightkit_agent_tools::coding_tools::async_inbox::activate_session(&config.session_id);
        let workspace_root = config.workspace_root;
        let assurance_broker_path = workspace_root
            .join(".rightkit")
            .join("sentinel")
            .join("broker.jsonl");
        let skill_dirs = config.skill_dirs;
        let tool_result_dir = workspace_root.join(".rightkit").join("tool-results");
        #[cfg(feature = "host")]
        let background_shells = config
            .background_shells
            .unwrap_or_else(|| Arc::new(Mutex::new(std::collections::HashMap::new())));
        #[cfg(feature = "host")]
        let browser_sessions = config
            .browser_sessions
            .unwrap_or_else(|| Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())));
        let task_store = config
            .task_store
            .unwrap_or_else(|| Arc::new(Mutex::new(std::collections::HashMap::new())));
        // Default to an LLM-backed summarizer built from the session model so
        // compaction never silently degrades to the lossy statistical stub.
        let default_summarizer = build_default_summarizer_with_route_resolver(
            llm.clone(),
            config.compaction_route_resolver,
        );
        Self {
            session_id: config.session_id,
            active_run_id: String::new(),
            llm,
            tools,
            conduit,
            transcript: Transcript::new(),
            usage: UsageAccount::new(0.0, 0.0),
            permission_mode: config.permission_mode,
            workspace_trust: config.workspace_trust,
            workspace_root,
            os_sandbox_enforced: false,
            content_provenance: rightkit_agent_policy::ContentProvenance::session(),
            iteration_budget: IterationBudget::new(config.max_iterations),
            auto_post_edit_diagnostics: config.auto_post_edit_diagnostics,
            max_worker_tool_calls: config.max_worker_tool_calls,
            budget_ceiling_usd: config.budget_ceiling_usd,
            interrupt_rx: None,
            permission_mode_rx: None,
            permission_rules: config.permission_rules,
            compaction_policy_rx: None,
            compaction_policy: crate::compaction_policy::CompactionModelPolicy::default(),
            pending_approvals: Arc::new(Mutex::new(HashMap::new())),
            pending_approval_contexts: Arc::new(Mutex::new(HashMap::new())),
            approved_effect_digests: Arc::new(Mutex::new(HashMap::new())),
            approval_required_calls: Arc::new(Mutex::new(std::collections::HashSet::new())),
            external_approval_times: HashMap::new(),
            deferred_tool_calls: Arc::new(
                rightkit_agent_tools::coding_tools::deferred_resolution::DeferredToolCallRegistry::new(),
            ),
            async_inbox_producer,
            pending_credential_requests: Arc::new(Mutex::new(
                crate::credential_request::PendingCredentialRequests::new(),
            )),
            cancel_tokens: Arc::new(Mutex::new(HashMap::new())),
            steer_rx: None,
            provider_boundary_rx: None,
            provider_boundary_projection: Default::default(),
            compaction_snapshot_feed: None,
            compaction_revision_values: Mutex::new(HashMap::new()),
            compaction_revision_notifier: None,
            goal: Arc::new(Mutex::new(None)),
            goal_state: Arc::new(Mutex::new(None)),
            continuation_enabled: false,
            spawner: None,
            subagent_supervisor: None,
            repair_attribution: None,
            questioner: None,
            #[cfg(feature = "host")]
            background_shells,
            file_service: rightkit_agent_tools::coding_tools::FileService::new(),
            #[cfg(feature = "host")]
            browser_sessions,
            task_store,
            plan_exit: None,
            message_dispatcher: None,
            team_directory: None,
            #[cfg(feature = "host")]
            context_tool_dispatcher: None,
            web_search_config: None,
            image_provider_keys: HashMap::new(),
            cloudflare_config: None,
            github_token: None,
            mcp: None,
            mcp_channel: normalize_mcp_channel(config.mcp_channel),
            toolset: config.toolset,
            execution_backend: config.execution_backend,
            execution_workspace_id: config.execution_workspace_id,
            execution_workspace_path: config.execution_workspace_path,
            execution_network_profile: config.execution_network_profile,
            execution_resource_limits: config.execution_resource_limits,
            #[cfg(feature = "host")]
            execution_backend_registry: config.execution_backend_registry,
            skill_dirs,
            hook_bus: None,
            subagent_role: None,
            session_start_hooks_fired: false,
            assurance_broker: {
                {
                    crate::assurance_broker::AssuranceBroker::open(assurance_broker_path)
                        .ok()
                        .map(|broker| Arc::new(Mutex::new(broker)))
                }
            },
            assurance_context_injected: false,
            sentinel_client: None,
            sentinel_run_id: None,
            guardian: None,
            tool_results: ToolResultStore::new().with_dir(tool_result_dir.clone()),
            tool_result_dir: tool_result_dir.clone(),
            context_fold: crate::context_fold::FoldStore::new().with_dir(
                config
                    .fold_archive_dir
                    .unwrap_or_else(|| tool_result_dir.clone()),
            ),
            context_fold_enabled: config.context_fold_enabled,
            read_cache: ReadCache::new(),
            pending_view_images: Vec::new(),
            cache_tracker: CacheTracker::new(),
            prompt_cache_break: Default::default(),
            runtime_middleware: config.runtime_middleware,
            effort: config.effort,
            summarizer: default_summarizer,
            memory_block_provider: None,
            compaction_archive_sink: None,
            compaction_decision_provider: None,
            compaction_lineage: Default::default(),
            compaction_epoch: 0,
            self_review_fork: global_self_review_fork(),
            turns_since_self_review: 0,
            self_review_turn_interval: crate::self_review::DEFAULT_SELF_REVIEW_TURN_INTERVAL,
            // `repeated_call` counter; the engine resets it on progress.
            stall_guardrail: crate::engine::stall_guardrail::StallGuardrail::default(),
            repeated_call_ids: std::collections::HashSet::new(),
            // Empty config.day_key → fill from the wall clock here (the only
            // site that does so). Tests pre-set `cfg.day_key` to avoid this.
            day_key: if config.day_key.is_empty() {
                today_day()
            } else {
                config.day_key
            },
            // Unified loop-detector thresholds (parity P-3). Defaults
            // reproduce pre-P-3 hard-coded call sites byte-identically.
            loop_detector_config: config.loop_detector_config.clone(),
            // Idempotency-aware third axis (parity P-3). Window size
            // matches the legacy `ToolLoopDetector` window so the
            // bookkeeping stays in step with the existing counters.
            idempotency_axis: IdempotencyAxis::new(
                config.loop_detector_config.mutating_repeat_warn_threshold,
                config.loop_detector_config.read_only_repeat_warn_threshold,
                config.loop_detector_config.same_name_varying_warn_threshold,
                config.loop_detector_config.tool_loop_window_size,
            ),
            origin_sink: {
                {
                    None
                }
            },
            run_changes_sink: {
                {
                    None
                }
            },
            mutation_diagnostics: None,
            typed_mutations_count: 0,
            typed_mutations_shared: None,
            denial_tracker: config.denial_tracker,
            abac_pdp: config.abac_pdp,
            deny_globs: config.deny_globs,
            wire_projection: std::sync::Arc::new(crate::compaction_archive::IdentityProjection),
            internal_composition_call_ids: std::collections::HashSet::new(),
            #[cfg(feature = "host")]
            sandbox_retry_grants: HashMap::new(),
            idle_grace: IdleGraceCoordinator::default(),
        }
    }

    /// Install a non-default idle-grace window. Tests use a short window to
    /// avoid sleeping; hosts may widen or narrow the default per deployment
    /// (e.g. a Fly Machine may want a slightly longer grace than an on-device
    /// session, to absorb one reconnect blip without a full daemon restart).
    pub fn set_idle_grace_window(&mut self, idle_grace: std::time::Duration) {
        self.idle_grace = IdleGraceCoordinator::new(idle_grace);
    }

    /// Read-only view of this engine's demand-driven lifetime facts
    /// (ENG-73). The embedding host polls this to decide whether to tear the
    /// engine down; it is never mutated through this accessor.
    pub fn idle_grace(&self) -> &IdleGraceCoordinator {
        &self.idle_grace
    }

    /// Acquire a named UI/session lease that keeps this engine alive
    /// regardless of turn state — e.g. a foreground view actively observing
    /// the session, or a daemon connection holding it open on behalf of a
    /// paired client. Release with `release_ui_lease` using the same id.
    pub fn acquire_ui_lease(&mut self, lease_id: impl Into<String>) {
        self.idle_grace.acquire_lease(lease_id);
    }

    /// Release a previously acquired UI/session lease. A no-op if the id was
    /// never held or was already released.
    pub fn release_ui_lease(&mut self, lease_id: &str) {
        self.idle_grace.release_lease(lease_id);
    }

    /// True once this engine has been idle (no active turn, no held lease)
    /// for at least its configured idle-grace window. The engine does not
    /// tear itself down on this signal; the embedding host owns that
    /// decision and the actual drop/reconstruct.
    pub fn should_teardown(&self) -> bool {
        self.idle_grace.should_teardown()
    }

    /// Remaining idle budget before `should_teardown` becomes true. `None`
    /// while a turn is active or a lease is held.
    pub fn time_until_teardown(&self) -> Option<std::time::Duration> {
        self.idle_grace.time_until_teardown()
    }

    /// Install the read-time wire projection. Canonical history is never
    /// mutated by any projection; the payload sent to the provider is a
    /// projection over it. The daemon installs the rolling-decay projection
    /// (P2-2); tests and the default keep the identity projection.
    /// Declare that this host runs every shell command under an enforcing OS sandbox.
    pub fn set_os_sandbox_enforced(&mut self, enforced: bool) {
        self.os_sandbox_enforced = enforced;
    }

    pub fn set_wire_projection(
        &mut self,
        projection: std::sync::Arc<dyn crate::compaction_archive::WireProjection>,
    ) {
        self.wire_projection = projection;
    }

    /// Fold a sub-trajectory in the transcript.
    ///
    /// When context folding is enabled, this replaces the sub-trajectory between
    /// `start_marker` and `end_marker` with a summary message. The full sub-trajectory
    /// is archived to disk and retrievable via the archive ref.
    ///
    /// Returns the number of messages folded, or 0 if folding is disabled or markers not found.
    ///
    /// ## Prefix-cache discipline
    ///
    /// Folding only operates on messages after the stable prefix boundary.
    pub fn fold_subtrajectory(
        &mut self,
        subtask_id: &str,
        start_marker: &str,
        end_marker: &str,
        summary: String,
    ) -> Result<crate::context_fold::FoldedTrajectory, crate::context_fold::ContextFoldError> {
        if !self.context_fold_enabled {
            return Err(crate::context_fold::ContextFoldError::Disabled);
        }

        // The stable prefix length is determined by the pinned system messages.
        // We don't fold anything within the stable prefix to preserve cache keys.
        let stable_prefix_len = self
            .transcript
            .messages()
            .iter()
            .take_while(|m| match m {
                TranscriptMessage::System { pinned, .. } => *pinned,
                _ => false,
            })
            .count();

        crate::context_fold::fold_subtrajectory(
            &mut self.transcript,
            &mut self.context_fold,
            subtask_id,
            start_marker,
            end_marker,
            summary,
            stable_prefix_len,
        )
    }

    /// Retrieve an archived sub-trajectory by reference id.
    pub fn get_folded_trajectory(
        &self,
        ref_id: &str,
    ) -> Result<Vec<TranscriptMessage>, crate::context_fold::ContextFoldError> {
        self.context_fold.get(ref_id)
    }

    /// Drop approval-required markers whose request has resolved. A marker
    /// still backed by an outstanding digest is kept (the grant is live); a
    /// marker with no digest was consumed, so its replay-denial duty is over
    /// once the turn that raised it ends. Never removes a live grant's marker.
    pub(crate) fn prune_resolved_approval_markers(&self) {
        let digests = self.approved_effect_digests.lock().unwrap();
        self.approval_required_calls
            .lock()
            .unwrap()
            .retain(|id| digests.contains_key(id));
    }

    /// Clear a request's marker and digest when it resolves without running
    /// (Deny or timeout). Removing a digest only tightens the deny path.
    pub(crate) fn clear_unapproved_request(&self, call_id: &str) {
        self.approved_effect_digests.lock().unwrap().remove(call_id);
        self.approval_required_calls.lock().unwrap().remove(call_id);
    }
}

// `now_iso`, `now_nanos`, and `pinned_system_prefix_as_stable` were
// extracted to `engine_helpers.rs`
// (FEATURE-MATRIX par §#141) to keep `engine.rs` under the 400-line
// convention. Call sites are unchanged because the helpers inherit
// `use super::*` from the engine module.
