use std::fmt;

/// Stable reason codes. The `E_` names follow PostRight's
/// docs/contracts/p3-proof-semantics.md §5.4 and §7; `revision_conflict` is the
/// daemon-api v1 code from §6 A2.
///
/// `E_NUMBER_INVALID` and `E_UNIT_INVALID` are not in §5.4. They refuse a value
/// or unit that is malformed when a record is built (`Decimal::try_parse`,
/// `Unit::try_parse`). They are not `E_NUMBER_MISMATCH` or `E_UNIT_MISMATCH`,
/// which mean a citation disagrees with a recorded proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasonCode {
    ForeignScope,
    UnknownProof,
    ObservationAsFact,
    ProofNotApproved,
    ProofWithdrawn,
    ProofSuperseded,
    ProofRevisionStale,
    ProofExpired,
    ProofNotYetValid,
    ClaimNotPermitted,
    NumberMismatch,
    UnitMismatch,
    SubjectMismatch,
    PeriodMismatch,
    ProofConflict,
    SkillStatusForbidden,
    AuthorForbidden,
    IllegalTransition,
    RevisionConflict,
    NumberInvalid,
    UnitInvalid,
    ProofConflictScope,
}

impl ReasonCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ReasonCode::ForeignScope => "E_FOREIGN_SCOPE",
            ReasonCode::UnknownProof => "E_UNKNOWN_PROOF",
            ReasonCode::ObservationAsFact => "E_OBSERVATION_AS_FACT",
            ReasonCode::ProofNotApproved => "E_PROOF_NOT_APPROVED",
            ReasonCode::ProofWithdrawn => "E_PROOF_WITHDRAWN",
            ReasonCode::ProofSuperseded => "E_PROOF_SUPERSEDED",
            ReasonCode::ProofRevisionStale => "E_PROOF_REVISION_STALE",
            ReasonCode::ProofExpired => "E_PROOF_EXPIRED",
            ReasonCode::ProofNotYetValid => "E_PROOF_NOT_YET_VALID",
            ReasonCode::ClaimNotPermitted => "E_CLAIM_NOT_PERMITTED",
            ReasonCode::NumberMismatch => "E_NUMBER_MISMATCH",
            ReasonCode::UnitMismatch => "E_UNIT_MISMATCH",
            ReasonCode::SubjectMismatch => "E_SUBJECT_MISMATCH",
            ReasonCode::PeriodMismatch => "E_PERIOD_MISMATCH",
            ReasonCode::ProofConflict => "E_PROOF_CONFLICT",
            ReasonCode::SkillStatusForbidden => "E_SKILL_STATUS_FORBIDDEN",
            ReasonCode::AuthorForbidden => "E_AUTHOR_FORBIDDEN",
            ReasonCode::IllegalTransition => "E_ILLEGAL_TRANSITION",
            ReasonCode::RevisionConflict => "revision_conflict",
            ReasonCode::NumberInvalid => "E_NUMBER_INVALID",
            ReasonCode::UnitInvalid => "E_UNIT_INVALID",
            ReasonCode::ProofConflictScope => "E_PROOF_CONFLICT_SCOPE",
        }
    }
}

impl fmt::Display for ReasonCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a channel-memory document, a citation or a write was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryError {
    /// The text is not JSON, or it does not fit the typed model.
    Parse { message: String },
    /// `contract` names a different contract.
    WrongContract { found: String },
    /// `contractVersion` is missing.
    MissingVersion,
    /// `contractVersion` is not `major.minor.patch`.
    InvalidVersion { text: String },
    /// The major differs from 1, or the minor is newer than this build reads.
    UnsupportedVersion { offered: String },
    /// JSON Schema validation failed. Each entry is `path: message`.
    Schema { errors: Vec<String> },
    /// The schema passed but a document rule failed. Rules that the contract
    /// names with a code start with that code.
    Semantic { rule: String },
    /// A typed refusal: a citation, a status transition, a revision chain
    /// step or a controlled write.
    Refused { code: ReasonCode, detail: String },
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MemoryError::Parse { message } => write!(f, "channel memory parse: {message}"),
            MemoryError::WrongContract { found } => {
                write!(f, "contract: expected `channel-memory`, found `{found}`")
            }
            MemoryError::MissingVersion => write!(f, "channel-memory: missing contractVersion"),
            MemoryError::InvalidVersion { text } => {
                write!(f, "channel-memory: invalid contractVersion `{text}`")
            }
            MemoryError::UnsupportedVersion { offered } => {
                write!(f, "channel-memory: version {offered} is not readable by this build (supports 1.0.x)")
            }
            MemoryError::Schema { errors } => {
                write!(f, "channel-memory schema: {}", errors.join("; "))
            }
            MemoryError::Semantic { rule } => write!(f, "channel-memory semantics: {rule}"),
            MemoryError::Refused { code, detail } => write!(f, "{code}: {detail}"),
        }
    }
}

impl std::error::Error for MemoryError {}
