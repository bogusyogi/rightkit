#![forbid(unsafe_code)]
#![allow(clippy::result_large_err)] // errors carry structured diagnostic context (field, unit, evidence) by design

/// Lowercase hex SHA-256 of `bytes`.
pub(crate) fn hex_digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

pub mod budget;
#[cfg(feature = "coding")]
pub mod coding;
pub mod delivery;
pub mod engine;
pub mod error;
pub mod escalation;
pub mod grant;
pub mod l3_inventory;
pub mod l4_platform;
pub mod p5_core;
pub mod p7_host;
pub mod plan;
pub mod profile;
pub mod route;
pub mod scheduler;
pub mod task;

pub use budget::{BudgetAccount, BudgetReservation};
pub use engine::{
    adjudicate, Adjudication, CandidateEvidence, DrainReport, EffectPolicy, EngineOutcome,
    Invocation, OrchestrationEngine, RuntimeAdmission, RuntimeState,
};
pub use error::RuntimeError;
pub use escalation::{validate_target, EscalationGrant};
pub use grant::EffectiveGrant;
pub use plan::{compile_plan, FrozenPlan};
pub use profile::AgentProfile;
pub use rightkit_evidence_contracts::{
    AgentDefinition, BudgetCeiling, InvocationGrant as CapabilityGrant, TaskSpec,
};
pub use route::{select_route, RouteCandidate, SelectedRoute};
pub use scheduler::{
    replan_remaining, ExecutionJournal, ExecutorActionEnvelope, PauseDecisionReceipt,
    RunLedgerEntry, Scheduler, SchedulerEvent, SchedulerOutput, SchedulerPolicy, TrajectoryRecord,
};
pub use task::{validate_task, ContextRequest};

// LEG-026 owns validation.rs; this declaration is intentionally reserved.
pub mod p6_inventory;
pub mod validation;
pub mod workplan;
