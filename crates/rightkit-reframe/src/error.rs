//! Typed errors for the reframe pipeline.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ReframeError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("ffmpeg failed: {0}")]
    Ffmpeg(String),
    #[error("ffprobe failed: {0}")]
    Ffprobe(String),
    #[error("model unavailable: {0}")]
    Model(String),
    #[error("onnx runtime: {0}")]
    Ort(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    /// The locked invariant: a vertical render needs an approved plan *and* approved anchors.
    #[error("reframe plan is not approved: {0}")]
    NotApproved(String),
    #[error("source media changed since analysis (expected blake3 {expected}, found {found})")]
    SourceChanged { expected: String, found: String },
    #[error("missing file: {0}")]
    Missing(PathBuf),
}

pub type Result<T> = std::result::Result<T, ReframeError>;
