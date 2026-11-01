use thiserror::Error;

#[derive(Debug, Error)]
pub enum QaError {
    #[error("{0}")]
    BadInput(String),
    #[error("{0}")]
    ToolMissing(String),
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{tool} failed: {message}")]
    ToolFailed { tool: &'static str, message: String },
    #[error("unreadable media: {0}")]
    Probe(String),
    #[error("unknown preset or platform {0:?}")]
    UnknownPreset(String),
    /// Packaging refused: no QA receipt exists for this exact render.
    #[error(
        "no QA report for render blake3:{render_blake3}: run the QA gate on this exact file first"
    )]
    QaMissing { render_blake3: String },
    /// Packaging refused: the QA report for this exact render did not pass.
    #[error("QA failed for render blake3:{render_blake3}: {}", failing.join(", "))]
    QaFailed {
        render_blake3: String,
        failing: Vec<String>,
    },
    /// The QA receipt or report does not verify against the render or itself.
    #[error("QA receipt does not verify: {0}")]
    ReceiptInvalid(String),
    #[error("render does not fit {platform}: {reason}")]
    PlatformMismatch { platform: String, reason: String },
    #[error("invalid metadata for {platform}: {reason}")]
    MetadataInvalid { platform: String, reason: String },
    #[error("captions given to packaging differ from the captions QA checked (blake3:{packaged} vs blake3:{checked})")]
    CaptionsMismatch { packaged: String, checked: String },
    #[error("package does not verify: {0}")]
    PackageInvalid(String),
}

impl QaError {
    pub fn code(&self) -> &'static str {
        match self {
            QaError::BadInput(_) => "bad_input",
            QaError::ToolMissing(_) => "media_tool_missing",
            QaError::Io { .. } => "io_error",
            QaError::ToolFailed { .. } => "tool_failed",
            QaError::Probe(_) => "probe_failed",
            QaError::UnknownPreset(_) => "unknown_preset",
            QaError::QaMissing { .. } => "qa_missing",
            QaError::QaFailed { .. } => "qa_failed",
            QaError::ReceiptInvalid(_) => "qa_receipt_invalid",
            QaError::PlatformMismatch { .. } => "platform_mismatch",
            QaError::MetadataInvalid { .. } => "metadata_invalid",
            QaError::CaptionsMismatch { .. } => "captions_mismatch",
            QaError::PackageInvalid(_) => "package_invalid",
        }
    }

    pub(crate) fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        QaError::Io {
            context: context.into(),
            source,
        }
    }
}
