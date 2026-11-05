#![allow(
    clippy::int_plus_one,
    clippy::empty_line_after_doc_comments,
    clippy::too_many_arguments,
    clippy::unnecessary_map_or,
    clippy::derivable_impls,
    clippy::doc_lazy_continuation,
    clippy::single_match,
    clippy::redundant_closure
)]

//! `host_engine` — the agent loop.
//!
//! `SessionEngine` drives a single agent session: it receives user messages, calls the LLM,
//! dispatches tool calls through the permission gate, records the transcript, handles
//! compaction, and emits events through the conduit. The LLM is abstracted behind the
//! [`LlmClient`] trait so the loop is testable without a network.

pub mod adapt_consumer;
pub mod agents;
pub mod assurance_broker;
pub mod cache;
pub mod compaction_archive;
pub mod compaction_coordinator;
pub mod compaction_policy;
pub mod compaction_tuning;
pub mod compactor;
pub mod completion_gate;
pub mod conduit;
pub mod context_fold;
pub mod credential_request;
mod default_compaction;
pub use default_compaction::CompactionModelRouteResolver;
pub mod engine;
pub mod failure;
pub mod file_collision;
mod goal_continuation;
pub mod goal_state;
pub mod guardian;
pub mod image_eviction;
pub mod kinds;
pub mod lineage_budget;
pub mod llm;
pub mod mop;
pub mod mutation_diagnostics;
pub mod overflow_recovery;
pub mod pinned_context;
pub mod post_edit;
mod post_turn_gate;
pub mod prompt_cache_break;
pub mod provider_turn_boundary;
pub mod read_cache;
pub mod run_changes;
pub mod run_origin;
mod runtime_middleware_plumbing;
pub mod secret_redaction;
pub mod self_review;
pub mod sentinel_client;
pub mod session_state_summary;
mod skill_plumbing;
mod stall_strategy;
pub mod tool_await;
mod tool_dispatch_plumbing;
#[cfg(feature = "qualification")]
pub mod qualification_trace {
    pub use crate::tool_dispatch_plumbing::{EffectAdmissionStage, EffectAdmissionTrace};
    pub fn snapshot() -> &'static EffectAdmissionTrace {
        crate::tool_dispatch_plumbing::production_admission_trace()
    }
}
pub mod tool_output_stream;
mod tool_policy;
pub mod tool_result_store;
pub mod transcript;
mod transcript_plumbing;
pub mod usage;
pub mod verify_fanout;

pub use cache::{
    system_prompt_only_cache_key, CacheStatus, CacheTracker, ConductorStableKey, StableCacheKey,
    CONDUCTOR_CACHE_KEY_PREFIX,
};
pub use compaction_archive::{
    CompactionArchiveInput, CompactionArchiveReceipt, CompactionArchiveSink,
    CompactionDecisionProvider, CompactionFederationDecisionV1,
};
pub use compaction_policy::{
    ensure_context_fits, CompactionAction, CompactionModelPolicy, DEFAULT_CRITICAL_LEAD_PERCENT,
    IMAGE_TOKEN_COST,
};
pub use compaction_tuning::ObservationMasker;
pub use compactor::{
    CompactError, CompactionCommandKindV1, CompactionCommandV1, CompactionFailureClassV1,
    CompactionResult, CompactionSnapshotV1, CompactionUsage, Compactor, FallibleSummarizer,
    LlmSummarizer, MemoryBlockProvider, SessionRef, StatisticalSummarizer, Summarizer,
};
pub use conduit::{Conduit, ConduitReceive, ConduitReplay, ConduitSubscription};
pub use context_fold::{
    fold_subtrajectory, read_folded_trajectory, session_archive_dir, ContextFoldError, FoldStore,
    FoldedTrajectory,
};
pub use engine::{retire_session_compaction_state, CompactionOutcome, EngineError, SessionEngine};
pub use failure::{classify_failure, FailureKind};
pub use goal_state::{
    ContinueDecision, GoalGapFingerprint, GoalGapFingerprintClass, GoalGapObservation,
    GoalGapTracker, GoalState, UnresolvedGoalGap,
};
pub use guardian::{GuardianRequest, GuardianReviewer, GuardianVerdict};
pub use image_eviction::{
    prepare_image_history, prepare_image_history_with_accounting, ImageAccountingBasis,
    ImageBudgetEvidence, ImageBudgetPolicy, ImageProviderProfile, ImageRequestAccounting,
    PreparedImageHistory, IMAGE_HISTORY_RECLAIM_TARGET_BYTES, IMAGE_HISTORY_TRIGGER_BYTES,
};
pub use kinds::{parse_artifact_kind, ArtifactKind};
pub use lineage_budget::{usd_to_microusd, LineageBudget, LineageReservation};
pub use llm::{LlmClient, LlmRequestMetadata, LlmResponse, ScriptedLlm};
pub use mutation_diagnostics::{
    MutationDiagnostics, MutationDiagnosticsFailure, MutationDiagnosticsFailureKind,
};
pub use read_cache::ReadCache;
pub use run_changes::{InMemoryRunChangesSink, RunChangesSink};
pub use run_origin::{
    decide_after_origin, is_noop_probe, stable_call_fingerprint, InMemoryOriginSink,
    NoopOriginSink, OriginWriteResult, PreEffectDecision, RunOriginSink, ORIGIN_WRITE_BUDGET,
};
pub use self_review::{
    global_self_review_fork, set_global_self_review_fork, RestrictedSelfReviewFork,
    SelfReviewInput, SelfReviewProposalDraft, SelfReviewProposalKind,
    DEFAULT_SELF_REVIEW_TURN_INTERVAL,
};
pub use tool_result_store::{read_tool_result, ToolResultStore};
pub use transcript::Transcript;
pub use usage::UsageAccount;
