use std::fmt;

/// Why a channel-memory document or a citation was refused.
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
    /// The schema passed but a cross-field rule failed.
    Semantic { rule: String },
    /// A lookup named a scope other than the one the memory belongs to.
    ForeignScope { expected: String, found: String },
    /// A citation named a proof id that is not in the proof bank.
    UnknownProof { id: String },
    /// A citation named a proof that is not approved (pending or withdrawn).
    ProofNotApproved { id: String, status: String },
    /// A citation's value or units do not match the approved proof.
    ProofMismatch { id: String, reason: String },
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
            MemoryError::ForeignScope { expected, found } => {
                write!(
                    f,
                    "scope mismatch: memory is {expected}, lookup asked for {found}"
                )
            }
            MemoryError::UnknownProof { id } => write!(f, "proof `{id}` is not in the proof bank"),
            MemoryError::ProofNotApproved { id, status } => {
                write!(f, "proof `{id}` is {status}, not approved")
            }
            MemoryError::ProofMismatch { id, reason } => {
                write!(f, "proof `{id}` does not match the citation: {reason}")
            }
        }
    }
}

impl std::error::Error for MemoryError {}
