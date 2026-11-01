//! Host-owned, mechanical evidence emitted for Membrane H4/H6.
//!
//! This module deliberately contains no Adapt labels.  It records exact host
//! observations, keeps unavailable inputs typed, and keeps token estimates
//! bound to their estimator basis before callers compare or add them.

use serde::{Deserialize, Serialize};
use std::sync::Mutex;

const TOOL_SCHEMA: &str = "coderight.tool-schema-observation.v1";
const PROCEDURAL_ASSET: &str = "coderight.procedural-asset-observation.v1";
const EVALUATION_OUTCOME: &str = "coderight.evaluation-outcome.v1";
pub const PROVIDER_TURN_EVIDENCE_SCHEMA: &str = "coderight.provider-turn-evidence.v1";
const BYTE_ESTIMATOR_ID: &str = "byte_length_div_4_ceiling";
const BYTE_ESTIMATOR_VERSION: &str = "1";
const MAX_ROWS: usize = 4096;
const MAX_EVIDENCE_LABEL_BYTES: usize = 512;
const MAX_LOADED_CONTEXT_IDENTITIES: usize = 256;

/// Why a value could not be observed.  A missing observation is never encoded
/// as numeric zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    InputNotProvided,
    NotInstrumented,
    PendingResult,
    Unsupported,
    IncompatibleBasis,
    Overflow,
    SubmissionRejected,
    SubmissionUnknown,
    SubmissionUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationCoverage {
    Complete,
    Partial,
    Unavailable,
}

/// A count that is either observed or explicitly unavailable.  Field names
/// match Membrane's `CountObservationV1`; reason/source preserve P3/P4 gaps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CountObservationV1 {
    pub coverage: ObservationCoverage,
    pub count: Option<u64>,
    pub unavailable_reason: Option<UnavailableReason>,
    pub source: Option<String>,
}

impl CountObservationV1 {
    pub fn observed(value: u64) -> Self {
        Self {
            coverage: ObservationCoverage::Complete,
            count: Some(value),
            unavailable_reason: None,
            source: Some("host_observation".to_string()),
        }
    }

    pub fn unavailable(reason: UnavailableReason) -> Self {
        Self {
            coverage: ObservationCoverage::Unavailable,
            count: None,
            unavailable_reason: Some(reason),
            source: Some("host_observation".to_string()),
        }
    }

    pub fn is_observed(&self) -> bool {
        self.coverage == ObservationCoverage::Complete
            && self.count.is_some()
            && self.unavailable_reason.is_none()
    }
}

impl Default for CountObservationV1 {
    fn default() -> Self {
        Self::unavailable(UnavailableReason::NotInstrumented)
    }
}

/// Token estimate with its estimator/provider basis attached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TokenEstimateV1 {
    pub tokens: Option<u64>,
    pub estimator_id: String,
    pub estimator_version: String,
    pub model_family: Option<String>,
    pub provider_id: Option<String>,
    pub class: TokenEstimateClass,
    pub unavailable_reason: Option<UnavailableReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenEstimateClass {
    ProviderReported,
    Estimated,
}

impl TokenEstimateV1 {
    /// Conservative host estimate from exact byte length.  This is not a
    /// billing number and is never presented as provider usage.
    pub fn estimated_from_bytes(bytes: usize) -> Self {
        Self {
            tokens: Some(bytes.div_ceil(4) as u64),
            estimator_id: BYTE_ESTIMATOR_ID.to_string(),
            estimator_version: BYTE_ESTIMATOR_VERSION.to_string(),
            model_family: None,
            provider_id: None,
            class: TokenEstimateClass::Estimated,
            unavailable_reason: None,
        }
    }

    pub fn provider_reported(
        tokens: u64,
        provider_id: impl Into<String>,
        model_family: Option<String>,
    ) -> Self {
        Self {
            tokens: Some(tokens),
            estimator_id: "provider_reported".to_string(),
            estimator_version: "1".to_string(),
            model_family,
            provider_id: Some(provider_id.into()),
            class: TokenEstimateClass::ProviderReported,
            unavailable_reason: None,
        }
    }

    pub fn unavailable(reason: UnavailableReason) -> Self {
        Self {
            tokens: None,
            estimator_id: String::new(),
            estimator_version: String::new(),
            model_family: None,
            provider_id: None,
            class: TokenEstimateClass::Estimated,
            unavailable_reason: Some(reason),
        }
    }

    /// Two estimates can be added or compared only when their basis is the
    /// same.  Provider-reported values require the same provider; estimated
    /// values require the same estimator/version/model family basis.
    pub fn compatible_with(&self, other: &Self) -> bool {
        if self.tokens.is_none() || other.tokens.is_none() {
            return false;
        }
        match (self.class, other.class) {
            (TokenEstimateClass::Estimated, TokenEstimateClass::Estimated) => {
                self.estimator_id == other.estimator_id
                    && self.estimator_version == other.estimator_version
                    && self.model_family == other.model_family
            }
            (TokenEstimateClass::ProviderReported, TokenEstimateClass::ProviderReported) => {
                self.provider_id.is_some() && self.provider_id == other.provider_id
            }
            _ => false,
        }
    }

    pub fn checked_add(&self, other: &Self) -> Result<Self, UnavailableReason> {
        if !self.compatible_with(other) {
            return Err(UnavailableReason::IncompatibleBasis);
        }
        let tokens = self
            .tokens
            .expect("compatible estimate has tokens")
            .checked_add(other.tokens.expect("compatible estimate has tokens"))
            .ok_or(UnavailableReason::Overflow)?;
        let mut result = self.clone();
        result.tokens = Some(tokens);
        Ok(result)
    }
}

/// Source and evidence linkage for one emitted record.  A host receipt is
/// optional where its production seam does not provide one yet; the gap is
/// explicit instead of being replaced with an invented identifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceV1 {
    pub source: String,
    pub evidence_ids: Vec<String>,
    pub receipt_id: Option<String>,
    pub receipt_unavailable_reason: Option<UnavailableReason>,
}

impl ProvenanceV1 {
    pub fn from_evidence(source: impl Into<String>, evidence_ids: Vec<String>) -> Self {
        Self {
            source: source.into(),
            evidence_ids,
            receipt_id: None,
            receipt_unavailable_reason: Some(UnavailableReason::InputNotProvided),
        }
    }
}

/// Admission error for host evidence. Evidence IDs and receipts are optional
/// in current production seams, but a provenance record must identify its
/// source. This is deliberately a small gate: it does not upgrade missing
/// inputs into evidence and it does not assign a synthetic receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceGateError {
    EmptySource,
}

pub fn validate_provenance(provenance: &ProvenanceV1) -> Result<(), EvidenceGateError> {
    if provenance.source.trim().is_empty() {
        return Err(EvidenceGateError::EmptySource);
    }
    Ok(())
}

/// Outcome observed at one physical provider submission boundary. `Unavailable`
/// means no submission result was observed; it is not an alias for success.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSubmissionStateV1 {
    Submitted,
    Rejected,
    SubmissionUnknown,
    Unavailable,
}

/// One typed H9/H10 value. Non-complete coverage deliberately carries no
/// substitute value, preventing rejection or ambiguous transport failure from
/// becoming retained context evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderEvidenceValueV1<T> {
    pub coverage: ObservationCoverage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<UnavailableReason>,
}

impl<T> ProviderEvidenceValueV1<T> {
    pub fn complete(value: T) -> Self {
        Self {
            coverage: ObservationCoverage::Complete,
            value: Some(value),
            unavailable_reason: None,
        }
    }

    pub fn unavailable(reason: UnavailableReason) -> Self {
        Self {
            coverage: ObservationCoverage::Unavailable,
            value: None,
            unavailable_reason: Some(reason),
        }
    }

    fn validate(&self) -> Result<(), ProviderTurnEvidenceError> {
        match self.coverage {
            ObservationCoverage::Complete
                if self.value.is_some() && self.unavailable_reason.is_none() =>
            {
                Ok(())
            }
            ObservationCoverage::Partial | ObservationCoverage::Unavailable
                if self.value.is_none() && self.unavailable_reason.is_some() =>
            {
                Ok(())
            }
            ObservationCoverage::Complete => Err(ProviderTurnEvidenceError::InvalidValueShape),
            ObservationCoverage::Partial | ObservationCoverage::Unavailable => {
                Err(ProviderTurnEvidenceError::InvalidValueShape)
            }
        }
    }
}

/// Provider-neutral, content-free identity of one measured physical attempt.
/// Host provider adapters map their own prepared-request/submission types into
/// this primitive contract without telemetry depending on provider crates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderTurnEvidenceV1 {
    pub schema_version: String,
    pub session_id: String,
    pub task_id: String,
    pub request_id: String,
    pub generation: u64,
    pub authorization_id: String,
    pub prepared_digest: String,
    pub prepared_content_type: String,
    pub prepared_route: String,
    pub prepared_model: String,
    pub serializer_generation: u64,
    pub tool_catalog_generation: ProviderEvidenceValueV1<u64>,
    pub modality: String,
    pub measured_tokens: u64,
    pub physical_attempt: u32,
    pub submission_state: ProviderSubmissionStateV1,
    pub loaded_context_identities: ProviderEvidenceValueV1<Vec<String>>,
    pub retained_representation_digest: ProviderEvidenceValueV1<String>,
    pub provenance: ProvenanceV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderTurnEvidenceError {
    InvalidSchema,
    MissingIdentity,
    LabelTooLong,
    TooManyLoadedContextIdentities,
    TooManyEvidenceIds,
    InvalidPhysicalAttempt,
    InvalidValueShape,
    CompleteRequiresSubmitted,
    SubmissionReasonMismatch,
    EmptyCompleteDigest,
    InvalidProvenance,
}

impl ProviderTurnEvidenceV1 {
    /// Enforce H9/H10 truthfulness before a row reaches telemetry. A submitted
    /// attempt may still have typed unavailable retention; every other state
    /// must have typed non-complete H9/H10 values.
    pub fn validate(&self) -> Result<(), ProviderTurnEvidenceError> {
        if self.schema_version != PROVIDER_TURN_EVIDENCE_SCHEMA {
            return Err(ProviderTurnEvidenceError::InvalidSchema);
        }
        let labels = [
            self.session_id.as_str(),
            self.task_id.as_str(),
            self.request_id.as_str(),
            self.authorization_id.as_str(),
            self.prepared_digest.as_str(),
            self.prepared_content_type.as_str(),
            self.prepared_route.as_str(),
            self.prepared_model.as_str(),
            self.modality.as_str(),
        ];
        if labels.iter().any(|value| value.trim().is_empty()) {
            return Err(ProviderTurnEvidenceError::MissingIdentity);
        }
        if labels
            .iter()
            .any(|value| value.len() > MAX_EVIDENCE_LABEL_BYTES)
        {
            return Err(ProviderTurnEvidenceError::LabelTooLong);
        }
        if self.physical_attempt == 0 {
            return Err(ProviderTurnEvidenceError::InvalidPhysicalAttempt);
        }
        self.loaded_context_identities.validate()?;
        self.retained_representation_digest.validate()?;
        self.tool_catalog_generation.validate()?;
        if let Some(identities) = &self.loaded_context_identities.value {
            if identities.len() > MAX_LOADED_CONTEXT_IDENTITIES {
                return Err(ProviderTurnEvidenceError::TooManyLoadedContextIdentities);
            }
            if identities.iter().any(|identity| {
                identity.trim().is_empty() || identity.len() > MAX_EVIDENCE_LABEL_BYTES
            }) {
                return Err(ProviderTurnEvidenceError::MissingIdentity);
            }
        }
        if self
            .retained_representation_digest
            .value
            .as_ref()
            .is_some_and(|digest| digest.trim().is_empty())
        {
            return Err(ProviderTurnEvidenceError::EmptyCompleteDigest);
        }
        if self.submission_state != ProviderSubmissionStateV1::Submitted
            && (self.loaded_context_identities.coverage == ObservationCoverage::Complete
                || self.retained_representation_digest.coverage == ObservationCoverage::Complete)
        {
            return Err(ProviderTurnEvidenceError::CompleteRequiresSubmitted);
        }
        let expected_unavailable_reason = match self.submission_state {
            ProviderSubmissionStateV1::Submitted => None,
            ProviderSubmissionStateV1::Rejected => Some(UnavailableReason::SubmissionRejected),
            ProviderSubmissionStateV1::SubmissionUnknown => {
                Some(UnavailableReason::SubmissionUnknown)
            }
            ProviderSubmissionStateV1::Unavailable => {
                Some(UnavailableReason::SubmissionUnavailable)
            }
        };
        if expected_unavailable_reason.is_some()
            && (self.loaded_context_identities.unavailable_reason != expected_unavailable_reason
                || self.retained_representation_digest.unavailable_reason
                    != expected_unavailable_reason)
        {
            return Err(ProviderTurnEvidenceError::SubmissionReasonMismatch);
        }
        if self.provenance.source.len() > MAX_EVIDENCE_LABEL_BYTES
            || self.provenance.evidence_ids.len() > MAX_LOADED_CONTEXT_IDENTITIES
            || self.provenance.evidence_ids.iter().any(|identity| {
                identity.trim().is_empty() || identity.len() > MAX_EVIDENCE_LABEL_BYTES
            })
            || self.provenance.receipt_id.as_ref().is_some_and(|receipt| {
                receipt.trim().is_empty() || receipt.len() > MAX_EVIDENCE_LABEL_BYTES
            })
        {
            return Err(
                if self.provenance.evidence_ids.len() > MAX_LOADED_CONTEXT_IDENTITIES {
                    ProviderTurnEvidenceError::TooManyEvidenceIds
                } else {
                    ProviderTurnEvidenceError::LabelTooLong
                },
            );
        }
        validate_provenance(&self.provenance)
            .map_err(|_| ProviderTurnEvidenceError::InvalidProvenance)
    }
}

/// Bounded ring snapshot for resource/API exposure. `dropped_rows` counts
/// oldest-first eviction; no caller receives mutable access to process state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderTurnEvidenceRingV1 {
    pub capacity: usize,
    pub dropped_rows: u64,
    pub rows: Vec<ProviderTurnEvidenceV1>,
}

/// C-2 per-tool schema and execution observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSchemaObservationV1 {
    pub schema_version: String,
    pub tool_id: String,
    pub server_id: Option<String>,
    pub server_id_unavailable_reason: Option<UnavailableReason>,
    pub tool_name: String,
    pub schema_digest: String,
    pub schema_bytes: u64,
    pub schema_token_estimate: TokenEstimateV1,
    pub always_on: bool,
    pub visible_turn_ids: Vec<String>,
    pub visible_turn_ids_unavailable_reason: Option<UnavailableReason>,
    pub invocation_count: CountObservationV1,
    pub success_count: CountObservationV1,
    pub failure_count: CountObservationV1,
    pub result_bytes_total: CountObservationV1,
    pub result_token_estimate_total: TokenEstimateV1,
    pub exact_overlap_refs: Vec<String>,
    pub exact_overlap_refs_unavailable_reason: Option<UnavailableReason>,
    pub provenance: ProvenanceV1,
}

impl ToolSchemaObservationV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn from_schema_bytes(
        tool_id: impl Into<String>,
        tool_name: impl Into<String>,
        schema: &[u8],
        always_on: bool,
        visible_turn_ids: Vec<String>,
        invocation_count: CountObservationV1,
        success_count: CountObservationV1,
        failure_count: CountObservationV1,
        result_bytes_total: CountObservationV1,
        exact_overlap_refs: Vec<String>,
        provenance: ProvenanceV1,
    ) -> Self {
        let result_token_estimate_total = result_bytes_total
            .count
            .and_then(|bytes| usize::try_from(bytes).ok())
            .map(TokenEstimateV1::estimated_from_bytes)
            .unwrap_or_else(|| {
                TokenEstimateV1::unavailable(
                    result_bytes_total
                        .unavailable_reason
                        .unwrap_or(UnavailableReason::NotInstrumented),
                )
            });
        let exact_overlap_refs_unavailable_reason = exact_overlap_refs
            .is_empty()
            .then_some(UnavailableReason::NotInstrumented);
        Self {
            schema_version: TOOL_SCHEMA.to_string(),
            tool_id: tool_id.into(),
            server_id: None,
            server_id_unavailable_reason: Some(UnavailableReason::InputNotProvided),
            tool_name: tool_name.into(),
            schema_digest: sha256_hex(schema),
            schema_bytes: schema.len() as u64,
            schema_token_estimate: TokenEstimateV1::estimated_from_bytes(schema.len()),
            always_on,
            visible_turn_ids_unavailable_reason: visible_turn_ids
                .is_empty()
                .then_some(UnavailableReason::InputNotProvided),
            visible_turn_ids,
            invocation_count,
            success_count,
            failure_count,
            result_bytes_total,
            result_token_estimate_total,
            exact_overlap_refs,
            exact_overlap_refs_unavailable_reason,
            provenance,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProceduralAssetKind {
    Skill,
    InstructionFile,
    PromptFragment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetProvenance {
    Authored,
    AdaptProposed,
    Vendor,
}

/// C-2 per-asset observation.  Counts not supplied by current host seams are
/// represented as typed gaps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProceduralAssetObservationV1 {
    pub schema_version: String,
    pub asset_id: String,
    pub asset_kind: ProceduralAssetKind,
    pub digest: String,
    pub bytes: u64,
    pub token_estimate: TokenEstimateV1,
    pub always_on: bool,
    pub visible_turn_ids: Vec<String>,
    pub visible_turn_ids_unavailable_reason: Option<UnavailableReason>,
    pub exposed_count: CountObservationV1,
    pub selected_count: CountObservationV1,
    pub applied_count: CountObservationV1,
    pub corrections_after_use: CountObservationV1,
    pub provenance: Option<AssetProvenance>,
    pub provenance_unavailable_reason: Option<UnavailableReason>,
    pub provenance_receipt: ProvenanceV1,
}

impl ProceduralAssetObservationV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn from_bytes(
        asset_id: impl Into<String>,
        asset_kind: ProceduralAssetKind,
        bytes: &[u8],
        always_on: bool,
        visible_turn_ids: Vec<String>,
        exposed_count: CountObservationV1,
        selected_count: CountObservationV1,
        applied_count: CountObservationV1,
        corrections_after_use: CountObservationV1,
        provenance: Option<AssetProvenance>,
        provenance_receipt: ProvenanceV1,
    ) -> Self {
        let provenance_unavailable_reason = provenance
            .is_none()
            .then_some(UnavailableReason::InputNotProvided);
        Self {
            schema_version: PROCEDURAL_ASSET.to_string(),
            asset_id: asset_id.into(),
            asset_kind,
            digest: sha256_hex(bytes),
            bytes: bytes.len() as u64,
            token_estimate: TokenEstimateV1::estimated_from_bytes(bytes.len()),
            always_on,
            visible_turn_ids_unavailable_reason: visible_turn_ids
                .is_empty()
                .then_some(UnavailableReason::InputNotProvided),
            visible_turn_ids,
            exposed_count,
            selected_count,
            applied_count,
            corrections_after_use,
            provenance,
            provenance_unavailable_reason,
            provenance_receipt,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationVerdict {
    Pass,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnavailableFieldV1 {
    pub field: String,
    pub reason: UnavailableReason,
    pub source: String,
}

impl UnavailableFieldV1 {
    pub fn new(
        field: impl Into<String>,
        reason: UnavailableReason,
        source: impl Into<String>,
    ) -> Self {
        Self {
            field: field.into(),
            reason,
            source: source.into(),
        }
    }
}

/// H6 outcome emitted by an evaluator/execution harness.  Optional identity
/// fields remain optional because current bakeoff inputs do not carry all
/// host/session/provider identifiers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluationOutcomeV1 {
    pub schema_version: String,
    pub outcome_id: String,
    pub evaluator_id: Option<String>,
    pub evaluator_version: Option<String>,
    pub dataset_id: Option<String>,
    pub dataset_digest: Option<String>,
    pub case_id: Option<String>,
    pub experiment_id: Option<String>,
    pub trace_id: Option<String>,
    pub session_id: Option<String>,
    pub task_id: Option<String>,
    pub model: Option<String>,
    pub client: Option<String>,
    pub route_policy: Option<String>,
    pub score_type: String,
    pub score: Option<f64>,
    pub value: Option<f64>,
    pub verdict: EvaluationVerdict,
    pub expected: Option<String>,
    pub reference: Option<String>,
    pub baseline_ref: Option<String>,
    pub observed_cost_usd: f64,
    pub latency_ms: u64,
    pub tool_call_count: u64,
    pub evidence_source: String,
    pub execution_receipt: ProvenanceV1,
    pub unavailable_fields: Vec<UnavailableFieldV1>,
    pub timestamp_unix_ms: u64,
}

impl EvaluationOutcomeV1 {
    /// Attach immutable bakeoff identity once public execution evidence exists.
    /// This binds H6 rows without admitting private corpus material.
    pub fn bind_comparison_identity(
        &mut self,
        experiment_id: String,
        baseline_ref: Option<String>,
        receipt_id: String,
        comparison_receipt_id: String,
    ) {
        self.experiment_id = Some(experiment_id);
        if baseline_ref.is_some() {
            self.baseline_ref = baseline_ref;
        }
        self.execution_receipt.receipt_id = Some(receipt_id);
        self.execution_receipt.receipt_unavailable_reason = None;
        if !self
            .execution_receipt
            .evidence_ids
            .iter()
            .any(|evidence_id| evidence_id == &comparison_receipt_id)
        {
            self.execution_receipt
                .evidence_ids
                .push(comparison_receipt_id);
        }
        self.unavailable_fields.retain(|field| {
            field.field != "experiment_id"
                && (field.field != "baseline_ref" || self.baseline_ref.is_none())
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub fn verified_completion(
        outcome_id: impl Into<String>,
        evaluator_id: Option<String>,
        evaluator_version: Option<String>,
        case_id: String,
        task_id: String,
        passed: bool,
        observed_cost_usd: f64,
        latency_ms: u64,
        tool_call_count: u64,
        evidence_source: impl Into<String>,
        execution_receipt: ProvenanceV1,
        timestamp_unix_ms: u64,
    ) -> Self {
        let score = if passed { 1.0 } else { 0.0 };
        let mut unavailable_fields = Vec::new();
        if evaluator_id.is_none() {
            unavailable_fields.push(UnavailableFieldV1::new(
                "evaluator_id",
                UnavailableReason::InputNotProvided,
                "executed_bakeoff_inputs",
            ));
        }
        if evaluator_version.is_none() {
            unavailable_fields.push(UnavailableFieldV1::new(
                "evaluator_version",
                UnavailableReason::InputNotProvided,
                "executed_bakeoff_inputs",
            ));
        }
        for field in [
            "dataset_id",
            "dataset_digest",
            "experiment_id",
            "trace_id",
            "session_id",
            "model",
            "client",
            "route_policy",
            "expected",
            "reference",
            "baseline_ref",
        ] {
            unavailable_fields.push(UnavailableFieldV1::new(
                field,
                UnavailableReason::InputNotProvided,
                "executed_bakeoff_inputs",
            ));
        }
        Self {
            schema_version: EVALUATION_OUTCOME.to_string(),
            outcome_id: outcome_id.into(),
            evaluator_id,
            evaluator_version,
            dataset_id: None,
            dataset_digest: None,
            case_id: Some(case_id),
            experiment_id: None,
            trace_id: None,
            session_id: None,
            task_id: Some(task_id),
            model: None,
            client: None,
            route_policy: None,
            score_type: "verified_completion".to_string(),
            score: Some(score),
            value: Some(score),
            verdict: if passed {
                EvaluationVerdict::Pass
            } else {
                EvaluationVerdict::Fail
            },
            expected: None,
            reference: None,
            baseline_ref: None,
            observed_cost_usd,
            latency_ms,
            tool_call_count,
            evidence_source: evidence_source.into(),
            execution_receipt,
            unavailable_fields,
            timestamp_unix_ms,
        }
    }
}

#[derive(Debug, Default)]
struct EvidenceSink {
    tool_schema: Vec<ToolSchemaObservationV1>,
    procedural_assets: Vec<ProceduralAssetObservationV1>,
    evaluation_outcomes: Vec<EvaluationOutcomeV1>,
    provider_turn_evidence: Vec<ProviderTurnEvidenceV1>,
    provider_turn_evidence_dropped_rows: u64,
}

static SINK: Mutex<Option<EvidenceSink>> = Mutex::new(None);

fn with_sink<R>(f: impl FnOnce(&mut EvidenceSink) -> R) -> R {
    let mut guard = SINK.lock().expect("evidence telemetry sink lock poisoned");
    f(guard.get_or_insert_with(EvidenceSink::default))
}

fn push_bounded<T>(rows: &mut Vec<T>, row: T) {
    if rows.len() >= MAX_ROWS {
        rows.remove(0);
    }
    rows.push(row);
}

pub fn record_tool_schema_observation(event: ToolSchemaObservationV1) {
    if let Err(error) = validate_provenance(&event.provenance) {
        tracing::warn!(?error, tool_id = %event.tool_id, "rejected tool evidence without provenance");
        return;
    }
    tracing::info!(
        operation = "membrane_h4",
        schema_version = %event.schema_version,
        tool_id = %event.tool_id,
        schema_bytes = event.schema_bytes,
        "tool schema observation"
    );
    with_sink(|sink| push_bounded(&mut sink.tool_schema, event));
}

pub fn record_procedural_asset_observation(event: ProceduralAssetObservationV1) {
    if let Err(error) = validate_provenance(&event.provenance_receipt) {
        tracing::warn!(?error, asset_id = %event.asset_id, "rejected asset evidence without provenance");
        return;
    }
    tracing::info!(
        operation = "membrane_h4",
        schema_version = %event.schema_version,
        asset_id = %event.asset_id,
        asset_bytes = event.bytes,
        "procedural asset observation"
    );
    with_sink(|sink| push_bounded(&mut sink.procedural_assets, event));
}

pub fn record_evaluation_outcome(event: EvaluationOutcomeV1) {
    if let Err(error) = validate_provenance(&event.execution_receipt) {
        tracing::warn!(?error, outcome_id = %event.outcome_id, "rejected evaluation evidence without provenance");
        return;
    }
    tracing::info!(
        operation = "membrane_h6",
        schema_version = %event.schema_version,
        outcome_id = %event.outcome_id,
        verdict = ?event.verdict,
        "evaluation outcome"
    );
    with_sink(|sink| push_bounded(&mut sink.evaluation_outcomes, event));
}

/// Admit one H9/H10 physical-attempt row. Invalid rows are returned to caller
/// and never retained, so negative controls can prove false completion cannot
/// leak into resource metrics.
pub fn record_provider_turn_evidence(
    event: ProviderTurnEvidenceV1,
) -> Result<(), ProviderTurnEvidenceError> {
    event.validate()?;
    tracing::info!(
        operation = "membrane_h9_h10",
        schema_version = %event.schema_version,
        request_id = %event.request_id,
        generation = event.generation,
        physical_attempt = event.physical_attempt,
        submission_state = ?event.submission_state,
        measured_tokens = event.measured_tokens,
        "provider turn evidence"
    );
    with_sink(|sink| {
        if sink.provider_turn_evidence.len() >= MAX_ROWS {
            sink.provider_turn_evidence.remove(0);
            sink.provider_turn_evidence_dropped_rows =
                sink.provider_turn_evidence_dropped_rows.saturating_add(1);
        }
        sink.provider_turn_evidence.push(event);
    });
    Ok(())
}

pub fn tool_schema_observations() -> Vec<ToolSchemaObservationV1> {
    with_sink(|sink| sink.tool_schema.clone())
}

pub fn procedural_asset_observations() -> Vec<ProceduralAssetObservationV1> {
    with_sink(|sink| sink.procedural_assets.clone())
}

pub fn evaluation_outcomes() -> Vec<EvaluationOutcomeV1> {
    with_sink(|sink| sink.evaluation_outcomes.clone())
}

pub fn provider_turn_evidence_ring() -> ProviderTurnEvidenceRingV1 {
    with_sink(|sink| ProviderTurnEvidenceRingV1 {
        capacity: MAX_ROWS,
        dropped_rows: sink.provider_turn_evidence_dropped_rows,
        rows: sink.provider_turn_evidence.clone(),
    })
}

pub fn reset_evidence_telemetry() {
    with_sink(|sink| {
        sink.tool_schema.clear();
        sink.procedural_assets.clear();
        sink.evaluation_outcomes.clear();
        sink.provider_turn_evidence.clear();
        sink.provider_turn_evidence_dropped_rows = 0;
    });
    super::context_ceiling::reset_remaining_context_telemetry();
}

fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut digest = String::with_capacity(64);
    for byte in rightkit_crypto::sha256(bytes) {
        let _ = write!(digest, "{byte:02x}");
    }
    digest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider_turn(state: ProviderSubmissionStateV1) -> ProviderTurnEvidenceV1 {
        let observed = state == ProviderSubmissionStateV1::Submitted;
        let unavailable_reason = match state {
            ProviderSubmissionStateV1::Submitted => UnavailableReason::PendingResult,
            ProviderSubmissionStateV1::Rejected => UnavailableReason::SubmissionRejected,
            ProviderSubmissionStateV1::SubmissionUnknown => UnavailableReason::SubmissionUnknown,
            ProviderSubmissionStateV1::Unavailable => UnavailableReason::SubmissionUnavailable,
        };
        ProviderTurnEvidenceV1 {
            schema_version: PROVIDER_TURN_EVIDENCE_SCHEMA.to_string(),
            session_id: "session-1".to_string(),
            task_id: "task-1".to_string(),
            request_id: "request-1".to_string(),
            generation: 7,
            authorization_id: "authorization-1".to_string(),
            prepared_digest: "sha256:prepared".to_string(),
            prepared_content_type: "application/json".to_string(),
            prepared_route: "route-1".to_string(),
            prepared_model: "model-1".to_string(),
            serializer_generation: 3,
            tool_catalog_generation: ProviderEvidenceValueV1::complete(4),
            modality: "text_only".to_string(),
            measured_tokens: 123,
            physical_attempt: 1,
            submission_state: state,
            loaded_context_identities: if observed {
                ProviderEvidenceValueV1::complete(vec!["context-1".to_string()])
            } else {
                ProviderEvidenceValueV1::unavailable(unavailable_reason)
            },
            retained_representation_digest: if observed {
                ProviderEvidenceValueV1::complete("sha256:retained".to_string())
            } else {
                ProviderEvidenceValueV1::unavailable(unavailable_reason)
            },
            provenance: ProvenanceV1::from_evidence(
                "provider_submission_boundary",
                vec!["authorization-1".to_string()],
            ),
        }
    }

    #[test]
    fn estimates_require_matching_basis_before_addition() {
        let left = TokenEstimateV1::estimated_from_bytes(5);
        let right = TokenEstimateV1::estimated_from_bytes(4);
        assert!(left.compatible_with(&right));
        assert_eq!(left.checked_add(&right).unwrap().tokens, Some(3));

        let provider = TokenEstimateV1::provider_reported(2, "provider-a", None);
        assert!(!left.compatible_with(&provider));
        assert_eq!(
            left.checked_add(&provider),
            Err(UnavailableReason::IncompatibleBasis)
        );
    }

    #[test]
    fn unavailable_counts_do_not_serialize_as_zero() {
        let count = CountObservationV1::unavailable(UnavailableReason::NotInstrumented);
        let json = serde_json::to_string(&count).unwrap();
        assert!(json.contains("null"));
        assert!(!json.contains(":0"));
    }

    #[test]
    fn schema_and_asset_digests_are_exact_sha256() {
        let schema = ToolSchemaObservationV1::from_schema_bytes(
            "tool-a",
            "tool-a",
            b"{}",
            true,
            vec!["turn-1".to_string()],
            CountObservationV1::observed(0),
            CountObservationV1::observed(0),
            CountObservationV1::observed(0),
            CountObservationV1::observed(0),
            Vec::new(),
            ProvenanceV1::from_evidence("provider_request", vec!["turn-1".to_string()]),
        );
        assert_eq!(
            schema.schema_digest,
            "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        );
        let asset = ProceduralAssetObservationV1::from_bytes(
            "skill-a",
            ProceduralAssetKind::Skill,
            b"abc",
            false,
            Vec::new(),
            CountObservationV1::unavailable(UnavailableReason::NotInstrumented),
            CountObservationV1::observed(1),
            CountObservationV1::observed(1),
            CountObservationV1::unavailable(UnavailableReason::NotInstrumented),
            None,
            ProvenanceV1::from_evidence("skill_tool", Vec::new()),
        );
        assert_eq!(asset.bytes, 3);
        assert_eq!(asset.digest.len(), 64);
    }

    #[test]
    fn evaluation_outcome_keeps_missing_h6_identity_typed() {
        let outcome = EvaluationOutcomeV1::verified_completion(
            "outcome-1",
            Some("verifier-type".to_string()),
            None,
            "case-1".to_string(),
            "task-1".to_string(),
            true,
            0.25,
            12,
            2,
            "live_execution",
            ProvenanceV1::from_evidence("executed_bakeoff", vec!["task-1".to_string()]),
            42,
        );
        assert_eq!(outcome.score, Some(1.0));
        assert!(outcome.unavailable_fields.iter().any(|field| {
            field.field == "dataset_id" && field.reason == UnavailableReason::InputNotProvided
        }));
    }

    #[test]
    fn provenance_gate_requires_a_real_source_but_keeps_optional_inputs_typed() {
        let missing = ProvenanceV1::from_evidence("   ", Vec::new());
        assert_eq!(
            validate_provenance(&missing),
            Err(EvidenceGateError::EmptySource)
        );
        let available = ProvenanceV1::from_evidence("live_execution", Vec::new());
        assert_eq!(validate_provenance(&available), Ok(()));
        assert!(available.receipt_id.is_none());
        assert_eq!(
            available.receipt_unavailable_reason,
            Some(UnavailableReason::InputNotProvided)
        );
    }

    #[test]
    fn submitted_attempt_admits_complete_h9_h10_without_request_content() {
        let row = provider_turn(ProviderSubmissionStateV1::Submitted);
        assert_eq!(row.validate(), Ok(()));
        let json = serde_json::to_string(&row).unwrap();
        assert!(json.contains("authorization-1"));
        assert!(!json.contains("request body sentinel"));
    }

    /// Negative control: transport rejection can never retain complete loaded
    /// identities or a retained representation digest.
    #[test]
    fn rejected_attempt_with_complete_h9_h10_is_rejected_by_sink() {
        let _guard = crate::agent_telemetry::TELEMETRY_TEST_LOCK.lock().unwrap();
        reset_evidence_telemetry();
        let row = provider_turn(ProviderSubmissionStateV1::Rejected);
        let mut false_complete = row;
        false_complete.loaded_context_identities =
            ProviderEvidenceValueV1::complete(vec!["context-1".to_string()]);
        assert_eq!(
            record_provider_turn_evidence(false_complete),
            Err(ProviderTurnEvidenceError::CompleteRequiresSubmitted)
        );
        assert!(provider_turn_evidence_ring().rows.is_empty());
    }

    #[test]
    fn unknown_and_unavailable_attempts_remain_typed_non_complete() {
        for state in [
            ProviderSubmissionStateV1::SubmissionUnknown,
            ProviderSubmissionStateV1::Unavailable,
        ] {
            let row = provider_turn(state);
            assert_eq!(row.validate(), Ok(()));
            assert_ne!(
                row.loaded_context_identities.coverage,
                ObservationCoverage::Complete
            );
            assert_ne!(
                row.retained_representation_digest.coverage,
                ObservationCoverage::Complete
            );
        }
    }

    #[test]
    fn provider_turn_ring_evicts_oldest_and_counts_drops() {
        let _guard = crate::agent_telemetry::TELEMETRY_TEST_LOCK.lock().unwrap();
        reset_evidence_telemetry();
        for generation in 0..=MAX_ROWS {
            let mut row = provider_turn(ProviderSubmissionStateV1::Submitted);
            row.generation = generation as u64;
            record_provider_turn_evidence(row).unwrap();
        }
        let ring = provider_turn_evidence_ring();
        assert_eq!(ring.rows.len(), MAX_ROWS);
        assert_eq!(ring.dropped_rows, 1);
        assert_eq!(ring.rows.first().unwrap().generation, 1);
        reset_evidence_telemetry();
    }
}
