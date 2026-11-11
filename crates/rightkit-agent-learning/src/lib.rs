//! rightkit-agent-learning — a coding agent's post-session learning pipeline (CodeRight).
//!
//! Extracts proposal candidates from a finished session, evaluates them, promotes
//! good ones into a target sink, and detects regressions against a baseline.
//!
//! The crate also provides evaluation-gated workflow optimization proposals.

pub mod analyzer;
pub mod deployer;
pub mod error;
pub mod eval_gate;
pub mod evaluation;
pub mod extractor;
pub mod finetuning;
pub mod instincts;
pub mod meta_skill;
pub mod proposal_store;
pub mod proposals;
pub mod regression;
pub mod skill_bundle;
pub mod skill_opt;
pub mod skill_synthesis;
pub mod store;
pub mod types;

pub use analyzer::{AnalyzerSource, DailyAnalyzer, DailyReport, ReviewCandidate};
pub use deployer::{DeploymentSink, ProposalDeployer, RecordingSink};
pub use error::LearningError;
pub use eval_gate::{EvalEvidence, EvalGate, GateOutcome};
pub use evaluation::{EvaluationConfig, EvaluationEngine};
pub use extractor::{PostSessionLearningExtractor, SessionSummary};
pub use finetuning::{ExportPlan, FineTuneTrigger};
pub use instincts::{Instinct, InstinctStore, PreferenceEvalGate};
pub use meta_skill::{initial_skillopt_meta_skill, refine_skillopt_meta_skill, SkillOptMetaSkill};
pub use proposal_store::ProposalStore;
pub use proposals::{
    OptimizationAdmission, PromotionRecord, Proposal as WorkflowProposal, ProposalBuilder,
    ProposalKind as WorkflowProposalKind,
};
pub use regression::{RegressionDetector, RegressionVerdict};
pub use skill_bundle::SkillBundle;
pub use skill_opt::{
    BenchmarkGate, BenchmarkGateKind, RejectedEdit, SkillOptimizationCandidate,
    SkillOptimizationCandidateProposal, SkillOptimizationEpochInput, SkillOptimizationEpochResult,
    SkillOptimizationHistoryEntry, SkillOptimizationProposalInput, SkillOptimizationProposalResult,
    SkillOptimizationRun, SkillOptimizationStatus, SkillOptimizationStore, SkillOptimizationTarget,
};
pub use skill_synthesis::{
    SkillSynthesisLoop, SynthesisConfig, SynthesisCycleResult, SynthesizeFn, VerifyFn,
};
pub use store::LearningProposalStore;
pub use types::{Proposal, ProposalKind, ProposalStatus};
