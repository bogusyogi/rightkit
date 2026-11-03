//! Host-owned semantic-review invocation & foreground-memory cursor signals.
//!
//! RightKit supplies provider authority & lifecycle facts.  Membrane owns
//! semantic interpretation, proposal validation, and durable admission.

use super::unavailable::{BindingSurfaceV1, UnavailableReasonV1, UnavailableStateV1};
use serde::{Deserialize, Serialize};

pub const REVIEW_INVOCATION_SCHEMA_VERSION: u32 = 1;
pub const FOREGROUND_MEMORY_SIGNAL_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LiveProviderAuthorityV1 {
    /// Stable configured route identity; no credential or prompt is carried.
    pub provider_id: String,
    pub model_profile: String,
    pub authority_revision: String,
}

impl LiveProviderAuthorityV1 {
    fn validate(&self) -> Result<(), ReviewValidationError> {
        if self.provider_id.trim().is_empty()
            || self.model_profile.trim().is_empty()
            || self.authority_revision.trim().is_empty()
        {
            return Err(ReviewValidationError::EmptyField("provider_authority"));
        }
        Ok(())
    }
}

/// Durable cursor identifying source events already covered by foreground
/// memory.  Background extraction may use it only after a complete signal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForegroundMemoryCursorV1 {
    pub session_id: String,
    pub last_seq: u64,
    pub cursor_digest: String,
}

impl ForegroundMemoryCursorV1 {
    pub fn validate(&self) -> Result<(), ReviewValidationError> {
        if self.session_id.trim().is_empty() {
            return Err(ReviewValidationError::EmptyField("cursor.session_id"));
        }
        if self.last_seq == 0 {
            return Err(ReviewValidationError::InvalidCursor);
        }
        if !valid_digest(&self.cursor_digest) {
            return Err(ReviewValidationError::InvalidDigest("cursor_digest"));
        }
        Ok(())
    }

    pub fn digest_for(session_id: &str, last_seq: u64) -> String {
        let mut digest = rightkit_fs::digest::Sha256Hasher::new();
        digest.update(b"coderight.foreground-memory-cursor.v1\0");
        digest.update(session_id.as_bytes());
        digest.update(&last_seq.to_be_bytes());
        format!("sha256:{}", digest.finalize())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForegroundMemoryCompletionStateV1 {
    Complete,
    NotEmitted,
    Unavailable,
}

/// Foreground memory owns this signal.  It is not inferred from assistant
/// output, and an absent cursor never becomes an empty successful range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForegroundMemoryCompletionSignalV1 {
    pub schema_version: u32,
    pub signal_id: String,
    pub session_id: String,
    pub task_id: String,
    pub state: ForegroundMemoryCompletionStateV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ForegroundMemoryCursorV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<UnavailableStateV1>,
    pub observed_at_unix_ms: u64,
}

impl ForegroundMemoryCompletionSignalV1 {
    pub const SCHEMA_VERSION: u32 = FOREGROUND_MEMORY_SIGNAL_SCHEMA_VERSION;

    pub fn validate(&self) -> Result<(), ReviewValidationError> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(ReviewValidationError::SchemaVersion(self.schema_version));
        }
        for (field, value) in [
            ("signal_id", &self.signal_id),
            ("session_id", &self.session_id),
            ("task_id", &self.task_id),
        ] {
            if value.trim().is_empty() {
                return Err(ReviewValidationError::EmptyField(field));
            }
        }
        match self.state {
            ForegroundMemoryCompletionStateV1::Complete => {
                let cursor = self
                    .cursor
                    .as_ref()
                    .ok_or(ReviewValidationError::CursorRequired)?;
                cursor.validate()?;
                if cursor.session_id != self.session_id || self.unavailable.is_some() {
                    return Err(ReviewValidationError::SignalIdentityMismatch);
                }
            }
            ForegroundMemoryCompletionStateV1::NotEmitted => {
                if self.cursor.is_some() || self.unavailable.is_some() {
                    return Err(ReviewValidationError::SignalStateMismatch);
                }
            }
            ForegroundMemoryCompletionStateV1::Unavailable => {
                let unavailable = self
                    .unavailable
                    .ok_or(ReviewValidationError::UnavailableReasonRequired)?;
                if unavailable.surface != BindingSurfaceV1::ForegroundMemoryCursor
                    || self.cursor.is_some()
                {
                    return Err(ReviewValidationError::SignalStateMismatch);
                }
            }
        }
        Ok(())
    }
}

/// One bounded request sent to configured RightKit provider authority.
/// Input is referred to by digest; this contract does not embed Membrane
/// candidate ranking, planning, or semantic labels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackgroundSemanticReviewInvocationV1 {
    pub schema_version: u32,
    pub invocation_id: String,
    pub task_id: String,
    pub session_id: String,
    pub source_cursor: ForegroundMemoryCursorV1,
    pub provider_authority: LiveProviderAuthorityV1,
    pub input_digest: String,
    pub max_input_tokens: u64,
    pub deadline_at_unix_ms: u64,
    pub requested_at_unix_ms: u64,
}

impl BackgroundSemanticReviewInvocationV1 {
    pub const SCHEMA_VERSION: u32 = REVIEW_INVOCATION_SCHEMA_VERSION;

    pub fn validate(&self) -> Result<(), ReviewValidationError> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(ReviewValidationError::SchemaVersion(self.schema_version));
        }
        for (field, value) in [
            ("invocation_id", &self.invocation_id),
            ("task_id", &self.task_id),
            ("session_id", &self.session_id),
        ] {
            if value.trim().is_empty() {
                return Err(ReviewValidationError::EmptyField(field));
            }
        }
        self.source_cursor.validate()?;
        if self.source_cursor.session_id != self.session_id {
            return Err(ReviewValidationError::SignalIdentityMismatch);
        }
        self.provider_authority.validate()?;
        if !valid_digest(&self.input_digest) {
            return Err(ReviewValidationError::InvalidDigest("input_digest"));
        }
        if self.max_input_tokens == 0 || self.deadline_at_unix_ms <= self.requested_at_unix_ms {
            return Err(ReviewValidationError::InvalidBudgetOrDeadline);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewOutcomeStateV1 {
    Completed,
    Unavailable,
    Denied,
    Failed,
}

/// Outcome carries only lifecycle/provenance. Semantic output remains behind
/// Membrane's proposal/admission boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackgroundSemanticReviewOutcomeV1 {
    pub schema_version: u32,
    pub invocation_id: String,
    pub state: ReviewOutcomeStateV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<UnavailableStateV1>,
    pub completed_at_unix_ms: u64,
}

impl BackgroundSemanticReviewOutcomeV1 {
    pub fn validate(&self) -> Result<(), ReviewValidationError> {
        if self.schema_version != REVIEW_INVOCATION_SCHEMA_VERSION {
            return Err(ReviewValidationError::SchemaVersion(self.schema_version));
        }
        if self.invocation_id.trim().is_empty() {
            return Err(ReviewValidationError::EmptyField("invocation_id"));
        }
        match self.state {
            ReviewOutcomeStateV1::Completed => {
                let digest = self
                    .output_digest
                    .as_deref()
                    .ok_or(ReviewValidationError::OutputDigestRequired)?;
                if !valid_digest(digest) || self.unavailable.is_some() {
                    return Err(ReviewValidationError::OutcomeStateMismatch);
                }
            }
            ReviewOutcomeStateV1::Unavailable => {
                let unavailable = self
                    .unavailable
                    .ok_or(ReviewValidationError::UnavailableReasonRequired)?;
                if unavailable.surface != BindingSurfaceV1::BackgroundSemanticReview
                    || self.output_digest.is_some()
                {
                    return Err(ReviewValidationError::OutcomeStateMismatch);
                }
            }
            ReviewOutcomeStateV1::Denied | ReviewOutcomeStateV1::Failed => {
                if self.output_digest.is_some() {
                    return Err(ReviewValidationError::OutcomeStateMismatch);
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReviewValidationError {
    #[error("unsupported review schema version: {0}")]
    SchemaVersion(u32),
    #[error("review field is empty: {0}")]
    EmptyField(&'static str),
    #[error("foreground cursor is invalid")]
    InvalidCursor,
    #[error("review digest is invalid: {0}")]
    InvalidDigest(&'static str),
    #[error("foreground completion signal requires cursor")]
    CursorRequired,
    #[error("foreground completion signal requires typed unavailable reason")]
    UnavailableReasonRequired,
    #[error("foreground completion signal state does not match fields")]
    SignalStateMismatch,
    #[error("foreground completion signal identity does not match cursor")]
    SignalIdentityMismatch,
    #[error("review invocation budget or deadline is invalid")]
    InvalidBudgetOrDeadline,
    #[error("completed review outcome requires output digest")]
    OutputDigestRequired,
    #[error("review outcome state does not match fields")]
    OutcomeStateMismatch,
}

fn valid_digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

// ── CM-001 background semantic-review contract ─────────────────────────────

/// Version of the host-neutral loopback semantic-review seam.  The request
/// and result are transport shapes only: Cortex/Adapt admission remains
/// outside RightKit.
pub const BACKGROUND_SEMANTIC_REVIEW_SCHEMA_VERSION: u32 = 1;
pub const BACKGROUND_SEMANTIC_EVENT_SCHEMA_VERSION: u32 = 1;
pub const BACKGROUND_SEMANTIC_PROVENANCE_SCHEMA_VERSION: u32 = 1;

pub const MAX_BACKGROUND_SEMANTIC_EVENTS: usize = 128;
pub const MAX_BACKGROUND_SEMANTIC_EVENT_BYTES: usize = 256 * 1024;
pub const MAX_BACKGROUND_SEMANTIC_PROPOSALS: usize = 64;

/// Job classes served by the one RightKit provider seam.  Adding a class
/// does not create another model stack or another authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundReviewJobKindV1 {
    AdaptBehavioralReview,
    CortexSemanticDream,
    CortexMemoryCandidateExtraction,
}

impl BackgroundReviewJobKindV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AdaptBehavioralReview => "adapt_behavioral_review",
            Self::CortexSemanticDream => "cortex_semantic_dream",
            Self::CortexMemoryCandidateExtraction => "cortex_memory_candidate_extraction",
        }
    }
}

impl std::fmt::Display for BackgroundReviewJobKindV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Event provenance is required on every event sent to a background model.
/// It is evidence linkage, not semantic authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackgroundEventProvenanceV1 {
    pub source: String,
    pub receipt_id: String,
    pub receipt_digest: String,
}

impl BackgroundEventProvenanceV1 {
    pub fn validate(&self) -> Result<(), BackgroundSemanticReviewValidationError> {
        if self.source.trim().is_empty() {
            return Err(BackgroundSemanticReviewValidationError::EmptyField(
                "event.provenance.source",
            ));
        }
        if self.receipt_id.trim().is_empty() {
            return Err(BackgroundSemanticReviewValidationError::EmptyField(
                "event.provenance.receipt_id",
            ));
        }
        if !valid_digest(&self.receipt_digest) {
            return Err(BackgroundSemanticReviewValidationError::InvalidDigest(
                "event.provenance.receipt_digest",
            ));
        }
        Ok(())
    }
}

/// A bounded host event.  Payload is intentionally opaque to RightKit's
/// provider adapter; it must never be interpreted as a durable memory write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackgroundSessionEventV1 {
    pub schema_version: u32,
    pub session_id: String,
    pub seq: u64,
    pub event_id: String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub payload: serde_json::Value,
    pub content_hash: String,
    pub provenance: BackgroundEventProvenanceV1,
}

impl BackgroundSessionEventV1 {
    pub const SCHEMA_VERSION: u32 = BACKGROUND_SEMANTIC_EVENT_SCHEMA_VERSION;

    pub fn validate(&self) -> Result<(), BackgroundSemanticReviewValidationError> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(BackgroundSemanticReviewValidationError::SchemaVersion(
                self.schema_version,
            ));
        }
        for (field, value) in [
            ("event.session_id", &self.session_id),
            ("event.event_id", &self.event_id),
            ("event.type", &self.event_type),
        ] {
            if value.trim().is_empty() {
                return Err(BackgroundSemanticReviewValidationError::EmptyField(field));
            }
        }
        if self.seq == 0 {
            return Err(BackgroundSemanticReviewValidationError::InvalidEventSequence);
        }
        if !self.payload.is_object() {
            return Err(BackgroundSemanticReviewValidationError::InvalidEventPayload);
        }
        if !valid_digest(&self.content_hash) {
            return Err(BackgroundSemanticReviewValidationError::InvalidDigest(
                "event.content_hash",
            ));
        }
        self.provenance.validate()
    }

    pub fn estimated_input_tokens(&self) -> usize {
        serde_json::to_vec(&self.payload)
            .map(|bytes| bytes.len().saturating_add(3) / 4)
            .unwrap_or(usize::MAX)
    }
}

/// Half-open event sequence range used by the foreground memory writer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackgroundEventRangeV1 {
    pub session_id: String,
    pub start_seq: u64,
    pub end_seq: u64,
}

impl BackgroundEventRangeV1 {
    pub fn validate(&self) -> Result<(), BackgroundSemanticReviewValidationError> {
        if self.session_id.trim().is_empty() {
            return Err(BackgroundSemanticReviewValidationError::EmptyField(
                "foreground_memory_state.range.session_id",
            ));
        }
        if self.start_seq == 0 || self.start_seq >= self.end_seq {
            return Err(BackgroundSemanticReviewValidationError::InvalidEventRange);
        }
        Ok(())
    }

    pub fn overlaps(&self, other: &Self) -> bool {
        self.session_id == other.session_id
            && self.start_seq < other.end_seq
            && self.end_seq > other.start_seq
    }
}

/// Foreground memory state is authoritative input.  It is never inferred
/// from assistant text or from an absent cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ForegroundMemoryStateV1 {
    Unavailable { reason: UnavailableReasonV1 },
    AvailableNoEmission,
    AvailableEmission { range: BackgroundEventRangeV1 },
}

impl ForegroundMemoryStateV1 {
    fn validate(&self, session_id: &str) -> Result<(), BackgroundSemanticReviewValidationError> {
        match self {
            Self::Unavailable { .. } | Self::AvailableNoEmission => Ok(()),
            Self::AvailableEmission { range } => {
                range.validate()?;
                if range.session_id != session_id {
                    return Err(BackgroundSemanticReviewValidationError::IdentityMismatch(
                        "foreground_memory_state.range.session_id",
                    ));
                }
                Ok(())
            }
        }
    }

    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable { .. })
    }
}

/// Remaining input budget supplied by the scheduler immediately before one
/// review request.  The provider never expands either remainder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackgroundSemanticReviewBudgetV1 {
    pub per_turn_input_tokens: u64,
    pub aggregate_input_tokens: u64,
}

impl BackgroundSemanticReviewBudgetV1 {
    fn validate(self) -> Result<(), BackgroundSemanticReviewValidationError> {
        if self.per_turn_input_tokens == 0 {
            return Err(BackgroundSemanticReviewValidationError::InvalidBudget(
                "per_turn_input_tokens",
            ));
        }
        if self.aggregate_input_tokens == 0 {
            return Err(BackgroundSemanticReviewValidationError::InvalidBudget(
                "aggregate_input_tokens",
            ));
        }
        Ok(())
    }
}

/// Capabilities are an explicit deny-by-default list.  Any enabled effect
/// capability is rejected before the configured provider is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RestrictedCapabilitiesV1 {
    #[serde(default)]
    pub allow_tools: bool,
    #[serde(default)]
    pub allow_filesystem: bool,
    #[serde(default)]
    pub allow_network: bool,
    #[serde(default)]
    pub allow_process: bool,
    #[serde(default)]
    pub allow_context_reentry: bool,
    #[serde(default)]
    pub allow_membrane_write: bool,
    #[serde(default)]
    pub allow_cortex_write: bool,
}

impl RestrictedCapabilitiesV1 {
    fn validate(self) -> Result<(), BackgroundSemanticReviewValidationError> {
        if self.allow_tools
            || self.allow_filesystem
            || self.allow_network
            || self.allow_process
            || self.allow_context_reentry
            || self.allow_membrane_write
            || self.allow_cortex_write
        {
            return Err(BackgroundSemanticReviewValidationError::CapabilityEscalation);
        }
        Ok(())
    }
}

/// Alias retained for callers that describe the same configured route as a
/// provider identity.  The canonical fields live in
/// [`LiveProviderAuthorityV1`] and are shared with the existing review seam.
pub type BackgroundProviderIdentityV1 = LiveProviderAuthorityV1;

/// Background semantic review request.  The event window is complete input
/// supplied by RightKit; the model receives no tools, no context re-entry,
/// and no durable-store handle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackgroundSemanticReviewRequestV1 {
    pub schema_version: u32,
    pub job_id: String,
    pub request_id: String,
    pub job_kind: BackgroundReviewJobKindV1,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub turn_id: String,
    pub cursor: ForegroundMemoryCursorV1,
    #[serde(default)]
    pub events: Vec<BackgroundSessionEventV1>,
    pub foreground_memory_state: ForegroundMemoryStateV1,
    pub budgets: BackgroundSemanticReviewBudgetV1,
    pub deadline_at_unix_ms: u64,
    pub restricted_capabilities: RestrictedCapabilitiesV1,
    pub provider_authority: LiveProviderAuthorityV1,
}

impl BackgroundSemanticReviewRequestV1 {
    pub const SCHEMA_VERSION: u32 = BACKGROUND_SEMANTIC_REVIEW_SCHEMA_VERSION;

    pub fn validate(&self) -> Result<(), BackgroundSemanticReviewValidationError> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(BackgroundSemanticReviewValidationError::SchemaVersion(
                self.schema_version,
            ));
        }
        for (field, value) in [
            ("job_id", &self.job_id),
            ("request_id", &self.request_id),
            ("session_id", &self.session_id),
            ("turn_id", &self.turn_id),
        ] {
            if value.trim().is_empty() {
                return Err(BackgroundSemanticReviewValidationError::EmptyField(field));
            }
        }
        if self
            .task_id
            .as_deref()
            .is_some_and(|task_id| task_id.trim().is_empty())
        {
            return Err(BackgroundSemanticReviewValidationError::EmptyField(
                "task_id",
            ));
        }
        if self.deadline_at_unix_ms == 0 {
            return Err(BackgroundSemanticReviewValidationError::InvalidDeadline);
        }
        self.cursor
            .validate()
            .map_err(BackgroundSemanticReviewValidationError::from)?;
        if self.cursor.session_id != self.session_id {
            return Err(BackgroundSemanticReviewValidationError::IdentityMismatch(
                "cursor.session_id",
            ));
        }
        self.foreground_memory_state.validate(&self.session_id)?;
        self.budgets.validate()?;
        self.restricted_capabilities.validate()?;
        self.provider_authority
            .validate()
            .map_err(BackgroundSemanticReviewValidationError::from)?;
        if self.events.len() > MAX_BACKGROUND_SEMANTIC_EVENTS {
            return Err(BackgroundSemanticReviewValidationError::EventLimit);
        }

        let mut previous_seq = self.cursor.last_seq;
        let mut total_bytes = 0usize;
        let mut event_ids = std::collections::HashSet::new();
        for event in &self.events {
            event.validate()?;
            if event.session_id != self.session_id {
                return Err(BackgroundSemanticReviewValidationError::IdentityMismatch(
                    "event.session_id",
                ));
            }
            if event.seq <= self.cursor.last_seq || event.seq <= previous_seq {
                return Err(BackgroundSemanticReviewValidationError::EventNotAfterCursor);
            }
            previous_seq = event.seq;
            if !event_ids.insert(&event.event_id) {
                return Err(BackgroundSemanticReviewValidationError::DuplicateEventId(
                    event.event_id.clone(),
                ));
            }
            total_bytes = total_bytes.saturating_add(
                serde_json::to_vec(event)
                    .map_err(|_| BackgroundSemanticReviewValidationError::Serialization)?
                    .len(),
            );
            if total_bytes > MAX_BACKGROUND_SEMANTIC_EVENT_BYTES {
                return Err(BackgroundSemanticReviewValidationError::EventBytesLimit);
            }
        }
        Ok(())
    }

    pub fn event_range(&self) -> Option<BackgroundEventRangeV1> {
        let first = self.events.first()?;
        let last = self.events.last()?;
        Some(BackgroundEventRangeV1 {
            session_id: self.session_id.clone(),
            start_seq: first.seq,
            end_seq: last.seq.saturating_add(1),
        })
    }

    pub fn estimated_input_tokens(&self) -> usize {
        self.events
            .iter()
            .map(BackgroundSessionEventV1::estimated_input_tokens)
            .try_fold(0usize, usize::checked_add)
            .unwrap_or(usize::MAX)
    }
}

/// Cortex Stage-1 semantic curation kind.  This shape remains a proposal;
/// RightKit never interprets it as an admitted mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticCurationKindV1 {
    Contradiction,
    NearDuplicate,
    Supersession,
    StaleSemanticAssumption,
    Merge,
    Split,
    UsefulnessLifecycleReview,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewEvidenceRefV1 {
    #[serde(default)]
    pub memory_ids: Vec<String>,
    #[serde(default)]
    pub source_event_ids: Vec<String>,
    #[serde(default)]
    pub source_content_hashes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SemanticCurationProposalV1 {
    pub schema_version: u32,
    pub proposal_id: String,
    pub scope_id: String,
    pub kind: SemanticCurationKindV1,
    #[serde(default)]
    pub target_memory_ids: Vec<String>,
    pub evidence: Vec<ReviewEvidenceRefV1>,
}

impl SemanticCurationProposalV1 {
    pub const SCHEMA_VERSION: u32 = 1;

    pub fn validate(&self) -> Result<(), BackgroundSemanticReviewValidationError> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(BackgroundSemanticReviewValidationError::SchemaVersion(
                self.schema_version,
            ));
        }
        if self.proposal_id.trim().is_empty() {
            return Err(BackgroundSemanticReviewValidationError::EmptyField(
                "curation_proposal.proposal_id",
            ));
        }
        if self.scope_id.trim().is_empty() {
            return Err(BackgroundSemanticReviewValidationError::EmptyField(
                "curation_proposal.scope_id",
            ));
        }
        if self.evidence.is_empty() {
            return Err(BackgroundSemanticReviewValidationError::EmptyProposalEvidence);
        }
        let mut ids = std::collections::HashSet::new();
        for target in &self.target_memory_ids {
            if target.trim().is_empty() {
                return Err(BackgroundSemanticReviewValidationError::EmptyField(
                    "curation_proposal.target_memory_ids",
                ));
            }
        }
        for evidence in &self.evidence {
            if evidence.memory_ids.is_empty()
                && evidence.source_event_ids.is_empty()
                && evidence.source_content_hashes.is_empty()
            {
                return Err(BackgroundSemanticReviewValidationError::EmptyProposalEvidence);
            }
            for id in evidence
                .memory_ids
                .iter()
                .chain(evidence.source_event_ids.iter())
                .chain(evidence.source_content_hashes.iter())
            {
                if id.trim().is_empty() {
                    return Err(BackgroundSemanticReviewValidationError::EmptyField(
                        "curation_proposal.evidence",
                    ));
                }
                if !ids.insert(id) {
                    return Err(
                        BackgroundSemanticReviewValidationError::DuplicateEvidenceId(id.clone()),
                    );
                }
            }
        }
        Ok(())
    }

    pub fn validate_against(
        &self,
        request: &BackgroundSemanticReviewRequestV1,
    ) -> Result<(), BackgroundSemanticReviewValidationError> {
        self.validate()?;
        let event_ids = request
            .events
            .iter()
            .map(|event| event.event_id.as_str())
            .collect::<std::collections::HashSet<_>>();
        let content_hashes = request
            .events
            .iter()
            .map(|event| event.content_hash.as_str())
            .collect::<std::collections::HashSet<_>>();
        for evidence in &self.evidence {
            if evidence
                .source_event_ids
                .iter()
                .any(|event_id| !event_ids.contains(event_id.as_str()))
                || evidence
                    .source_content_hashes
                    .iter()
                    .any(|digest| !content_hashes.contains(digest.as_str()))
            {
                return Err(BackgroundSemanticReviewValidationError::ProposalProvenanceMismatch);
            }
        }
        Ok(())
    }
}

/// Model-derived extraction candidate.  Content remains untrusted until
/// Cortex validates/adjudicates it; exact event provenance is required here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MemoryCandidateV1 {
    pub schema_version: u32,
    pub candidate_id: String,
    pub session_id: String,
    pub from_seq: u64,
    pub to_seq: u64,
    pub scope_id: String,
    pub content: String,
    pub source_event_ids: Vec<String>,
    pub source_content_hashes: Vec<String>,
}

impl MemoryCandidateV1 {
    pub const SCHEMA_VERSION: u32 = 1;

    pub fn validate_against(
        &self,
        request: &BackgroundSemanticReviewRequestV1,
    ) -> Result<(), BackgroundSemanticReviewValidationError> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(BackgroundSemanticReviewValidationError::SchemaVersion(
                self.schema_version,
            ));
        }
        if self.candidate_id.trim().is_empty()
            || self.session_id.trim().is_empty()
            || self.scope_id.trim().is_empty()
            || self.content.trim().is_empty()
        {
            return Err(BackgroundSemanticReviewValidationError::EmptyField(
                "memory_candidate",
            ));
        }
        let Some(range) = request.event_range() else {
            return Err(BackgroundSemanticReviewValidationError::CandidateProvenanceMismatch);
        };
        if self.session_id != range.session_id
            || self.from_seq != range.start_seq
            || self.to_seq != range.end_seq
            || self.source_event_ids
                != request
                    .events
                    .iter()
                    .map(|event| event.event_id.clone())
                    .collect::<Vec<_>>()
            || self.source_content_hashes
                != request
                    .events
                    .iter()
                    .map(|event| event.content_hash.clone())
                    .collect::<Vec<_>>()
        {
            return Err(BackgroundSemanticReviewValidationError::CandidateProvenanceMismatch);
        }
        Ok(())
    }
}

/// Provider usage is optional per field.  A missing provider counter remains
/// `None`; zero is never manufactured as a measurement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackgroundMeasuredUsageV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_request_id: Option<String>,
}

/// Digest-bound provenance receipt for request execution and sink handoff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackgroundSemanticReviewProvenanceV1 {
    pub schema_version: u32,
    pub receipt_id: String,
    pub source: String,
    pub observed_at_unix_ms: u64,
    pub receipt_digest: String,
}

impl BackgroundSemanticReviewProvenanceV1 {
    pub fn from_material(
        receipt_id: impl Into<String>,
        source: impl Into<String>,
        observed_at_unix_ms: u64,
        material: &[u8],
    ) -> Self {
        let mut digest = rightkit_fs::digest::Sha256Hasher::new();
        digest.update(material);
        Self {
            schema_version: BACKGROUND_SEMANTIC_PROVENANCE_SCHEMA_VERSION,
            receipt_id: receipt_id.into(),
            source: source.into(),
            observed_at_unix_ms,
            receipt_digest: format!("sha256:{}", digest.finalize()),
        }
    }

    pub fn validate(&self) -> Result<(), BackgroundSemanticReviewValidationError> {
        if self.schema_version != BACKGROUND_SEMANTIC_PROVENANCE_SCHEMA_VERSION {
            return Err(BackgroundSemanticReviewValidationError::SchemaVersion(
                self.schema_version,
            ));
        }
        if self.receipt_id.trim().is_empty() || self.source.trim().is_empty() {
            return Err(BackgroundSemanticReviewValidationError::EmptyField(
                "provenance",
            ));
        }
        if !valid_digest(&self.receipt_digest) {
            return Err(BackgroundSemanticReviewValidationError::InvalidDigest(
                "provenance.receipt_digest",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum BackgroundSemanticReviewBlockReasonV1 {
    InvalidRequest,
    CursorUnavailable,
    CursorMismatch,
    ForegroundMemorySignalUnavailable,
    ForegroundMemoryEmissionOverlap,
    NoNewEvents,
    PerTurnBudgetExceeded,
    AggregateBudgetExceeded,
    DeadlineExceeded,
    CapabilityEscalation,
    ProviderUnavailable,
    ProviderIdentityMismatch,
    AuthenticationFailed,
    ProposalSinkUnavailable,
    CursorAdvanceFailed,
    AlreadyRunning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum BackgroundSemanticReviewFailureReasonV1 {
    InvalidProposal,
    ProviderFailed,
    ProviderTimeout,
    ProposalSinkFailed,
    CursorAdvanceFailed,
    UsageUnavailable,
    SerializationFailed,
    MeasuredBudgetExceeded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum BackgroundSemanticReviewStatusV1 {
    Proposals,
    Blocked {
        reason: BackgroundSemanticReviewBlockReasonV1,
    },
    Failed {
        reason: BackgroundSemanticReviewFailureReasonV1,
    },
}

/// Background semantic result.  Proposal arrays are untrusted material; no
/// field in this result is an admission or durable Cortex mutation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackgroundSemanticReviewResultV1 {
    pub schema_version: u32,
    pub job_id: String,
    pub request_id: String,
    pub job_kind: BackgroundReviewJobKindV1,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub turn_id: String,
    pub cursor: ForegroundMemoryCursorV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<ForegroundMemoryCursorV1>,
    #[serde(default)]
    pub curation_proposals: Vec<SemanticCurationProposalV1>,
    #[serde(default)]
    pub memory_candidates: Vec<MemoryCandidateV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<BackgroundMeasuredUsageV1>,
    pub provider_authority: LiveProviderAuthorityV1,
    pub provenance_receipt: BackgroundSemanticReviewProvenanceV1,
    pub status: BackgroundSemanticReviewStatusV1,
}

impl BackgroundSemanticReviewResultV1 {
    pub const SCHEMA_VERSION: u32 = BACKGROUND_SEMANTIC_REVIEW_SCHEMA_VERSION;

    pub fn validate_against(
        &self,
        request: &BackgroundSemanticReviewRequestV1,
    ) -> Result<(), BackgroundSemanticReviewValidationError> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(BackgroundSemanticReviewValidationError::SchemaVersion(
                self.schema_version,
            ));
        }
        if self.job_id != request.job_id
            || self.request_id != request.request_id
            || self.job_kind != request.job_kind
            || self.session_id != request.session_id
            || self.task_id != request.task_id
            || self.turn_id != request.turn_id
            || self.cursor != request.cursor
            || self.provider_authority != request.provider_authority
        {
            return Err(BackgroundSemanticReviewValidationError::ResultIdentityMismatch);
        }
        self.cursor
            .validate()
            .map_err(BackgroundSemanticReviewValidationError::from)?;
        if let Some(next_cursor) = &self.next_cursor {
            next_cursor
                .validate()
                .map_err(BackgroundSemanticReviewValidationError::from)?;
            if next_cursor.session_id != request.session_id
                || next_cursor.last_seq < request.cursor.last_seq
                || request
                    .events
                    .last()
                    .is_some_and(|event| next_cursor.last_seq > event.seq)
            {
                return Err(BackgroundSemanticReviewValidationError::NextCursorMismatch);
            }
        }
        self.provider_authority
            .validate()
            .map_err(BackgroundSemanticReviewValidationError::from)?;
        self.provenance_receipt.validate()?;
        if self.curation_proposals.len() > MAX_BACKGROUND_SEMANTIC_PROPOSALS
            || self.memory_candidates.len() > MAX_BACKGROUND_SEMANTIC_PROPOSALS
        {
            return Err(BackgroundSemanticReviewValidationError::ProposalLimit);
        }
        match &self.status {
            BackgroundSemanticReviewStatusV1::Proposals => {
                if request.events.is_empty() || self.next_cursor.is_none() {
                    return Err(BackgroundSemanticReviewValidationError::MissingNextCursor);
                }
                for proposal in &self.curation_proposals {
                    proposal.validate_against(request)?;
                }
                for candidate in &self.memory_candidates {
                    candidate.validate_against(request)?;
                }
            }
            BackgroundSemanticReviewStatusV1::Blocked { .. }
            | BackgroundSemanticReviewStatusV1::Failed { .. } => {
                if self.next_cursor.is_some()
                    || !self.curation_proposals.is_empty()
                    || !self.memory_candidates.is_empty()
                {
                    return Err(BackgroundSemanticReviewValidationError::StatusPayloadMismatch);
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackgroundSemanticReviewValidationError {
    #[error("background semantic-review schema version is unsupported: {0}")]
    SchemaVersion(u32),
    #[error("background semantic-review field is empty: {0}")]
    EmptyField(&'static str),
    #[error("background semantic-review cursor is invalid")]
    InvalidCursor,
    #[error("background semantic-review deadline is invalid")]
    InvalidDeadline,
    #[error("background semantic-review budget is invalid: {0}")]
    InvalidBudget(&'static str),
    #[error("background semantic-review capability escalation is forbidden")]
    CapabilityEscalation,
    #[error("background semantic-review digest is invalid: {0}")]
    InvalidDigest(&'static str),
    #[error("background semantic-review event sequence is invalid")]
    InvalidEventSequence,
    #[error("background semantic-review event payload is invalid")]
    InvalidEventPayload,
    #[error("background semantic-review event is not strictly after cursor")]
    EventNotAfterCursor,
    #[error("background semantic-review event limit exceeded")]
    EventLimit,
    #[error("background semantic-review event byte limit exceeded")]
    EventBytesLimit,
    #[error("background semantic-review event range is invalid")]
    InvalidEventRange,
    #[error("background semantic-review identity mismatch: {0}")]
    IdentityMismatch(&'static str),
    #[error("background semantic-review duplicate event id: {0}")]
    DuplicateEventId(String),
    #[error("background semantic-review serialization failed")]
    Serialization,
    #[error("background semantic-review proposal evidence is empty")]
    EmptyProposalEvidence,
    #[error("background semantic-review duplicate evidence id: {0}")]
    DuplicateEvidenceId(String),
    #[error("background semantic-review proposal provenance does not match request window")]
    ProposalProvenanceMismatch,
    #[error("background semantic-review candidate provenance does not match request window")]
    CandidateProvenanceMismatch,
    #[error("background semantic-review result identity does not match request")]
    ResultIdentityMismatch,
    #[error("background semantic-review next cursor does not match request")]
    NextCursorMismatch,
    #[error("background semantic-review proposals require next cursor")]
    MissingNextCursor,
    #[error("background semantic-review status and payload do not match")]
    StatusPayloadMismatch,
    #[error("background semantic-review proposal limit exceeded")]
    ProposalLimit,
}

impl From<ReviewValidationError> for BackgroundSemanticReviewValidationError {
    fn from(error: ReviewValidationError) -> Self {
        match error {
            ReviewValidationError::SchemaVersion(version) => Self::SchemaVersion(version),
            ReviewValidationError::EmptyField(field) => Self::EmptyField(field),
            ReviewValidationError::InvalidDigest(field) => Self::InvalidDigest(field),
            ReviewValidationError::InvalidCursor
            | ReviewValidationError::CursorRequired
            | ReviewValidationError::UnavailableReasonRequired
            | ReviewValidationError::SignalStateMismatch
            | ReviewValidationError::SignalIdentityMismatch
            | ReviewValidationError::InvalidBudgetOrDeadline
            | ReviewValidationError::OutputDigestRequired
            | ReviewValidationError::OutcomeStateMismatch => Self::InvalidCursor,
        }
    }
}
