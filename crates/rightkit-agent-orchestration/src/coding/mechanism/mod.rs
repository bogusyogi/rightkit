//! Mechanism-first execution resolution (canon atom **PL-010**, `ORIGINAL`).
//!
//! A typed [`MechanismResolver`] sits between topology routing
//! ([`crate::coding::dispatch::DispatchRouter`]: Direct vs Conduct) and model routing
//! ([`rightkit_llm::agent_router`]: which model), answering "does this need a model at
//! all?". Deterministic mechanisms bind first (explicit binding, exact
//! capability match over descriptors projected from the existing tool
//! registry — no second registry — structured signals); a bounded classifier
//! runs last and may only propose within the closed [`MechanismClass`] set.
//!
//! Typed outcomes ([`MechanismOutcome`]): `Completed`/`Insufficient`/
//! `Ambiguous`/`Unsupported`/`Denied`/`Failed`/`Stale`/`HumanRequired`, where
//! `Denied` never escalates to a model (structurally enforced in
//! [`MechanismDecision::assemble`]).
//!
//! Research lineage (no code donor; canon: "No compatible donor;
//! research-informed (PAL/LLM+P/MRKL/LLMCompiler/LATM)"): see
//! `class` for the per-class attribution.
//!
//! This module is additive: it reads nothing from and writes nothing to
//! `dispatch.rs` or `host_router`; the daemon composes them at the
//! seams documented in the lane wiring note
//! (`docs/canon/daemon-master-canon-atoms.md`).

mod capability;
mod class;
mod classifier;
mod decision;
mod membrane;
mod requirement;
mod resolver;

pub use capability::{
    AuthorityNeed, CapabilityDescriptor, CapabilitySource, StaticCapabilitySource,
};
pub use class::MechanismClass;
pub use classifier::{
    ClassifiedMechanism, ClassifierError, HeuristicMechanismClassifier, MechanismClassifier,
};
pub use decision::{
    AvoidanceReceipt, EscalationTrigger, ExecutionRequirement, MechanismDecision,
    MechanismDecisionError, MechanismEscalationReceipt, MechanismOutcome, ResolutionStage,
    ResolvedBinding, VerificationContract, DEFAULT_MAX_SEMANTIC_OUTPUT_CHARS,
    MECHANISM_DECISION_SCHEMA_VERSION,
};
pub use membrane::{MembraneBinding, MembraneBindingError, MembraneSurface};
pub use requirement::{
    CapabilityRequirement, ExplicitBinding, MechanismRequest, RequestScope, StructuredIntent,
    StructuredSignals,
};
pub use resolver::{MechanismResolver, SemanticOutput, SemanticVerification};

#[cfg(test)]
#[path = "mechanism_tests.rs"]
mod mechanism_tests;
