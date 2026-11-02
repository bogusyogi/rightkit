//! Protocol shape for mechanically fitting a Membrane-authored packet.
//!
//! Push owns reduction mechanics, while Pull/Membrane owns which evidence is
//! admitted. A host may select only among these complete representations; it
//! must not construct a new one by dropping individual items.

use crate::host_observation::{
    ensure_same_estimator_basis, EstimatorBasisV1, HostObservationValidationError,
    ObservationCoverageV1, ObservationUnavailableReasonV1, RemainingContextCeilingV1,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Current [`PacketReductionPlanV1`] schema version.
pub const PACKET_REDUCTION_PLAN_SCHEMA_VERSION: u32 = 1;
pub const PACKET_REDUCTION_SELECTION_RECEIPT_SCHEMA_VERSION: u32 = 1;

/// One internally coherent representation published by Membrane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PacketReductionRepresentationV1 {
    /// Stable representation label such as `full`, `reduced_1`, or `floor`.
    pub id: String,
    /// Token count measured with plan's declared estimator basis.
    pub tokens: u64,
    /// Parent packet/document identity retained by this representation.
    pub parent_ref: String,
    /// Protected task-critical items retained verbatim by this representation.
    pub protected: Vec<String>,
    /// Evidence identities retained by this representation.
    pub evidence_refs: Vec<String>,
    /// Exact resolver paths for recovering source material.
    pub resolver_paths: Vec<String>,
    /// Minimum viable token size for this representation.
    pub minimum_viable_tokens: u64,
    /// Content-free note describing coverage supplied by this representation.
    pub coverage_note: String,
    /// Complete serialized content for this representation. The host emits
    /// this value exactly; it never edits packet membership itself.
    pub content: serde_json::Value,
}

/// A bounded ladder of complete representations for host-side capacity fit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PacketReductionPlanV1 {
    pub schema_version: u32,
    /// Basis used for every representation token count.
    pub estimator_basis: EstimatorBasisV1,
    pub representations: Vec<PacketReductionRepresentationV1>,
    /// The plan-wide protected set. Every representation must contain it.
    pub protected: Vec<String>,
    /// Smallest viable representation size for this plan.
    pub minimum_viable_tokens: u64,
}

/// Content-free receipt for one request-time host-capacity selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PacketReductionSelectionReceiptV1 {
    pub schema_version: u32,
    /// Stable packet identity shared by every representation in the plan.
    pub plan_ref: String,
    /// Host observation identity used for this selection.
    pub ceiling_id: String,
    pub session_id: String,
    pub selected_representation_id: String,
    pub selected_tokens: u64,
    pub remaining_tokens: u64,
    pub estimator_basis: EstimatorBasisV1,
    pub decision: String,
}

impl PacketReductionSelectionReceiptV1 {
    pub const SCHEMA_VERSION: u32 = PACKET_REDUCTION_SELECTION_RECEIPT_SCHEMA_VERSION;
}

/// Publication failures for [`PacketReductionPlanV1`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PacketReductionPlanError {
    #[error("packet reduction plan schema version is unsupported: {0}")]
    SchemaVersion(u32),
    #[error("packet reduction plan has no representations")]
    NoRepresentations,
    #[error("packet reduction plan contains an empty {0}")]
    EmptyField(&'static str),
    #[error("packet reduction plan contains duplicate values in {0}")]
    DuplicateValue(&'static str),
    #[error("packet reduction plan contains duplicate representation id: {0}")]
    DuplicateRepresentation(String),
    #[error("packet reduction plan has inconsistent minimum viable tokens")]
    MinimumViableTokens,
    #[error("packet reduction plan estimator basis is invalid: {0}")]
    InvalidEstimatorBasis(#[source] HostObservationValidationError),
    #[error("representation {representation} is below minimum viable size")]
    RepresentationBelowMinimum { representation: String },
    #[error("representation {representation} omits protected item: {item}")]
    ProtectedItemMissing {
        representation: String,
        item: String,
    },
}

/// Refusal reasons for host-side selection from a published plan.
///
/// Selection accepts only an exact, validated host ceiling.  In particular,
/// partial or unavailable observations never become an invented numeric
/// budget.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PacketReductionSelectionError {
    #[error("packet reduction plan is invalid: {0}")]
    InvalidPlan(#[source] PacketReductionPlanError),
    #[error("remaining context ceiling is invalid: {0}")]
    InvalidCeiling(#[source] HostObservationValidationError),
    #[error("packet and ceiling estimator bases differ: {0}")]
    EstimatorBasisMismatch(#[source] HostObservationValidationError),
    #[error("remaining context ceiling is not exact: coverage={coverage:?}, reason={reason:?}")]
    CapacityUnavailable {
        coverage: ObservationCoverageV1,
        reason: ObservationUnavailableReasonV1,
    },
    #[error(
        "no packet reduction representation fits remaining capacity: {remaining_tokens} tokens available; {minimum_viable_tokens} required"
    )]
    NoRepresentationFits {
        remaining_tokens: u64,
        minimum_viable_tokens: u64,
    },
}

/// Descriptive alias for callers that name selection after its plan contract.
pub type PacketReductionPlanSelectionError = PacketReductionSelectionError;

impl PacketReductionPlanV1 {
    pub const SCHEMA_VERSION: u32 = PACKET_REDUCTION_PLAN_SCHEMA_VERSION;

    /// Validate invariants required before publishing a plan to a host.
    pub fn validate(&self) -> Result<(), PacketReductionPlanError> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(PacketReductionPlanError::SchemaVersion(self.schema_version));
        }
        if self.representations.is_empty() {
            return Err(PacketReductionPlanError::NoRepresentations);
        }
        self.estimator_basis
            .validate()
            .map_err(PacketReductionPlanError::InvalidEstimatorBasis)?;
        validate_non_empty(&self.protected, "protected")?;
        ensure_unique(&self.protected, "protected")?;

        let mut ids = HashSet::with_capacity(self.representations.len());
        let mut viable = false;
        for representation in &self.representations {
            if representation.id.is_empty() {
                return Err(PacketReductionPlanError::EmptyField("representation id"));
            }
            if !ids.insert(&representation.id) {
                return Err(PacketReductionPlanError::DuplicateRepresentation(
                    representation.id.clone(),
                ));
            }
            if representation.parent_ref.is_empty() {
                return Err(PacketReductionPlanError::EmptyField("parent_ref"));
            }
            validate_non_empty(&representation.protected, "representation protected")?;
            ensure_unique(&representation.protected, "representation protected")?;
            validate_non_empty(&representation.evidence_refs, "evidence_refs")?;
            ensure_unique(&representation.evidence_refs, "evidence_refs")?;
            validate_non_empty(&representation.resolver_paths, "resolver_paths")?;
            ensure_unique(&representation.resolver_paths, "resolver_paths")?;
            if representation.coverage_note.is_empty() {
                return Err(PacketReductionPlanError::EmptyField("coverage_note"));
            }
            if representation.content.is_null() {
                return Err(PacketReductionPlanError::EmptyField("content"));
            }
            if representation.minimum_viable_tokens != self.minimum_viable_tokens {
                return Err(PacketReductionPlanError::MinimumViableTokens);
            }
            if representation.tokens < self.minimum_viable_tokens {
                return Err(PacketReductionPlanError::RepresentationBelowMinimum {
                    representation: representation.id.clone(),
                });
            }
            viable = true;
            for item in &self.protected {
                if !representation
                    .protected
                    .iter()
                    .any(|candidate| candidate == item)
                {
                    return Err(PacketReductionPlanError::ProtectedItemMissing {
                        representation: representation.id.clone(),
                        item: item.clone(),
                    });
                }
            }
        }
        if !viable {
            return Err(PacketReductionPlanError::MinimumViableTokens);
        }
        Ok(())
    }

    /// Whether plan satisfies publication invariants.
    pub fn is_publishable(&self) -> bool {
        self.validate().is_ok()
    }

    /// Select the largest complete representation that fits host capacity.
    ///
    /// The host must provide its validated H8 observation.  Membrane does not
    /// derive capacity from packet contents, character budgets, or defaults.
    /// Plan validation runs first, so every returned representation retains
    /// plan-wide protected items and complete resolver/evidence lineage.
    pub fn select_for_capacity(
        &self,
        ceiling: &RemainingContextCeilingV1,
    ) -> Result<&PacketReductionRepresentationV1, PacketReductionSelectionError> {
        self.validate()
            .map_err(PacketReductionSelectionError::InvalidPlan)?;
        ceiling
            .validate()
            .map_err(PacketReductionSelectionError::InvalidCeiling)?;
        ensure_same_estimator_basis(&self.estimator_basis, &ceiling.remaining_tokens.basis)
            .map_err(PacketReductionSelectionError::EstimatorBasisMismatch)?;

        let estimate = &ceiling.remaining_tokens.estimate;
        if estimate.coverage != ObservationCoverageV1::Complete {
            let Some(reason) = estimate.unavailable_reason else {
                return Err(PacketReductionSelectionError::InvalidCeiling(
                    HostObservationValidationError::Coverage {
                        field: "remainingTokens.estimate".into(),
                        reason: "non-exact coverage requires a typed reason".into(),
                    },
                ));
            };
            return Err(PacketReductionSelectionError::CapacityUnavailable {
                coverage: estimate.coverage,
                reason,
            });
        }

        let remaining_tokens = estimate.value.ok_or_else(|| {
            PacketReductionSelectionError::InvalidCeiling(
                HostObservationValidationError::UnavailableValue {
                    field: "remainingTokens.estimate".into(),
                },
            )
        })?;

        let mut selected: Option<&PacketReductionRepresentationV1> = None;
        for representation in &self.representations {
            if representation.tokens <= remaining_tokens
                && selected
                    .as_ref()
                    .is_none_or(|current| representation.tokens > current.tokens)
            {
                selected = Some(representation);
            }
        }

        selected.ok_or(PacketReductionSelectionError::NoRepresentationFits {
            remaining_tokens,
            minimum_viable_tokens: self.minimum_viable_tokens,
        })
    }

    /// Compatibility spelling for callers that use the host-capacity term.
    pub fn select_for_host_capacity(
        &self,
        ceiling: &RemainingContextCeilingV1,
    ) -> Result<&PacketReductionRepresentationV1, PacketReductionSelectionError> {
        self.select_for_capacity(ceiling)
    }
}

fn validate_non_empty(
    values: &[String],
    field: &'static str,
) -> Result<(), PacketReductionPlanError> {
    if values.iter().any(String::is_empty) {
        Err(PacketReductionPlanError::EmptyField(field))
    } else {
        Ok(())
    }
}

fn ensure_unique(values: &[String], field: &'static str) -> Result<(), PacketReductionPlanError> {
    let mut seen = HashSet::with_capacity(values.len());
    if values.iter().all(|value| seen.insert(value)) {
        Ok(())
    } else {
        Err(PacketReductionPlanError::DuplicateValue(field))
    }
}
