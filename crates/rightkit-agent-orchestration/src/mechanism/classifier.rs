//! The bounded classifier — the LAST resolution stage.
//!
//! Canon (PL-010): "deterministic resolution first (explicit binding, exact
//! capability match, structured signals, bounded classifier last)."
//!
//! "Bounded" here is a structural property, not a hope:
//! - The proposal label set is the closed [`MechanismClass`] enum — a
//!   classifier cannot invent a mechanism kind.
//! - A proposal is never a binding: the classifier names a class and a
//!   confidence, and the resolver still requires an executor (projected
//!   descriptor) before anything executes.
//! - A classifier may never propose [`MechanismClass::Human`] into existence
//!   (human escalation needs an explicit signal/binding) and may never lift a
//!   denial.
//!
//! `HeuristicMechanismClassifier` is the deterministic in-crate default so
//! the stage has a working shape before an LLM-backed classifier is wired at
//! the daemon seam (the classifier trait is async for exactly that future).

use async_trait::async_trait;

use super::class::MechanismClass;
use super::requirement::{MechanismRequest, StructuredIntent};

/// One bounded classifier proposal.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassifiedMechanism {
    pub class: MechanismClass,
    /// Confidence in `[0.0, 1.0]`; clamped by the resolver.
    pub confidence: f32,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClassifierError {
    #[error("classifier unavailable: {0}")]
    Unavailable(String),
}

/// The bounded proposal stage. Async so a model-backed classifier can plug in
/// at the daemon seam without changing the resolver.
#[async_trait]
pub trait MechanismClassifier: Send + Sync {
    async fn classify(
        &self,
        request: &MechanismRequest,
    ) -> Result<ClassifiedMechanism, ClassifierError>;
}

/// Deterministic in-crate default: pure signal mapping, no inference, no
/// state. Mirrors the fallback philosophy of `dispatch.rs` — a deterministic
/// opinion with explicit confidence — but at mechanism level and strictly
/// bounded to the closed class set.
#[derive(Debug, Clone, Copy, Default)]
pub struct HeuristicMechanismClassifier;

#[async_trait]
impl MechanismClassifier for HeuristicMechanismClassifier {
    async fn classify(
        &self,
        request: &MechanismRequest,
    ) -> Result<ClassifiedMechanism, ClassifierError> {
        let signals = &request.signals;
        let (class, confidence, reason) = match signals.intent {
            StructuredIntent::Compute | StructuredIntent::Transform => (
                MechanismClass::StructuredCodeOp,
                0.6,
                "Compute/transform intent biases toward deterministic program execution.",
            ),
            StructuredIntent::Retrieve => (
                MechanismClass::SourceResolver,
                0.6,
                "Retrieve intent biases toward authoritative source resolution.",
            ),
            StructuredIntent::Mutate | StructuredIntent::Communicate => (
                MechanismClass::Process,
                0.5,
                "Mutating/communicating intent biases toward a bound process executor.",
            ),
            StructuredIntent::Observe => (
                MechanismClass::Builtin,
                0.7,
                "Observe intent biases toward a builtin capability.",
            ),
            StructuredIntent::Decide | StructuredIntent::Unknown => (
                MechanismClass::SemanticModel,
                0.5,
                "Decision/unknown intent cannot be satisfied deterministically; a semantic model is needed.",
            ),
        };
        Ok(ClassifiedMechanism {
            class,
            confidence,
            reason: reason.to_string(),
        })
    }
}
