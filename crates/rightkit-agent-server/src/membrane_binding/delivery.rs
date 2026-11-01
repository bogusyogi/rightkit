//! Host-to-Membrane context delivery contracts.
//!
//! These are wire shapes only.  Selection of representations and all packet
//! planning semantics remain in Membrane.

use super::unavailable::UnavailableStateV1;
use rightkit_logs::agent_telemetry::{
    ContextObservationCoverage as ObservationCoverage, RemainingContextCeilingV1,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const CURRENT_REQUEST_H8_SCHEMA_VERSION: u32 = 1;
pub const PACKET_REDUCTION_PLAN_SCHEMA_VERSION: u32 = 1;
const AUTHENTICATION_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum H8ObservationPhaseV1 {
    /// Capacity observed before current request dispatch.
    CurrentRequest,
    /// Provider usage observed after a response; usable only by a later
    /// request and never as current-request capacity.
    NextRequestCandidate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeliveryIdentityV1 {
    pub task_id: String,
    pub session_id: String,
    pub request_id: String,
    pub generation: String,
    pub observed_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
}

impl DeliveryIdentityV1 {
    fn validate(&self) -> Result<(), DeliveryValidationError> {
        for (field, value) in [
            ("task_id", &self.task_id),
            ("session_id", &self.session_id),
            ("request_id", &self.request_id),
            ("generation", &self.generation),
        ] {
            if value.trim().is_empty() {
                return Err(DeliveryValidationError::EmptyField(field));
            }
        }
        if self.expires_at_unix_ms <= self.observed_at_unix_ms {
            return Err(DeliveryValidationError::InvalidFreshness);
        }
        Ok(())
    }
}

/// H8 host observation captured from RightKit's provider seam.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CurrentRequestH8ObservationV1 {
    pub schema_version: u32,
    pub phase: H8ObservationPhaseV1,
    pub identity: DeliveryIdentityV1,
    /// Existing telemetry record is retained verbatim, including typed
    /// unavailable capacity and its host provenance receipt.
    pub observation: RemainingContextCeilingV1,
}

impl CurrentRequestH8ObservationV1 {
    pub const SCHEMA_VERSION: u32 = CURRENT_REQUEST_H8_SCHEMA_VERSION;

    pub fn validate(&self) -> Result<(), DeliveryValidationError> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(DeliveryValidationError::SchemaVersion(self.schema_version));
        }
        if self.phase != H8ObservationPhaseV1::CurrentRequest {
            return Err(DeliveryValidationError::PostResponseObservation);
        }
        self.identity.validate()?;
        if self.observation.session_id != self.identity.session_id
            || self.observation.requested_at_unix_ms != self.identity.observed_at_unix_ms
        {
            return Err(DeliveryValidationError::IdentityMismatch);
        }
        if self
            .observation
            .task_id
            .value
            .as_deref()
            .is_some_and(|task_id| task_id != self.identity.task_id)
        {
            return Err(DeliveryValidationError::IdentityMismatch);
        }
        let estimate = &self.observation.remaining_tokens.estimate;
        match estimate.coverage {
            ObservationCoverage::Complete
                if estimate.value.is_some() && estimate.unavailable_reason.is_none() => {}
            ObservationCoverage::Unavailable
                if estimate.value.is_none() && estimate.unavailable_reason.is_some() => {}
            _ => return Err(DeliveryValidationError::InvalidObservationCoverage),
        }
        Ok(())
    }

    /// Constructor for pre-dispatch H8.  Post-response telemetry must be
    /// explicitly labelled `NextRequestCandidate` by its producer.
    pub fn current_request(
        identity: DeliveryIdentityV1,
        observation: RemainingContextCeilingV1,
    ) -> Result<Self, DeliveryValidationError> {
        let value = Self {
            schema_version: Self::SCHEMA_VERSION,
            phase: H8ObservationPhaseV1::CurrentRequest,
            identity,
            observation,
        };
        value.validate()?;
        Ok(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EstimatorBasisV1 {
    pub id: String,
    pub version: String,
}

impl EstimatorBasisV1 {
    fn validate(&self) -> Result<(), DeliveryValidationError> {
        if self.id.trim().is_empty() || self.version.trim().is_empty() {
            return Err(DeliveryValidationError::EmptyField("estimator_basis"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PacketReductionRepresentationV1 {
    pub id: String,
    pub tokens: u64,
    pub parent_ref: String,
    pub protected: Vec<String>,
    pub evidence_refs: Vec<String>,
    pub resolver_paths: Vec<String>,
    pub minimum_viable_tokens: u64,
    pub coverage_note: String,
    /// Complete serialized representation emitted by Membrane; transported
    /// verbatim, never edited by the host.
    pub content: serde_json::Value,
}

/// Membrane-authored complete representation ladder. RightKit transports this
/// shape verbatim, preserving Membrane's selected identity, lineage, order,
/// scope, and scores in adjacent admission receipts; it never computes
/// ranking or chooses evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PacketReductionPlanV1 {
    pub schema_version: u32,
    pub estimator_basis: EstimatorBasisV1,
    pub representations: Vec<PacketReductionRepresentationV1>,
    pub protected: Vec<String>,
    pub minimum_viable_tokens: u64,
}

impl PacketReductionPlanV1 {
    pub const SCHEMA_VERSION: u32 = PACKET_REDUCTION_PLAN_SCHEMA_VERSION;

    pub fn validate(&self) -> Result<(), DeliveryValidationError> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(DeliveryValidationError::PlanSchemaVersion(
                self.schema_version,
            ));
        }
        self.estimator_basis.validate()?;
        // Canonical contract: representations non-empty; protected set may be
        // empty (ordinary retrieval can carry no protected blocks).
        if self.representations.is_empty() {
            return Err(DeliveryValidationError::EmptyField("plan"));
        }
        ensure_unique(&self.protected, "protected")?;
        let mut ids = HashSet::new();
        for representation in &self.representations {
            for (field, value) in [
                ("representation_id", &representation.id),
                ("parent_ref", &representation.parent_ref),
                ("coverage_note", &representation.coverage_note),
            ] {
                if value.trim().is_empty() {
                    return Err(DeliveryValidationError::EmptyField(field));
                }
            }
            if !ids.insert(&representation.id) {
                return Err(DeliveryValidationError::DuplicateValue("representation_id"));
            }
            if representation.minimum_viable_tokens != self.minimum_viable_tokens
                || representation.tokens < self.minimum_viable_tokens
            {
                return Err(DeliveryValidationError::MinimumViableTokens);
            }
            if representation
                .evidence_refs
                .iter()
                .chain(representation.resolver_paths.iter())
                .chain(representation.protected.iter())
                .any(|item| item.trim().is_empty())
            {
                return Err(DeliveryValidationError::EmptyField(
                    "representation lineage",
                ));
            }
            ensure_unique(&representation.protected, "representation_protected")?;
            ensure_unique(&representation.evidence_refs, "evidence_refs")?;
            ensure_unique(&representation.resolver_paths, "resolver_paths")?;
            for protected in &self.protected {
                if !representation
                    .protected
                    .iter()
                    .any(|item| item == protected)
                {
                    return Err(DeliveryValidationError::ProtectedItemMissing {
                        representation: representation.id.clone(),
                        item: protected.clone(),
                    });
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthenticationSchemeV1 {
    Ed25519Detached,
}

/// Authentication metadata is proof material, never a bearer secret.  The
/// active transport verifies it against its configured trust root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthenticationProofV1 {
    pub schema_version: u32,
    pub scheme: AuthenticationSchemeV1,
    pub issuer: String,
    pub key_id: String,
    pub payload_digest: String,
    pub proof: String,
}

impl AuthenticationProofV1 {
    fn validate(&self, payload: &[u8]) -> Result<(), DeliveryValidationError> {
        if self.schema_version != AUTHENTICATION_SCHEMA_VERSION {
            return Err(DeliveryValidationError::SchemaVersion(self.schema_version));
        }
        if self.issuer.trim().is_empty() || self.key_id.trim().is_empty() || self.proof.is_empty() {
            return Err(DeliveryValidationError::AuthenticationMissing);
        }
        if !valid_digest(&self.payload_digest) {
            return Err(DeliveryValidationError::InvalidDigest);
        }
        if self.payload_digest != digest_bytes(payload) {
            return Err(DeliveryValidationError::AuthenticationPayloadMismatch);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct H8PlanDeliveryPayloadV1 {
    pub identity: DeliveryIdentityV1,
    pub h8: CurrentRequestH8ObservationV1,
    pub reduction_plan: PacketReductionPlanV1,
}

impl H8PlanDeliveryPayloadV1 {
    fn validate(&self) -> Result<(), DeliveryValidationError> {
        self.identity.validate()?;
        self.h8.validate()?;
        self.reduction_plan.validate()?;
        if self.h8.identity != self.identity {
            return Err(DeliveryValidationError::IdentityMismatch);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthenticatedH8PlanDeliveryV1 {
    pub schema_version: u32,
    pub delivery_id: String,
    pub payload: H8PlanDeliveryPayloadV1,
    pub authentication: AuthenticationProofV1,
}

impl AuthenticatedH8PlanDeliveryV1 {
    pub const SCHEMA_VERSION: u32 = 1;

    pub fn validate(&self) -> Result<(), DeliveryValidationError> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(DeliveryValidationError::SchemaVersion(self.schema_version));
        }
        if self.delivery_id.trim().is_empty() {
            return Err(DeliveryValidationError::EmptyField("delivery_id"));
        }
        self.payload.validate()?;
        let bytes = serde_json::to_vec(&self.payload)
            .map_err(|_| DeliveryValidationError::Serialization)?;
        self.authentication.validate(&bytes)
    }

    pub fn payload_digest(payload: &H8PlanDeliveryPayloadV1) -> String {
        serde_json::to_vec(payload)
            .map(|bytes| digest_bytes(&bytes))
            .unwrap_or_else(|_| "sha256:".to_string())
    }
}

/// Delivery may be unavailable without turning absence into an empty plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(clippy::large_enum_variant)]
pub enum H8PlanDeliveryV1 {
    Delivered(AuthenticatedH8PlanDeliveryV1),
    Unavailable(UnavailableStateV1),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeliveryValidationError {
    #[error("unsupported delivery schema version: {0}")]
    SchemaVersion(u32),
    #[error("unsupported packet reduction plan schema version: {0}")]
    PlanSchemaVersion(u32),
    #[error("delivery field is empty: {0}")]
    EmptyField(&'static str),
    #[error("delivery freshness interval is invalid")]
    InvalidFreshness,
    #[error("delivery identity does not match observation")]
    IdentityMismatch,
    #[error("post-response H8 observation cannot serve current request")]
    PostResponseObservation,
    #[error("H8 observation coverage is not typed and internally consistent")]
    InvalidObservationCoverage,
    #[error("delivery contains duplicate values in {0}")]
    DuplicateValue(&'static str),
    #[error("packet reduction plan minimum viable tokens are inconsistent")]
    MinimumViableTokens,
    #[error("representation {representation} omits protected item {item}")]
    ProtectedItemMissing {
        representation: String,
        item: String,
    },
    #[error("authentication proof is missing required material")]
    AuthenticationMissing,
    #[error("authentication payload digest is invalid")]
    InvalidDigest,
    #[error("authentication proof does not bind payload bytes")]
    AuthenticationPayloadMismatch,
    #[error("delivery payload could not be serialized")]
    Serialization,
}

fn ensure_unique(values: &[String], field: &'static str) -> Result<(), DeliveryValidationError> {
    if values.iter().any(|value| value.trim().is_empty()) {
        return Err(DeliveryValidationError::EmptyField(field));
    }
    let mut seen = HashSet::new();
    if values.iter().all(|value| seen.insert(value)) {
        Ok(())
    } else {
        Err(DeliveryValidationError::DuplicateValue(field))
    }
}

fn valid_digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn digest_bytes(bytes: &[u8]) -> String {
    let mut digest = rightkit_fs::digest::Sha256Hasher::new();
    digest.update(bytes);
    format!("sha256:{}", digest.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> DeliveryIdentityV1 {
        DeliveryIdentityV1 {
            task_id: "task-1".into(),
            session_id: "session-1".into(),
            request_id: "request-1".into(),
            generation: "generation-1".into(),
            observed_at_unix_ms: 100,
            expires_at_unix_ms: 200,
        }
    }

    fn plan() -> PacketReductionPlanV1 {
        let representation = |id: &str, tokens| PacketReductionRepresentationV1 {
            id: id.into(),
            tokens,
            parent_ref: "packet://task-1".into(),
            protected: vec!["task".into()],
            evidence_refs: vec!["evidence://1".into()],
            resolver_paths: vec!["resolver://1".into()],
            minimum_viable_tokens: 16,
            coverage_note: "complete protected coverage".into(),
            content: serde_json::json!({"id": id}),
        };
        PacketReductionPlanV1 {
            schema_version: 1,
            estimator_basis: EstimatorBasisV1 {
                id: "provider_reported_context_tokens".into(),
                version: "1".into(),
            },
            representations: vec![representation("full", 64), representation("floor", 16)],
            protected: vec!["task".into()],
            minimum_viable_tokens: 16,
        }
    }

    fn unavailable_h8() -> RemainingContextCeilingV1 {
        serde_json::from_value(serde_json::json!({
            "schemaVersion": 1,
            "ceilingId": "ceiling-1",
            "sessionId": "session-1",
            "taskId": {"coverage":"complete","value":"task-1"},
            "requestedAtUnixMs": 100,
            "remainingTokens": {
                "basis": {"id":"provider_reported_context_tokens","version":"1"},
                "estimate": {"coverage":"unavailable","unavailableReason":"provider_omitted"}
            },
            "provenanceReceipt": {
                "schemaVersion": 1,
                "receiptId": "receipt-1",
                "source": "coderight",
                "observedAtUnixMs": 100,
                "receiptDigest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            }
        }))
        .unwrap()
    }

    #[test]
    fn current_request_h8_rejects_post_response_usage() {
        let observation = CurrentRequestH8ObservationV1 {
            schema_version: 1,
            phase: H8ObservationPhaseV1::NextRequestCandidate,
            identity: identity(),
            observation: unavailable_h8(),
        };
        assert!(matches!(
            observation.validate(),
            Err(DeliveryValidationError::PostResponseObservation)
        ));
    }

    #[test]
    fn plan_requires_protected_items_in_every_representation() {
        let mut invalid = plan();
        invalid.representations[1].protected.clear();
        assert!(matches!(
            invalid.validate(),
            Err(DeliveryValidationError::ProtectedItemMissing { .. })
        ));
    }

    #[test]
    fn authentication_digest_binds_exact_payload() {
        let h8 = CurrentRequestH8ObservationV1::current_request(identity(), unavailable_h8())
            .expect("H8 shape is valid");
        let payload = H8PlanDeliveryPayloadV1 {
            identity: identity(),
            h8,
            reduction_plan: plan(),
        };
        let digest = AuthenticatedH8PlanDeliveryV1::payload_digest(&payload);
        let delivery = AuthenticatedH8PlanDeliveryV1 {
            schema_version: 1,
            delivery_id: "delivery-1".into(),
            payload,
            authentication: AuthenticationProofV1 {
                schema_version: 1,
                scheme: AuthenticationSchemeV1::Ed25519Detached,
                issuer: "membrane-gateway".into(),
                key_id: "session-key-1".into(),
                payload_digest: digest,
                proof: "opaque-transport-proof".into(),
            },
        };
        assert!(delivery.validate().is_ok());
        let mut tampered = delivery;
        tampered.payload.reduction_plan.representations[0].tokens = 65;
        assert!(matches!(
            tampered.validate(),
            Err(DeliveryValidationError::AuthenticationPayloadMismatch)
        ));
    }
}
