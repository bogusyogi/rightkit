//! Host-side H9/H10 observation shapes owned by RightKit.
//!
//! These replace the former `membrane_client` re-exports. They are facts the host observed
//! after serialization; Membrane (Pulse, via its CLI) is never linked to produce them.
//! JSON field names and validation rules match the previously exchanged v1 documents, so
//! persisted rows keep parsing.

use serde::{Deserialize, Serialize};

pub const HOST_OBSERVATION_SCHEMA_VERSION: u32 = 1;

/// A host observation failed validation before it could be recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostObservationValidationError(pub String);

impl std::fmt::Display for HostObservationValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HostObservationValidationError {}

fn invalid(message: impl Into<String>) -> HostObservationValidationError {
    HostObservationValidationError(message.into())
}

fn validate_schema(shape: &str, found: u32) -> Result<(), HostObservationValidationError> {
    if found == HOST_OBSERVATION_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(invalid(format!(
            "{shape} has unsupported schema_version {found}; expected {HOST_OBSERVATION_SCHEMA_VERSION}"
        )))
    }
}

fn require_nonempty(field: &str, value: &str) -> Result<(), HostObservationValidationError> {
    if value.trim().is_empty() {
        Err(invalid(format!("{field} must be non-empty")))
    } else {
        Ok(())
    }
}

fn validate_sha256(field: &str, value: &str) -> Result<(), HostObservationValidationError> {
    let ok = value
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()));
    if ok {
        Ok(())
    } else {
        Err(invalid(format!(
            "invalid {field}: expected sha256:<64 hex>"
        )))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepresentationClassV1 {
    Native,
    RenderedFull,
    RenderedExcerpt,
    ResolverBacked,
    MetadataOnly,
}

/// Opaque handle carried verbatim from a context response; never minted here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepresentationHandleV1 {
    pub schema_version: u32,
    pub handle: String,
    pub class: RepresentationClassV1,
    pub installation_id: String,
    pub context_epoch: u64,
    pub source_ref: String,
    pub source_digest: String,
}

impl RepresentationHandleV1 {
    pub fn validate(&self) -> Result<(), HostObservationValidationError> {
        validate_schema("RepresentationHandleV1", self.schema_version)?;
        require_nonempty("handle", &self.handle)?;
        require_nonempty("installationId", &self.installation_id)?;
        require_nonempty("sourceRef", &self.source_ref)?;
        validate_sha256("sourceDigest", &self.source_digest)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationUnavailableReasonV1 {
    NotInstrumented,
    HubInactive,
    ProviderOmitted,
    HostUnsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationCoverageV1 {
    Complete,
    Partial,
    Unavailable,
}

/// A value with explicit coverage; `Unavailable` never carries a value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObservedFieldV1<T> {
    pub coverage: ObservationCoverageV1,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<ObservationUnavailableReasonV1>,
}

impl<T: Eq> Eq for ObservedFieldV1<T> {}

impl<T> ObservedFieldV1<T> {
    pub fn complete(value: T) -> Self {
        Self {
            coverage: ObservationCoverageV1::Complete,
            value: Some(value),
            unavailable_reason: None,
        }
    }

    pub fn partial(value: Option<T>, reason: ObservationUnavailableReasonV1) -> Self {
        Self {
            coverage: ObservationCoverageV1::Partial,
            value,
            unavailable_reason: Some(reason),
        }
    }

    pub fn unavailable(reason: ObservationUnavailableReasonV1) -> Self {
        Self {
            coverage: ObservationCoverageV1::Unavailable,
            value: None,
            unavailable_reason: Some(reason),
        }
    }

    pub fn validate(&self, field: &str) -> Result<(), HostObservationValidationError> {
        let bad = |reason: &str| Err(invalid(format!("invalid coverage for {field}: {reason}")));
        match self.coverage {
            ObservationCoverageV1::Complete => {
                if self.value.is_none() {
                    return bad("complete coverage requires a value");
                }
                if self.unavailable_reason.is_some() {
                    return bad("complete coverage cannot carry an unavailable reason");
                }
            }
            ObservationCoverageV1::Partial => {
                if self.unavailable_reason.is_none() {
                    return bad("partial coverage requires a typed reason");
                }
            }
            ObservationCoverageV1::Unavailable => {
                if self.value.is_some() {
                    return bad("unavailable coverage cannot carry a value");
                }
                if self.unavailable_reason.is_none() {
                    return bad("unavailable coverage requires a typed reason");
                }
            }
        }
        Ok(())
    }
}

/// Provenance receipt attached to H9/H10 records. Distinct from
/// `crate::observation::HostObservationProvenanceV1` (the three-field row receipt).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostReceiptProvenanceV1 {
    pub schema_version: u32,
    pub receipt_id: String,
    pub source: String,
    pub observed_at_unix_ms: u64,
    pub receipt_digest: String,
}

impl HostReceiptProvenanceV1 {
    pub fn new(
        receipt_id: impl Into<String>,
        source: impl Into<String>,
        observed_at_unix_ms: u64,
        receipt_digest: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: HOST_OBSERVATION_SCHEMA_VERSION,
            receipt_id: receipt_id.into(),
            source: source.into(),
            observed_at_unix_ms,
            receipt_digest: receipt_digest.into(),
        }
    }

    pub fn validate(&self) -> Result<(), HostObservationValidationError> {
        require_nonempty("provenance.receipt_id", &self.receipt_id)?;
        require_nonempty("provenance.source", &self.source)?;
        validate_schema("HostObservationProvenanceV1", self.schema_version)?;
        validate_sha256("provenance.receipt_digest", &self.receipt_digest)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoadedContextIdentityV1 {
    pub identity: String,
    pub source_ref: String,
    pub source_digest: String,
}

impl LoadedContextIdentityV1 {
    pub fn validate(&self, field: &str) -> Result<(), HostObservationValidationError> {
        require_nonempty(&format!("{field}.identity"), &self.identity)?;
        require_nonempty(&format!("{field}.sourceRef"), &self.source_ref)?;
        validate_sha256(&format!("{field}.sourceDigest"), &self.source_digest)
    }
}

/// H9: loaded context identities with a compaction generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoadedContextIdentitiesV1 {
    pub schema_version: u32,
    pub snapshot_id: String,
    pub session_id: String,
    pub compaction_generation: ObservedFieldV1<u64>,
    pub identities: ObservedFieldV1<Vec<LoadedContextIdentityV1>>,
    pub observed_at_unix_ms: u64,
    pub provenance_receipt: HostReceiptProvenanceV1,
}

impl LoadedContextIdentitiesV1 {
    pub fn validate(&self) -> Result<(), HostObservationValidationError> {
        validate_schema("LoadedContextIdentitiesV1", self.schema_version)?;
        require_nonempty("snapshotId", &self.snapshot_id)?;
        require_nonempty("sessionId", &self.session_id)?;
        self.compaction_generation
            .validate("compactionGeneration")?;
        self.identities.validate("identities")?;
        if let Some(identities) = &self.identities.value {
            for (index, identity) in identities.iter().enumerate() {
                identity.validate(&format!("identities[{index}]"))?;
            }
        }
        self.provenance_receipt.validate()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PacketDeliveryAcknowledgementStatusV1 {
    Acknowledged,
    Rejected,
}

/// H10: acknowledgement that host-side packet serialization completed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PacketDeliveryAcknowledgementV1 {
    pub schema_version: u32,
    pub acknowledgement_id: String,
    pub packet_digest: String,
    pub host_serialized_digest: String,
    pub session_id: String,
    pub task_id: ObservedFieldV1<String>,
    pub status: PacketDeliveryAcknowledgementStatusV1,
    pub serialized_bytes: ObservedFieldV1<u64>,
    pub acknowledged_at_unix_ms: u64,
    pub provenance_receipt: HostReceiptProvenanceV1,
}

impl PacketDeliveryAcknowledgementV1 {
    pub fn validate(&self) -> Result<(), HostObservationValidationError> {
        validate_schema("PacketDeliveryAcknowledgementV1", self.schema_version)?;
        require_nonempty("acknowledgementId", &self.acknowledgement_id)?;
        require_nonempty("sessionId", &self.session_id)?;
        validate_sha256("packetDigest", &self.packet_digest)?;
        validate_sha256("hostSerializedDigest", &self.host_serialized_digest)?;
        self.task_id.validate("taskId")?;
        self.serialized_bytes.validate("serializedBytes")?;
        self.provenance_receipt.validate()
    }
}
