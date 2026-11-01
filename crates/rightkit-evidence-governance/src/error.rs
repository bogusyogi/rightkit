use thiserror::Error;

#[derive(Debug, Error)]
pub enum GovernanceError {
    #[error("{code}: {message}")]
    Typed { code: &'static str, message: String },
    #[error("canonical error: {0}")]
    Canonical(#[from] rightkit_evidence_contracts::canonical::CanonicalError),
    #[error("io error: {0}")]
    Io(String),
}

impl GovernanceError {
    pub fn typed(code: &'static str, message: impl Into<String>) -> Self {
        Self::Typed {
            code,
            message: message.into(),
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::Typed { code, .. } => code,
            Self::Canonical(_) => "ARC_CANONICALIZATION_FAILED",
            Self::Io(_) => "ARC_STORE_CORRUPT",
        }
    }
}
