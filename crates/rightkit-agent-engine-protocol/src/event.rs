mod durability;
mod envelope;
mod protocol_event;
mod types;

// Compatibility facade: this is the same type, not another settlement model.
pub use crate::effect_settlement::ExternalEffectSettlement;
pub use durability::EventDurability;
pub use envelope::{
    stable_event_id, EventCorrelationV1, EventEnvelopeV1, EVENT_ENVELOPE_SCHEMA_V1,
};
pub use protocol_event::{
    CompactionFederationDecisionV1, CompactionFederationSelectionReceiptV1, GoalSnapshotV1,
    GoalStepV1, ProtocolEvent, COMPACTION_FEDERATION_SCHEMA_V1,
};
pub use types::{
    AgentRosterRow, MiddlewareDecisionEvidence, ModelRouteDecisionPayload,
    OrchestrationObservation, PriceSnapshot, ProtocolEventMeta, RunBudgetExhausted, RunBudgetKind,
    RunFailureKind, RunOriginFailure, RunOwnershipAction, RunOwnershipFailure,
    SelfReviewProposalDraft, SelfReviewProposalKind, SessionUsageRollup, ShellExitStatus,
    UserQuestion, UserQuestionOption, VerificationIssue,
};
