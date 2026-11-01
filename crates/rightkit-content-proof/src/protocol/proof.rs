//! Payload shapes for P3 proof semantics (docs/contracts/p3-proof-semantics.md): claim review (§5), the
//! review epoch (§5.1) and the 90-day expiry of unapproved collected proofs (decided 2026-10-16). The
//! daemon is the authority; these are the engine's wire shapes only.
use serde::{Deserialize, Serialize};

use super::Scope;

/// Days a collected proof may wait for Adrian's approval before it is marked expired (decision 2026-10-16).
pub const COLLECTED_EXPIRY_DAYS: i64 = 90;

/// Every §5.4 reason code the claim review can return, plus the two the 2026-10-16 decisions add.
pub const REVIEW_CODES: &[&str] = &[
    "E_UNKNOWN_PROOF",
    "E_FOREIGN_SCOPE",
    "E_OBSERVATION_AS_FACT",
    "E_PROOF_NOT_APPROVED",
    "E_PROOF_WITHDRAWN",
    "E_PROOF_SUPERSEDED",
    "E_PROOF_REVISION_STALE",
    "E_PROOF_EXPIRED",
    "E_PROOF_NOT_YET_VALID",
    "E_CLAIM_NOT_PERMITTED",
    "E_NUMBER_MISMATCH",
    "E_UNIT_MISMATCH",
    "E_SUBJECT_MISMATCH",
    "E_PERIOD_MISMATCH",
    "E_TEXT_ANNOTATION_MISMATCH",
    "E_UNBOUND_NUMBER",
    "E_STALE_REVIEW",
    "E_PROOF_CONFLICT",
    "E_RETIRED_CLAIM_TEXT",
    "E_COLLECTED_EXPIRED",
    "E_VALID_UNTIL_REQUIRED",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProofPeriod {
    pub from: String,
    pub to: String,
}

/// Character range in the text, as Unicode scalar offsets, end exclusive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProofSpan {
    pub start: usize,
    pub end: usize,
}

/// One claim annotation (§5.2). `value` is as it reads in the text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProofAnnotation {
    pub claim_id: String,
    pub proof_id: String,
    pub proof_revision: u32,
    pub value: String,
    pub units: String,
    pub subject: String,
    pub period: ProofPeriod,
    pub span: ProofSpan,
    /// The claim wording the copy relies on. When present it must equal (trim, lowercase) the text of one of
    /// the proof's `permittedClaims`, else `E_CLAIM_NOT_PERMITTED` (issue #296).
    #[serde(default)]
    pub claim_text: Option<String>,
}

/// `proof.claim_review` input. `now` is a UTC instant; today is its date. `epoch` is the brief's review
/// epoch the caller read; a review under an older epoch than the stored one is stale.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimReviewInput {
    pub scope: Scope,
    pub brief_id: String,
    pub epoch: u32,
    pub text: String,
    pub annotations: Vec<ProofAnnotation>,
    #[serde(default)]
    pub now: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewFinding {
    pub step: u8,
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub found: Option<String>,
    pub detail: String,
}

/// `proof.claim_review` output. `disposition` is `pass` or `blocked`; only `pass` makes a brief ready.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimReviewOutput {
    pub scope: Scope,
    pub brief_id: String,
    pub epoch: u32,
    pub current_epoch: u32,
    pub text_hash: String,
    pub disposition: String,
    pub findings: Vec<ReviewFinding>,
    pub review_id: i64,
}

/// `proof.review_epoch_bump` input: a retrieval, script change or claim change moves the epoch on.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EpochBumpInput {
    pub scope: Scope,
    pub brief_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpochOutput {
    pub scope: Scope,
    pub brief_id: String,
    pub epoch: u32,
}

/// `proof.review_records` input: the stored reviews of one brief, newest first.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewRecordsInput {
    pub scope: Scope,
    pub brief_id: String,
}

/// `proof.expire_collected` input. `now` is a UTC instant (default: the clock).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpireCollectedInput {
    pub scope: Scope,
    #[serde(default)]
    pub now: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpireCollectedOutput {
    pub scope: Scope,
    pub expired: Vec<String>,
    pub expired_total: u64,
}

/// `proof.approval_check` input: would Adrian's approval of this stored collected proof be allowed now?
/// Runs the policy of the 2026-10-16 decisions and writes nothing.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalCheckInput {
    pub scope: Scope,
    pub proof_id: String,
    pub author: String,
    #[serde(default)]
    pub valid_until: Option<String>,
    #[serde(default)]
    pub now: Option<String>,
}
