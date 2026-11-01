//! Semantic/applicability sealing (canon §7.3).
//!
//! Immutable semantic payload — every field that changes meaning or
//! applicability — is hashed into a `seal_digest`. Mutable state (lifecycle,
//! verification counters, observations) lives in a separate envelope and may
//! change only through receipted transition events. Any post-emission mutation
//! of a sealed field changes the digest and is rejected.

use serde::{Deserialize, Serialize};

use crate::authority::{AuthorityEffect, PrecedenceTier};
use crate::canonical::{canonical_object, sha256_canonical};
use crate::record::{InfluenceClass, RecordClass};
use crate::scope::ScopeDimensions;

pub const SEAL_CONTRACT_VERSION: &str = "adapt.semantic-seal.v1";
pub const ADMISSION_POLICY_VERSION: &str = "adapt.admission.v1";
pub const REDACTION_CONTRACT_VERSION: &str = "membrane.redaction.v1";
pub const PROVENANCE_CONTRACT_VERSION: &str = "adapt.provenance.v2";

/// The immutable semantic payload. Hashing this produces the seal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticPayloadV1 {
    /// Contract version of the sealing scheme itself.
    pub seal_contract_version: String,
    /// Semantic record kind (e.g. `preference`, `insight_issue`).
    pub record_kind: String,
    /// Category / detector family.
    pub category: String,
    /// Canonical text/description (normalized for hashing).
    pub canonical_text: String,
    pub scope: String,
    pub scope_dimensions: ScopeDimensions,
    /// Authority class tier carried by the sealed semantics.
    pub authority_tier: PrecedenceTier,
    pub authority_effect: AuthorityEffect,
    pub influence_class: InfluenceClass,
    pub record_class: Option<RecordClass>,
    /// Machine/client/model applicability where it is semantically binding.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_binding: Option<String>,
    /// Sorted digests of the source evidence objects.
    pub source_evidence_digests: Vec<String>,
    /// Binding to the canonical pool/version the semantics were mined under.
    pub canonical_pool_sha256: String,
    pub admission_policy_version: String,
    /// Held-out semantic-validator receipt identity.
    pub validator_receipt_id: String,
    pub validator_receipt_sha256: String,
    pub redaction_contract_version: String,
    pub provenance_contract_version: String,
}

impl SemanticPayloadV1 {
    /// Compute the seal digest over the canonical JSON form.
    pub fn seal_digest(&self) -> String {
        let value = serde_json::to_value(self).expect("payload serializes");
        sha256_canonical(&value)
    }
}

/// A receipted mutable-state change that never touches sealed semantics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvelopeMutation {
    pub kind: EnvelopeMutationKind,
    pub target_id: String,
    pub expected_seal_digest: String,
    pub receipt_sha256: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvelopeMutationKind {
    LifecycleTransition,
    VerificationStamp,
    ObservationAppend,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SealError {
    DigestMismatch { expected: String, found: String },
    MutationTouchesSealedField { field: &'static str },
}

/// Verify a stored payload against its recorded seal digest.
pub fn verify_seal(payload: &SemanticPayloadV1, recorded_digest: &str) -> Result<(), SealError> {
    let found = payload.seal_digest();
    if found != recorded_digest {
        return Err(SealError::DigestMismatch {
            expected: recorded_digest.to_string(),
            found,
        });
    }
    Ok(())
}

/// Validate an envelope mutation: the envelope may only mutate when the
/// caller's view of the sealed payload still matches the recorded seal.
pub fn validate_envelope_mutation(
    payload: &SemanticPayloadV1,
    recorded_digest: &str,
    mutation: &EnvelopeMutation,
) -> Result<(), SealError> {
    verify_seal(payload, recorded_digest)?;
    if mutation.expected_seal_digest != recorded_digest {
        return Err(SealError::DigestMismatch {
            expected: recorded_digest.to_string(),
            found: mutation.expected_seal_digest.clone(),
        });
    }
    if mutation.receipt_sha256.trim().is_empty() {
        return Err(SealError::MutationTouchesSealedField {
            field: "receipt_sha256",
        });
    }
    Ok(())
}

/// Deterministic manifest-level seal for an ordered batch of payloads:
/// binds each payload's digest plus the batch ordering itself.
pub fn batch_seal(payloads: &[&SemanticPayloadV1]) -> String {
    let digests: Vec<serde_json::Value> = payloads
        .iter()
        .map(|p| serde_json::Value::String(p.seal_digest()))
        .collect();
    let value = canonical_object([
        (
            "seal_contract_version",
            serde_json::Value::String(SEAL_CONTRACT_VERSION.into()),
        ),
        ("digests", serde_json::Value::Array(digests)),
    ]);
    sha256_canonical(&value)
}
