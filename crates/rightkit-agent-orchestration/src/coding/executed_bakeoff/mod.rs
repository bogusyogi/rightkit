//! Executed-task R1b bakeoff.
//!
//! Unlike the label-only router proof, this harness executes every candidate
//! and baseline task, then gives the resulting artifacts to a separate
//! verifier. Executors never receive the hidden verification contract and
//! cannot mark their own work complete. Reports deliberately omit prompts,
//! expected artifact digests, and artifact bodies.

mod fixtures;
mod gate;
mod membrane_adapter;
mod runner;
mod types;
mod verifier;

pub use fixtures::{RecordedExecution, RecordedFixtureExecutor};
pub use gate::evaluate_bakeoff;
pub use membrane_adapter::{
    to_membrane_comparison, MembraneAdapterError, MembraneCandidateCase,
    MembraneCandidateComparisonV1, MembraneCaseBinding, MembraneComparisonBindings,
    MembraneComparisonLimits, MembraneComparisonUsage,
};
pub use runner::{run_executed_bakeoff, BakeoffExecutor, Verifier};
pub use types::{
    AggregateMetrics, ArtifactRequirement, BakeoffError, BakeoffGateConfig, BakeoffGateDecision,
    BakeoffReport, ComparisonMetrics, ContextClass, EvaluationTask, EvidencePolicy, EvidenceSource,
    ExecutionArtifact, ExecutionObservability, ExecutionRecord, ExecutionRequest,
    ExecutionTopology, ExecutorDescriptor, LongContextMetrics, ReviewFinding, RunScore,
    StableTaskId, StrategyReport, TaskComparison, TaskIdentity, ToolCallTrace,
    VerificationContract, VerificationVerdict,
};
pub use verifier::ArtifactDigestVerifier;
