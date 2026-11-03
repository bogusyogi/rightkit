//! Typed resolution receipts: `ExecutionRequirement`, `MechanismDecision`,
//! typed outcomes, avoidance/escalation receipts, fingerprints.
//!
//! Canon (PL-010): "`ExecutionRequirement`/`MechanismDecision` receipts with
//! fingerprints … typed outcomes (`Completed`/`Insufficient`/`Ambiguous`/
//! `Unsupported`/`Denied`/`Failed`/`Stale`/`HumanRequired`) where `Denied`
//! never escalates to a model."
//!
//! Structural guarantees in this file:
//! - [`MechanismDecision::assemble`] rejects a decision that pairs
//!   [`MechanismOutcome::Denied`] with an escalation receipt — `Denied` never
//!   escalates to a model, by construction.
//! - [`MechanismDecision::needs_model`] is false for every outcome except
//!   `Insufficient` and an escalated `Failed`; in particular it is always
//!   false for `Denied`.
//! - Every decision carries a deterministic `decision_fingerprint` over its
//!   canonical receipt content, so telemetry and retries can correlate.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::class::MechanismClass;
use super::membrane::MembraneBinding;
use super::requirement::RequestScope;

/// Schema version stamped on every mechanism decision receipt.
pub const MECHANISM_DECISION_SCHEMA_VERSION: u32 = 1;

/// Bounded default for semantic-output verification (characters). A semantic
/// answer larger than this fails deterministic verification; the orchestrator
/// may override per contract via `VerificationContract::with_max_output_chars`.
pub const DEFAULT_MAX_SEMANTIC_OUTPUT_CHARS: usize = 100_000;

/// How a resolution ended. Exactly the canon's typed outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MechanismOutcome {
    /// A deterministic mechanism was bound; model routing avoided.
    Completed,
    /// Deterministic mechanisms cannot satisfy the request; a semantic model
    /// is needed. Model routing (`host_router`) runs next.
    Insufficient,
    /// Deterministic candidates exist but the request does not determine
    /// which one; the resolver refuses to pick silently.
    Ambiguous,
    /// The requirement names a capability/binding the projected registry
    /// cannot support.
    Unsupported,
    /// Policy/authority denied the capability. Terminal — never escalates to
    /// a model.
    Denied,
    /// A bound deterministic mechanism failed verification/execution.
    /// Escalation to a model is allowed exactly once.
    Failed,
    /// The request's observed registry revision no longer matches the live
    /// projection; re-resolution against the current revision is required.
    Stale,
    /// Only a human can satisfy the request (explicit binding or explicit
    /// approval signal). Parked for human input, never model-routed.
    HumanRequired,
}

impl MechanismOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Insufficient => "insufficient",
            Self::Ambiguous => "ambiguous",
            Self::Unsupported => "unsupported",
            Self::Denied => "denied",
            Self::Failed => "failed",
            Self::Stale => "stale",
            Self::HumanRequired => "human_required",
        }
    }
}

/// Which deterministic stage produced the decision. Canon: "deterministic
/// resolution first (explicit binding, exact capability match, structured
/// signals, bounded classifier last)".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionStage {
    /// Upstream denial gate (root or inherited).
    AuthorityGate,
    /// Root-level exact fast path (explicit binding or exact tool name).
    RootExactFastPath,
    /// Validated explicit binding.
    ExplicitBinding,
    /// Exact capability match against projected descriptors.
    ExactCapabilityMatch,
    /// Deterministic structured signals.
    StructuredSignals,
    /// Bounded classifier proposal (may not bind executors).
    BoundedClassifier,
    /// Terminal without binding (stale/unsupported/etc.).
    Terminal,
}

/// The binding a decision produces. `capability` is `None` for class-only
/// bindings (builtin/structured-code-op/human/semantic-model proposals).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ResolvedBinding {
    pub mechanism_class: MechanismClass,
    pub capability: Option<String>,
    pub binding_ref: String,
    /// Capability-registry revision the binding was resolved against.
    pub authority_revision: String,
}

/// Deterministic verification contract attached to a binding (and to the
/// semantic path). Canon: "deterministic verification of semantic output".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct VerificationContract {
    /// JSON Schema the semantic/structured output must satisfy. Only the
    /// bounded `required`-keys subset is checked deterministically (full
    /// schema validation belongs to the JSON-schema owner).
    pub output_schema: Option<Value>,
    /// Membrane `source_ref`s whose coverage the output must acknowledge.
    /// A semantic answer may never replace a bound source.
    pub require_source_coverage: Vec<String>,
    /// Bounded output size (characters).
    pub max_output_chars: usize,
    /// Named deterministic predicates. Unknown names fail closed.
    /// Known: `non_empty`, `json_parses`.
    pub deterministic_predicates: Vec<String>,
}

impl Default for VerificationContract {
    fn default() -> Self {
        Self {
            output_schema: None,
            require_source_coverage: Vec::new(),
            max_output_chars: DEFAULT_MAX_SEMANTIC_OUTPUT_CHARS,
            deterministic_predicates: Vec::new(),
        }
    }
}

impl VerificationContract {
    pub fn with_output_schema(mut self, schema: Value) -> Self {
        self.output_schema = Some(schema);
        self
    }

    pub fn with_source_coverage(mut self, refs: Vec<String>) -> Self {
        self.require_source_coverage = refs;
        self
    }

    pub fn with_max_output_chars(mut self, max: usize) -> Self {
        self.max_output_chars = max;
        self
    }

    pub fn with_predicates(mut self, predicates: Vec<String>) -> Self {
        self.deterministic_predicates = predicates;
        self
    }
}

/// What execution needs, once a mechanism is bound. Carried on the decision so
/// the executor seam receives a receipt, not a guess.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ExecutionRequirement {
    pub binding: ResolvedBinding,
    pub verification: VerificationContract,
    /// Capability names the executor must hold (validated at bind time).
    pub required_capabilities: Vec<String>,
}

/// Telemetry receipt: model routing was avoided by a deterministic mechanism.
/// Canon: "avoidance/escalation telemetry receipts". Serializable for the
/// telemetry seam; emission point is wiring, not resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AvoidanceReceipt {
    pub stage: ResolutionStage,
    pub mechanism_class: MechanismClass,
    pub reason: String,
}

/// The one allowed trigger kinds for post-binding escalation
/// (LLMCompiler fallback shape).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationTrigger {
    VerificationFailed,
    DeterministicBindingFailed,
    ClassifierUnavailable,
}

/// Telemetry receipt: a deterministic path failed and model routing takes
/// over. Bounded to one attempt, mirroring `host_router`'s
/// `bounded_escalation` shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MechanismEscalationReceipt {
    pub attempt: u8,
    pub trigger: EscalationTrigger,
    pub from_class: Option<MechanismClass>,
    /// Always `SemanticModel`: escalation from a mechanism decision can only
    /// mean "hand to model routing".
    pub to_class: MechanismClass,
    /// Request fingerprint of the original resolution (correlation id).
    pub request_fingerprint: String,
}

/// Errors produced while assembling or escalating decisions.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MechanismDecisionError {
    #[error("denied outcome can never carry an escalation receipt")]
    DeniedWithEscalation,
    #[error("decision already used its one bounded escalation")]
    AlreadyEscalated,
    #[error("outcome {0} is not escalatable; only Failed escalates after binding")]
    NotEscalatable(String),
}

/// The full resolution receipt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct MechanismDecision {
    pub schema_version: u32,
    /// Fingerprint of the request this decision resolved.
    pub request_fingerprint: String,
    pub node_id: String,
    pub scope_is_root: bool,
    pub stage: ResolutionStage,
    pub outcome: MechanismOutcome,
    pub reason: String,
    /// Present only when a mechanism was bound.
    pub execution_requirement: Option<ExecutionRequirement>,
    /// Present for `Ambiguous`: the deterministic candidate set.
    pub candidates: Vec<ResolvedBinding>,
    /// Present when model routing was avoided.
    pub avoidance: Option<AvoidanceReceipt>,
    /// Present when a deterministic path failed and the model path takes over.
    pub escalation: Option<MechanismEscalationReceipt>,
    /// Capability-registry revision the decision was resolved against.
    pub resolved_against_revision: String,
    /// Provider-authoritative bindings honored by this decision.
    pub membrane_bindings: Vec<MembraneBinding>,
    /// Deterministic fingerprint over the canonical receipt.
    pub decision_fingerprint: String,
    /// Capability names that caused an `AuthorityGate` `Denied` outcome.
    /// Empty for every other outcome. Lets a per-child mechanism host
    /// inherit exactly what the parent denied (`RequestScope::Child`'s
    /// `inherited_denials`) without parsing `reason` prose — see
    /// [`MechanismDecision::with_denied_capabilities`]. Deliberately excluded
    /// from the fingerprint canonicalization: it is host-wiring metadata, not
    /// part of the decision's resolved identity.
    pub denied_capabilities: Vec<String>,
}

impl Default for MechanismDecision {
    fn default() -> Self {
        Self {
            schema_version: MECHANISM_DECISION_SCHEMA_VERSION,
            request_fingerprint: String::new(),
            node_id: String::new(),
            scope_is_root: true,
            stage: ResolutionStage::Terminal,
            outcome: MechanismOutcome::Insufficient,
            reason: String::new(),
            execution_requirement: None,
            candidates: Vec::new(),
            avoidance: None,
            escalation: None,
            resolved_against_revision: String::new(),
            membrane_bindings: Vec::new(),
            decision_fingerprint: String::new(),
            denied_capabilities: Vec::new(),
        }
    }
}

impl MechanismDecision {
    /// Assemble a receipt and compute its deterministic fingerprint.
    ///
    /// Fails when the canon invariant "Denied never escalates" would be
    /// violated — the invalid state is unrepresentable.
    pub fn assemble(
        request_fingerprint: String,
        node_id: impl Into<String>,
        scope: &RequestScope,
        stage: ResolutionStage,
        outcome: MechanismOutcome,
        reason: impl Into<String>,
        execution_requirement: Option<ExecutionRequirement>,
        candidates: Vec<ResolvedBinding>,
        avoidance: Option<AvoidanceReceipt>,
        escalation: Option<MechanismEscalationReceipt>,
        resolved_against_revision: impl Into<String>,
        membrane_bindings: Vec<MembraneBinding>,
    ) -> Result<Self, MechanismDecisionError> {
        if outcome == MechanismOutcome::Denied && escalation.is_some() {
            return Err(MechanismDecisionError::DeniedWithEscalation);
        }
        let node_id = node_id.into();
        let reason = reason.into();
        let resolved_against_revision = resolved_against_revision.into();
        let decision_fingerprint = {
            let canonical = serde_json::json!({
                "v": MECHANISM_DECISION_SCHEMA_VERSION,
                "request_fingerprint": &request_fingerprint,
                "node_id": &node_id,
                "scope_is_root": matches!(scope, RequestScope::Root),
                "stage": stage,
                "outcome": outcome,
                "reason": &reason,
                "execution_requirement": &execution_requirement,
                "candidates": &candidates,
                "avoidance": &avoidance,
                "escalation": &escalation,
                "resolved_against_revision": &resolved_against_revision,
                "membrane_bindings": &membrane_bindings,
            });
            format!(
                "sha256:{}",
                super::requirement::MechanismRequest::sha256_hex(canonical.to_string().as_bytes())
            )
        };
        Ok(Self {
            schema_version: MECHANISM_DECISION_SCHEMA_VERSION,
            request_fingerprint,
            node_id,
            scope_is_root: matches!(scope, RequestScope::Root),
            stage,
            outcome,
            reason: reason.into(),
            execution_requirement,
            candidates,
            avoidance,
            escalation,
            resolved_against_revision,
            membrane_bindings,
            decision_fingerprint,
            denied_capabilities: Vec::new(),
        })
    }

    /// `true` only when model routing must run next. `Denied` is structurally
    /// excluded: a denial never reaches a model.
    pub fn needs_model(&self) -> bool {
        match self.outcome {
            MechanismOutcome::Insufficient => true,
            MechanismOutcome::Failed => self.escalation.is_some(),
            _ => false,
        }
    }

    /// Records which capability names caused an `AuthorityGate` `Denied`
    /// outcome. Meaningful only when `outcome == Denied`; calling it on any
    /// other decision is harmless (the caller simply gets a list back it
    /// never reads). Kept as a post-`assemble` setter rather than an
    /// `assemble` parameter so every existing call site — most of which never
    /// produce a `Denied` outcome — stays unchanged.
    pub fn with_denied_capabilities(mut self, denied_capabilities: Vec<String>) -> Self {
        self.denied_capabilities = denied_capabilities;
        self
    }

    /// True when this decision terminates resolution without any binding.
    pub fn has_binding(&self) -> bool {
        self.execution_requirement.is_some()
    }

    /// True when the live registry revision has moved past the revision this
    /// decision was resolved against (canon `Stale`).
    pub fn is_stale_against(&self, current_revision: &str) -> bool {
        !current_revision.is_empty() && self.resolved_against_revision != current_revision
    }

    /// Attach the one bounded escalation receipt to a `Failed` decision.
    /// `Denied` can never be escalated; an already-escalated decision is
    /// rejected; anything other than `Failed`/escalated-`Failed` is not
    /// escalatable here (an `Insufficient` decision already routes to the
    /// model without an escalation receipt).
    pub fn with_escalation(
        mut self,
        trigger: EscalationTrigger,
        from_class: Option<MechanismClass>,
    ) -> Result<Self, MechanismDecisionError> {
        if self.outcome == MechanismOutcome::Denied {
            return Err(MechanismDecisionError::DeniedWithEscalation);
        }
        if self.escalation.is_some() {
            return Err(MechanismDecisionError::AlreadyEscalated);
        }
        if self.outcome != MechanismOutcome::Failed {
            return Err(MechanismDecisionError::NotEscalatable(
                self.outcome.as_str().to_string(),
            ));
        }
        self.escalation = Some(MechanismEscalationReceipt {
            attempt: 1,
            trigger,
            from_class,
            to_class: MechanismClass::SemanticModel,
            request_fingerprint: self.request_fingerprint.clone(),
        });
        self.decision_fingerprint = Self::recompute_fingerprint(&self);
        Ok(self)
    }

    fn recompute_fingerprint(decision: &MechanismDecision) -> String {
        let canonical = serde_json::json!({
            "v": MECHANISM_DECISION_SCHEMA_VERSION,
            "request_fingerprint": &decision.request_fingerprint,
            "node_id": &decision.node_id,
            "scope_is_root": decision.scope_is_root,
            "stage": decision.stage,
            "outcome": decision.outcome,
            "reason": &decision.reason,
            "execution_requirement": &decision.execution_requirement,
            "candidates": &decision.candidates,
            "avoidance": &decision.avoidance,
            "escalation": &decision.escalation,
            "resolved_against_revision": &decision.resolved_against_revision,
            "membrane_bindings": &decision.membrane_bindings,
        });
        format!(
            "sha256:{}",
            super::requirement::MechanismRequest::sha256_hex(canonical.to_string().as_bytes())
        )
    }
}
