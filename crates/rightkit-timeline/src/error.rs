use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TimelineError {
    #[error("fps numerator and denominator must be positive")]
    InvalidFps,
    #[error("invalid source id {0:?}: expected 64 lowercase hex characters (BLAKE3)")]
    InvalidSourceId(String),
    #[error("unknown source {0}")]
    UnknownSource(String),
    #[error("unknown track {0}")]
    UnknownTrack(String),
    #[error("unknown clip {0}")]
    UnknownClip(String),
    #[error("duplicate id {0}")]
    DuplicateId(String),
    #[error("invalid range for {0}: {1}")]
    InvalidRange(String, String),
    #[error("index {index} out of range (len {len})")]
    IndexOutOfRange { index: usize, len: usize },
    #[error("clip {0} would start before time zero")]
    NegativeStart(String),
    #[error("clip {0} is not on a narration track")]
    NotNarration(String),
    #[error("source {0} lacks the required stream: {1}")]
    MissingStream(String, &'static str),
    #[error("invalid title {0}: {1}")]
    InvalidTitle(String, String),
    #[error("timeline is empty")]
    EmptyTimeline,
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("no render input path for source {0}")]
    MissingInputPath(String),
    #[error("nothing to undo")]
    NothingToUndo,
    #[error("nothing to redo")]
    NothingToRedo,
    #[error("json: {0}")]
    Json(String),
    #[error("unsupported schema version {0}")]
    SchemaVersion(u32),
    #[error("revision {seq} integrity failure: {detail}")]
    Integrity { seq: u64, detail: String },
}
