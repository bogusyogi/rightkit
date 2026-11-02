//! # membrane-protocol
//!
//! The **contract source of truth** for the Membrane protocol's five typed
//! shapes:
//!
//!   | Shape                  | Rust type                | Schema (v1)                        |
//!   |------------------------|--------------------------|------------------------------------|
//!   | ScopeGrant             | [`ScopeGrantV1`]         | `schemas/scope-grant.v1.schema.json` |
//!   | ContextCandidateSet    | [`ContextCandidateSetV1`]| `schemas/context-candidate-set.v1.schema.json` |
//!   | ContextPacket          | [`ContextPacketV1`]      | `schemas/context-packet.v1.schema.json` |
//!   | ContextReceipt         | [`ContextReceiptV1`]     | `schemas/context-receipt.v1.schema.json` |
//!   | KnowledgeEmission      | [`KnowledgeEmissionV1`]  | `schemas/knowledge-emission.v1.schema.json` |
//!
//! The Rust types in [`types`] are authoritative. The JSON Schemas under
//! `schemas/` are derived from them, and golden fixtures under `schemas/operations/`
//! are the shared canonical instances. The crate's tests prove — and the
//! TypeScript binding under `bindings/` independently re-proves — that each
//! golden fixture:
//!
//!   1. validates against its JSON Schema,
//!   2. deserializes + re-serializes to the **identical canonical bytes**, and
//!   3. yields the **same `sha256:` digest** in Rust and in TypeScript.
//!
//! See `docs/reference/protocol/source-of-truth.md` for the full contract story.

pub mod background_review;
pub mod canonical;
pub mod compression;
pub mod diagnostics;
pub mod federation;
pub mod fusion;
pub mod heartbeat;
pub mod hook;
pub mod host_observation;
pub mod installation;
pub mod observable_event;
pub mod operations;
pub mod program_result;
pub mod provider_readiness;
pub mod resident;
pub use program_result::{
    ProgramResultActionV1, ProgramResultDataV1, ProgramResultProtectedSpanV1,
    ProgramResultRequestV1,
};
pub mod push;
/// Pull-owned packet reduction contracts. `push` remains a wire-compatibility
/// module for older clients; new callers use this namespace.
pub mod pull {
    pub use super::push::*;
}
pub mod session_transcript;
pub mod source_resolution;
pub mod types;

pub use background_review::{
    BackgroundReviewActivitySignalV1, BackgroundReviewConfigError, BackgroundReviewConfigV1,
    BackgroundReviewExecutionStatusV1, BackgroundReviewExecutionV1, BackgroundReviewJobKindV1,
    BackgroundReviewJobV1, BackgroundReviewObservationReceiptV1, BackgroundReviewObservationV1,
    BackgroundReviewProposalRefV1, BackgroundReviewReasonV1, BackgroundReviewStatusV1,
    BackgroundReviewValidationError, BACKGROUND_REVIEW_ACTIVITY_SCHEMA_VERSION,
    BACKGROUND_REVIEW_EXECUTION_SCHEMA_VERSION, BACKGROUND_REVIEW_RECEIPT_SCHEMA_VERSION,
    BACKGROUND_REVIEW_SCHEMA_VERSION,
};
pub use canonical::{canonical_json_of, canonicalize, digest_str};
pub use compression::{
    CompressionReceiptV1, DroppedSpanV1, ImmutableSourceError, ImmutableSourceReason,
    NonEmptyString, SpanV1, COMPRESSION_RECEIPT_SCHEMA_VERSION,
};
pub use diagnostics::{
    evaluate_gate, AggregateDeltaV1, AggregateIssueDelta, BlueprintDeltaV1, BlueprintFreshness,
    CapabilityVocabulary, ChangedFileHashV1, ConvergenceClass, CostClass, CoverageLaneV1,
    CoverageObligationV1, DeltaClassification, DiagnosticEvidenceSnapshotV1,
    DiagnosticGateDecisionV1, DiagnosticIssueV1, ExactnessRequirement, GateOutcome,
    GatePolicyProfileV1, LaneState, ObligationState, ObservationV1, RequiredScope, SeverityHint,
    SourceClass, SourceRange, TypedOmission, WorkspaceEpochOrigin, WorkspaceEpochV1,
    DIAGNOSTIC_EVIDENCE_SNAPSHOT_SCHEMA_VERSION, DIAGNOSTIC_GATE_DECISION_SCHEMA_VERSION,
    WORKSPACE_EPOCH_SCHEMA_VERSION,
};
pub use federation::{
    sort_candidates, sort_omissions, sort_provider_ids, sort_provider_outputs, sort_warnings,
    Candidate, DeadlineBudget, FederationCandidate, FederationCandidateV1, FederationDiagnosticsV1,
    FederationError, FederationErrorV1, FederationOmission, FederationOmissionV1,
    FederationProviderStatus, FederationProviderStatusV1, FederationRequest, FederationRequestV1,
    FederationResponse, FederationResponseV1, FederationStatus, FederationValidationError,
    FreshnessSnapshotV1, InsufficientConfidenceLaneSearchV1, InsufficientConfidenceReasonV1,
    InsufficientConfidenceStatusV1, InsufficientConfidenceV1, ProviderContext, ProviderContextV1,
    ProviderDiagnosticsV1, ProviderId, ProviderIdV1, ProviderKind, ProviderKindV1,
    ProviderOmissionV1, ProviderOutput, ProviderOutputV1, ProviderStatusV1, ProviderWarningV1,
    PublicationFenceChangeV1, PublicationFenceStatusV1, PublicationFenceV1, ReasonCode,
    ReasonCodeV1, WarningSeverity, FEDERATION_REQUEST_SCHEMA_VERSION,
    FEDERATION_RESPONSE_SCHEMA_VERSION, INSUFFICIENT_CONFIDENCE_POLICY,
    INSUFFICIENT_CONFIDENCE_SCHEMA_VERSION, PROVIDER_ORDER, PROVIDER_OUTPUT_SCHEMA_VERSION,
    PUBLICATION_FENCE_POLICY, PUBLICATION_FENCE_SCHEMA_VERSION,
};
pub use fusion::{FusionDecisionV1, FusionReceiptV1};
pub use heartbeat::{
    AdapterHeartbeatV1, AdapterStatus, MechanismReceiptV1, ADAPTER_HEARTBEAT_SCHEMA_VERSION,
};
pub use hook::{
    aggregate_hook_status, normalize_hook_payload, project_hook_host_response,
    HookDispatchMetadataV1, HookDispatchResultV1, HookEvent, HookExecutionMode, HookHostDecision,
    HookHostResponseV1, HookInputEnvelopeV1, HookInvocationStatus, HookModuleId,
    HookModuleOutputV1, HookModuleResultV1, HookModuleState, HookNormalizationError,
    HookPermissionDecision, HookSpecificOutputV1, HOOK_MODULE_DEADLINE_MS, HOOK_SCHEMA_VERSION,
};
pub use host_observation::{
    compare_token_estimates, ensure_same_estimator_basis, sum_token_estimates,
    CodeRightEvaluationOutcomeV1, CodeRightExecutionObservationV1, CompletionEmissionStatusV1,
    CompletionEmissionV1, DeliveryAcknowledgementV1, EstimatorBasisV1, EvaluationOutcomeV1,
    ExecutionCostV1, ExecutionObservationKindV1, ExecutionObservationV1, ExecutionUsageV1,
    HostObservationCoverageV1, HostObservationProvenanceV1, HostObservationUnavailableReasonV1,
    HostObservationValidationError, HostProvenanceReceiptV1, LoadedContextIdentitiesV1,
    LoadedContextIdentityV1, ObservationCoverage, ObservationCoverageV1,
    ObservationUnavailableReason, ObservationUnavailableReasonV1, ObservedFieldV1,
    PacketDeliveryAcknowledgementStatusV1, PacketDeliveryAcknowledgementV1,
    PacketDeliveryAcknowledgmentV1, RemainingContextCeilingV1, RepresentationClassV1,
    RepresentationHandleV1, TokenEstimateV1, EVALUATION_OUTCOME_SCHEMA_VERSION,
    EXECUTION_OBSERVATION_SCHEMA_VERSION, HOST_OBSERVATION_PROVENANCE_SCHEMA_VERSION,
    HOST_OBSERVATION_SCHEMA_VERSION, LOADED_CONTEXT_IDENTITIES_SCHEMA_VERSION,
    PACKET_DELIVERY_ACKNOWLEDGEMENT_SCHEMA_VERSION, PACKET_DELIVERY_ACKNOWLEDGMENT_SCHEMA_VERSION,
    REMAINING_CONTEXT_CEILING_SCHEMA_VERSION,
};
pub use installation::{
    canonical_data_root, ApiSchemaV1, ComponentV1, HandshakeError, InstallationManifestV1,
    MANIFEST_SCHEMA_VERSION, PROTOCOL_SCHEMA_VERSION,
};
pub use observable_event::{
    ObservableEventKindV1, ObservableEventV1, OBSERVABLE_EVENT_SCHEMA_VERSION,
};
pub use operations::{
    operations, operations_slice, ErrorResult, OperationEnvelope, OperationIndexEntry,
    OperationParameter, OperationResult, OperationSpec, OperationsIndex, ResultKind, SuccessResult,
    OPERATIONS,
};
pub use provider_readiness::{
    ProviderIdentityV1, ProviderObservationV1, ProviderReadinessStateV1, ProviderReadinessV1,
    ProviderTestQueryV1, PROVIDER_READINESS_SCHEMA_VERSION,
};
pub use push::{
    PacketReductionPlanError, PacketReductionPlanSelectionError, PacketReductionPlanV1,
    PacketReductionRepresentationV1, PacketReductionSelectionError,
    PACKET_REDUCTION_PLAN_SCHEMA_VERSION,
};
pub use resident::{
    ResidentAnswerV1, ResidentErrorV1, ResidentOp, ResidentRequestV1, ResidentServiceFileV1,
    RESIDENT_APP, RESIDENT_ENDPOINT_VERSION, RESIDENT_MAX_FRAME_BYTES, RESIDENT_SERVICE,
};
pub use session_transcript::{
    SessionTranscriptAppendReceiptV1, SessionTranscriptAppendV1, SessionTranscriptConflictV1,
    SessionTranscriptCursorTipV1, SessionTranscriptEventV1, SESSION_TRANSCRIPT_EVENT_TYPE,
    SESSION_TRANSCRIPT_HOST, SESSION_TRANSCRIPT_MAX_EVENTS_PER_APPEND,
    SESSION_TRANSCRIPT_SCHEMA_VERSION,
};
pub use source_resolution::{
    SourceResolutionReceiptV1, SourceResolutionStatusV1, SOURCE_RESOLUTION_RECEIPT_SCHEMA_VERSION,
};
pub use types::*;

use serde::Serialize;

/// The canonical byte contract shared with the TypeScript binding.
///
/// Implemented for every protocol type via its `Serialize` impl. The methods
/// return the canonical JSON (sorted keys, no whitespace) and its `sha256:`
/// digest — the exact bytes/digest the TS side must reproduce.
pub trait CanonicalSerialize: Serialize {
    /// Canonical JSON: object keys sorted, no insignificant whitespace.
    fn canonical_json(&self) -> String {
        canonical_json_of(self)
    }
    /// `sha256:<hex>` of [`Self::canonical_json`].
    fn canonical_digest(&self) -> String {
        crate::canonical::canonical_digest_of(self)
    }
}

// Blanket-impl the canonical contract for every serializable protocol type.
impl<T: Serialize> CanonicalSerialize for T {}

/// One typed contract shape: its name, its canonical JSON Schema, and its
/// golden fixture, all embedded so both the Rust tests and any downstream
/// consumer read exactly the same artifacts the TS binding reads from disk.
pub struct ContractShape {
    /// Stable shape name, e.g. `"ScopeGrantV1"`.
    pub name: &'static str,
    /// Repo-relative path of the JSON Schema (also embedded as `schema`).
    pub schema_path: &'static str,
    /// Repo-relative path of the golden fixture (also embedded as `fixture`).
    pub fixture_path: &'static str,
    /// The canonical JSON Schema text.
    pub schema: &'static str,
    /// The golden fixture JSON text.
    pub fixture: &'static str,
}

// Schemas & fixtures ship in this crate under `assets/`. The source repository
// retains matching canonical files at its root for its non-Rust bindings.
const ROOT: &str = env!("CARGO_MANIFEST_DIR");

/// Every typed contract shape, in canonical order. Used by the round-trip tests
/// and re-exported for downstream gates.
pub const SHAPES: &[ContractShape] = &[
    ContractShape {
        name: "ScopeGrantV1",
        schema_path: "schemas/scope-grant.v1.schema.json",
        fixture_path: "schemas/operations/scope-grant.v1.golden.json",
        schema: include_str!("../assets/schemas/scope-grant.v1.schema.json"),
        fixture: include_str!("../assets/operations/scope-grant.v1.golden.json"),
    },
    ContractShape {
        name: "ContextCandidateSetV1",
        schema_path: "schemas/context-candidate-set.v1.schema.json",
        fixture_path: "schemas/operations/context-candidate-set.v1.golden.json",
        schema: include_str!("../assets/schemas/context-candidate-set.v1.schema.json"),
        fixture: include_str!("../assets/operations/context-candidate-set.v1.golden.json"),
    },
    ContractShape {
        name: "ContextPacketV1",
        schema_path: "schemas/context-packet.v1.schema.json",
        fixture_path: "schemas/operations/context-packet.v1.golden.json",
        schema: include_str!("../assets/schemas/context-packet.v1.schema.json"),
        fixture: include_str!("../assets/operations/context-packet.v1.golden.json"),
    },
    ContractShape {
        name: "ContextReceiptV1",
        schema_path: "schemas/context-receipt.v1.schema.json",
        fixture_path: "schemas/operations/context-receipt.v1.golden.json",
        schema: include_str!("../assets/schemas/context-receipt.v1.schema.json"),
        fixture: include_str!("../assets/operations/context-receipt.v1.golden.json"),
    },
    ContractShape {
        name: "KnowledgeEmissionV1",
        schema_path: "schemas/knowledge-emission.v1.schema.json",
        fixture_path: "schemas/operations/knowledge-emission.v1.golden.json",
        schema: include_str!("../assets/schemas/knowledge-emission.v1.schema.json"),
        fixture: include_str!("../assets/operations/knowledge-emission.v1.golden.json"),
    },
    ContractShape {
        name: "ObservableEventV1",
        schema_path: "schemas/observable-event.v1.schema.json",
        fixture_path: "schemas/operations/observable-event.v1.golden.json",
        schema: include_str!("../assets/schemas/observable-event.v1.schema.json"),
        fixture: include_str!("../assets/operations/observable-event.v1.golden.json"),
    },
    ContractShape {
        name: "WorkspaceEpochV1",
        schema_path: "schemas/workspace-epoch.v1.schema.json",
        fixture_path: "schemas/operations/workspace-epoch.v1.golden.json",
        schema: include_str!("../assets/schemas/workspace-epoch.v1.schema.json"),
        fixture: include_str!("../assets/operations/workspace-epoch.v1.golden.json"),
    },
    ContractShape {
        name: "DiagnosticEvidenceSnapshotV1",
        schema_path: "schemas/diagnostic-evidence-snapshot.v1.schema.json",
        fixture_path: "schemas/operations/diagnostic-evidence-snapshot.v1.golden.json",
        schema: include_str!("../assets/schemas/diagnostic-evidence-snapshot.v1.schema.json"),
        fixture: include_str!("../assets/operations/diagnostic-evidence-snapshot.v1.golden.json"),
    },
    ContractShape {
        name: "DiagnosticGateDecisionV1",
        schema_path: "schemas/diagnostic-gate-decision.v1.schema.json",
        fixture_path: "schemas/operations/diagnostic-gate-decision.v1.golden.json",
        schema: include_str!("../assets/schemas/diagnostic-gate-decision.v1.schema.json"),
        fixture: include_str!("../assets/operations/diagnostic-gate-decision.v1.golden.json"),
    },
    ContractShape {
        name: "CoverageObligationV1",
        schema_path: "schemas/coverage-obligation.v1.schema.json",
        fixture_path: "schemas/operations/coverage-obligation.v1.golden.json",
        schema: include_str!("../assets/schemas/coverage-obligation.v1.schema.json"),
        fixture: include_str!("../assets/operations/coverage-obligation.v1.golden.json"),
    },
];

pub mod explicit;

/// Absolute path of this crate's source root at build time (for tests that read from disk).
pub fn repo_root() -> &'static str {
    ROOT
}
