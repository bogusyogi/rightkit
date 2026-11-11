//! `rightkit-agent-engine-protocol` — pure data types crate.
//!
//! No I/O, no async.  All types implement `serde::Serialize` + `serde::Deserialize`
//! using camelCase field names and `$kind` discriminators that match the TypeScript
//! frontend types in `host-app/src/lib/types.ts` exactly.

pub mod approvals;
pub mod artifact;
pub mod effect_settlement;
pub mod event;
pub mod ids;
pub mod message;
pub mod op;
pub mod protocol;
pub mod references;
pub mod refs;
pub mod retry_notice;
pub mod schedule;
pub mod secret_scan;
pub mod task;
pub mod time;
pub mod tool;
pub mod tool_output;
pub mod traits;
pub mod trust;

// ── Most-used re-exports ──────────────────────────────────────────────────────
pub use approvals::{
    effect_binding, ElicitationAction, ElicitationRequest, ElicitationRequestEvent,
    ExecApprovalKind, ExecApprovalRequestEvent, ExecPolicyAmendment, GuardianAssessmentAction,
    GuardianAssessmentDecisionSource, GuardianAssessmentEvent, GuardianAssessmentOutcome,
    GuardianAssessmentStatus, GuardianCommandSource, GuardianRiskLevel, GuardianUserAuthorization,
    NetworkApprovalContext, NetworkApprovalProtocol, NetworkPolicyAmendment,
    NetworkPolicyRuleAction, PendingApprovalContext, PermissionDecision, PermissionDecisionResult,
    PermissionGateRequest, PermissionGateResponse, ReviewDecision,
};
pub use artifact::{ArtifactApproval, ArtifactRow, ArtifactVerdict};
pub use effect_settlement::ExternalEffectSettlement;
pub use event::{
    AgentRosterRow, CompactionFederationDecisionV1, CompactionFederationSelectionReceiptV1,
    MiddlewareDecisionEvidence, ModelRouteDecisionPayload, OrchestrationObservation, PriceSnapshot,
    ProtocolEvent, ProtocolEventMeta, RunBudgetExhausted, RunBudgetKind, RunFailureKind,
    RunOriginFailure, RunOwnershipAction, RunOwnershipFailure, SelfReviewProposalDraft,
    SelfReviewProposalKind, SessionUsageRollup, ShellExitStatus, UserQuestion, UserQuestionOption,
    VerificationIssue, COMPACTION_FEDERATION_SCHEMA_V1,
};
pub use ids::{
    assistant_message_id, is_canonical_uuid_v7, message_id, tool_call_origin_id, tool_result_id,
    uuid_v7,
};
pub use message::TranscriptMessage;
pub use op::Op;
pub use protocol::{AskForApproval, GranularApprovalConfig, NetworkAccess};
pub use references::{ReferenceMeta, ReferenceProvenance};
pub use refs::{ArtifactRef, OutputRef, TOOL_OUTPUT_INTERNAL_KIND};
pub use retry_notice::{RetryNotice, RetryNotifier};
/// The task contract this protocol uses for authority: task statuses, the approval binding,
/// principals and intent provenance.
pub use rightkit_agent_task_contract as task_contract;
/// The approvals core's binding a pending approval carries.
pub use rightkit_approvals::Binding as ApprovalCoreBinding;
pub use schedule::{
    default_job_mode, default_schema_version, ScheduleRunEvent, ScheduleRunResourceLimits,
    ScheduleRunState,
};
pub use task::{CompletionPredicateResult, DoneWhenPredicate, TaskNode, TaskState};
pub use time::now_iso;
pub use tool::ToolCall;
pub use tool_output::{ToolOutputNotifier, ToolOutputStream};
pub use traits::{ArtifactTrustStore, SessionConduit};
pub use trust::{ContentTrust, TrustEnvelope};
