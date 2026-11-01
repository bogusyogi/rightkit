#![allow(
    dead_code,
    unused_variables,
    unused_imports,
    unused_mut,
    clippy::too_many_arguments,
    clippy::useless_conversion,
    clippy::derivable_impls,
    clippy::doc_lazy_continuation,
    clippy::needless_range_loop
)]

//! `host_orchestration` — multi-agent coordination for the the host app engine.
//!
//! This crate provides the building blocks for orchestrating multiple agents:
//! an [`AgentRegistry`] of available agents, an [`AgentRouter`] that matches tasks
//! to agents, a [`Conductor`] that runs worker tasks (sequentially or in parallel),
//! a [`JurySystem`] that aggregates validation verdicts, an
//! [`OrchestrationGovernance`] layer that allows/denies actions, a
//! [`VerificationParser`], and a [`RunWatchdog`] that detects stalled runs.

pub mod artifact_rollback;
pub mod builtins;
pub mod catalog;
pub mod checkpoint;
pub mod compare;
pub mod completion;
pub mod conductor;
pub mod conductor_cache_key;
pub mod diff_walkthrough;
pub mod dispatch;
pub mod error;
pub mod escalation;
pub mod eval_hack;
pub mod evolution;
pub mod executed_bakeoff;
pub mod failure;
pub mod goal_loop;
pub mod governance;
pub mod handoff_brief;
pub mod info_flow;
pub mod jury;
pub mod mechanism;
pub mod orchestrator;
pub mod personas;
pub mod predicate;
pub mod promotion;
pub mod prompt_version;
pub mod proposal_activation;
pub mod recovery;
pub mod registry;
pub mod repo_map;
pub mod review_panel;
pub mod role_pack;
pub mod role_pack_mcp;
pub mod rollout;
pub mod roster_proof;
pub mod router;
pub mod router_proof;
pub mod run_governance;
pub mod session_recap;
pub mod skill_agent;
pub mod small_model_utility;
pub mod spawn;
pub mod subagent_routing;
#[cfg(feature = "coding-host")]
pub mod supervision;
pub mod task_list_projection;
pub mod taste;
pub mod taste_engine;
pub mod teams;
pub mod terminal_bench;
pub mod trajectory_detector;
pub mod trigger;
pub mod unbacked_work_gate;
pub mod verification;
pub mod watchdog;

pub use artifact_rollback::{ArtifactRollback, ArtifactVersion};
pub use checkpoint::{
    resume_graph, run_graph_resumable, run_graph_resumable_with_conductor_and_handoff_archive,
    GraphCheckpointStore, ResumeGraphRequest,
};
pub use compare::{BlindComparison, BlindEntry, Candidate, ComparisonOutcome};
pub use completion::{arbitrate, Arbitration, CompletionConfig, CompletionInput, CompletionState};
pub use conductor::{
    Conductor, ConductorPlan, EchoWorker, FailWorker, Worker, WorkerOutcome, WorkerResult,
    WorkerTask,
};
pub use conductor_cache_key::{
    conductor_day_from_iso, conductor_stable_cache_key, CONDUCTOR_CACHE_KEY_PREFIX,
};
pub use dispatch::{
    ClassifierComplexity, ClassifierOutcome, DispatchClassifier, DispatchDecision, DispatchRouter,
    ExecutionStrategy, JudgeSource,
};
pub use error::OrchestrationError;
pub use eval_hack::{detect as detect_eval_hacks, EvalHackFinding};
pub use evolution::{
    bucket_from_estimate, run_record_from_result, DurationAwareRouter, EvolutionHarness,
    HarnessTracer,
};
pub use executed_bakeoff::{
    evaluate_bakeoff, run_executed_bakeoff, to_membrane_comparison, ArtifactDigestVerifier,
    ArtifactRequirement, BakeoffError, BakeoffExecutor, BakeoffGateConfig, BakeoffGateDecision,
    BakeoffReport, ContextClass, EvaluationTask, EvidencePolicy, EvidenceSource, ExecutionArtifact,
    ExecutionObservability, ExecutionRecord, ExecutionRequest, ExecutionTopology,
    ExecutorDescriptor, MembraneAdapterError, MembraneCandidateCase, MembraneCandidateComparisonV1,
    MembraneCaseBinding, MembraneComparisonBindings, MembraneComparisonLimits,
    MembraneComparisonUsage, RecordedExecution, RecordedFixtureExecutor, ReviewFinding,
    StableTaskId, TaskIdentity, ToolCallTrace, VerificationContract, VerificationVerdict, Verifier,
};
pub use failure::{FailureDimension, FailureKind};
pub use governance::rules::{BudgetRule, ConcurrencyRule, GovernanceRule, TrustRule};
pub use governance::{GateDecision, GovernanceContext, GovernanceVerdict, OrchestrationGovernance};
pub use handoff_brief::{
    estimate_baseline_tokens, ContextItem, HandoffArchiveConfig, HandoffBrief, HandoffBriefBuilder,
    SkeletonizeConfig,
};
pub use info_flow::{E2RTreeSearch, InfoFlowRouter, RoutingMode, SkillBankRouter};
pub use jury::{JurorVerdict, JuryDecision, JurySystem};
pub use orchestrator::{
    recovering_worker, run_graph, run_graph_with_conductor,
    run_graph_with_conductor_and_handoff_archive, run_graph_with_recovery, RunOutcome,
};
pub use predicate::{PredicateContext, PredicateEvaluator};
pub use promotion::{decide_promotion, BenchmarkReport, PromotionConfig, PromotionDecision};
pub use prompt_version::{PromptRegistry, PromptVersion, WorkflowVariant, WorkflowVariantRegistry};
pub use proposal_activation::{ActivatableProposal, ProposalActivator, ProposalStatus};
pub use recovery::{RecoveringWorker, RecoveryNote, StepLocator};
pub use registry::{AgentDescriptor, AgentRegistry};
pub use repo_map::{
    RepositoryMap, RepositoryMapEntry, RepositoryMapSource, DEFAULT_REPOSITORY_MAP_TOKENS,
};
pub use review_panel::{
    council_for_category, execute_panel, jury_for_category, PanelAuthority, PanelKind, PanelMember,
    PanelMemberResult, PanelRunResult, ReviewPanel,
};
pub use rollout::{Rollout, RolloutConfig, RolloutEvent, RolloutStage};
pub use roster_proof::{
    block_on_sync, roster_corpus, rule_path_latency_p95_ms, score_roster_router,
    total_cost_if_routed_to, AlwaysCapableRosterBaseline, CheapestCapableBaseline,
    DispatchRouterRosterAdapter, ModelChoice, RosterLabeledTask, RosterProofReport,
    RosterRouterAdapter, RosterRouterUnderTest, RosterTaskScore, RubricTag,
};
pub use router::AgentRouter;
pub use router_proof::{
    held_out_corpus, score_router, AlwaysCapableRouter, AlwaysCheapRouter, DispatchRouterAdapter,
    InvertedRouter, LabeledTask, ModelTier, ProofReport, RouterUnderTest, TaskScore,
};
pub use spawn::{spawn_with_capabilities, SpawnPlan, SpawnRequest, SpawnTranscriptArtifact};
pub use subagent_routing::{
    derive_context_request, resolve_invocation, CapabilityGrant, ContextRequest, PrefixChangeEvent,
    PrefixChangeReason, PrefixChangeSink, Profile, ProfileCeilingViolation,
    RecordingPrefixChangeSink, ResolvedInvocation, SchedulerPolicy, SessionTurnModel, TaskSpec,
};
pub use taste::{
    compute_taste_score, mop_rate, reliability_decay_curve, reliability_decay_slope,
    variance_amplification_factor, DurationBucket, RunRecord, TasteScore,
};
pub use taste_engine::{propose, ImprovementKind, ImprovementProposal, TasteEngineConfig};
pub use terminal_bench::{TbenchCatalog, TbenchCategory, TbenchDifficulty, TbenchTask};
pub use trajectory_detector::SequenceAnomalyDetector;
pub use verification::{VerificationIssue, VerificationParser, VerificationResult};
pub use watchdog::RunWatchdog;
