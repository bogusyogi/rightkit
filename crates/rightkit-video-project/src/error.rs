use rightkit_timeline::TimelineError;
use thiserror::Error;

/// Every failure the project layer reports. [`ProjectError::code`] is the stable
/// machine-readable code used in the engine's JSON errors.
#[derive(Debug, Error)]
pub enum ProjectError {
    #[error("{0}")]
    BadInput(String),
    #[error("{0} is not a CutRight project (no project.json)")]
    NotAProject(String),
    #[error("{0}")]
    ProjectExists(String),
    #[error("project data is corrupt: {0}")]
    Corrupt(String),
    #[error("project is locked by another process: {0}")]
    Locked(String),
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Edit(TimelineError),
    #[error("nothing to undo")]
    NothingToUndo,
    #[error("nothing to redo")]
    NothingToRedo,
    #[error("cursor mismatch: expected {expected}, project is at {actual}")]
    CursorMismatch { expected: usize, actual: usize },
    #[error("settings changed since they were read (revision {actual}, expected {expected})")]
    RevisionConflict { expected: u64, actual: u64 },
    #[error("{0}")]
    MediaToolMissing(String),
    #[error("{0}")]
    EncoderUnavailable(String),
    #[error("{0}")]
    ColourUnavailable(String),
    #[error("probe failed: {0}")]
    ProbeFailed(String),
    #[error("audio decode failed: {0}")]
    AudioDecodeFailed(String),
    #[error("audio signal unavailable: {0}")]
    AudioSignalUnavailable(String),
    #[error("source file not found: {0}")]
    SourceNotFound(String),
    #[error("source changed while being read: {0}")]
    SourceChanged(String),
    #[error("source {id} does not match its registered BLAKE3: {detail}")]
    SourceHashMismatch { id: String, detail: String },
    #[error("render failed: {message}")]
    RenderFailed { message: String },
    #[error("render cancelled")]
    RenderCancelled,
    #[error("render timed out after {0} seconds")]
    RenderTimeout(u64),
}

impl ProjectError {
    pub fn code(&self) -> &'static str {
        match self {
            ProjectError::BadInput(_) => "bad_input",
            ProjectError::NotAProject(_) => "not_a_project",
            ProjectError::ProjectExists(_) => "project_exists",
            ProjectError::Corrupt(_) => "project_corrupt",
            ProjectError::Locked(_) => "project_locked",
            ProjectError::Io { .. } => "io_error",
            ProjectError::Edit(_) => "edit_rejected",
            ProjectError::NothingToUndo => "nothing_to_undo",
            ProjectError::NothingToRedo => "nothing_to_redo",
            ProjectError::CursorMismatch { .. } => "cursor_mismatch",
            ProjectError::RevisionConflict { .. } => "revision_conflict",
            ProjectError::MediaToolMissing(_) => "media_tool_missing",
            ProjectError::EncoderUnavailable(_) => "encoder_unavailable",
            ProjectError::ColourUnavailable(_) => "colour_unavailable",
            ProjectError::ProbeFailed(_) => "probe_failed",
            ProjectError::AudioDecodeFailed(_) => "audio_decode_failed",
            ProjectError::AudioSignalUnavailable(_) => "audio_signal_unavailable",
            ProjectError::SourceNotFound(_) => "source_not_found",
            ProjectError::SourceChanged(_) => "source_changed",
            ProjectError::SourceHashMismatch { .. } => "source_hash_mismatch",
            ProjectError::RenderFailed { .. } => "render_failed",
            ProjectError::RenderCancelled => "render_cancelled",
            ProjectError::RenderTimeout(_) => "render_timeout",
        }
    }

    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        ProjectError::Io {
            context: context.into(),
            source,
        }
    }
}

impl From<rightkit_ffmpeg::encoders::EncoderError> for ProjectError {
    fn from(e: rightkit_ffmpeg::encoders::EncoderError) -> Self {
        ProjectError::EncoderUnavailable(e.to_string())
    }
}

impl From<TimelineError> for ProjectError {
    fn from(e: TimelineError) -> Self {
        match e {
            TimelineError::NothingToUndo => ProjectError::NothingToUndo,
            TimelineError::NothingToRedo => ProjectError::NothingToRedo,
            TimelineError::Json(_)
            | TimelineError::SchemaVersion(_)
            | TimelineError::Integrity { .. } => ProjectError::Corrupt(e.to_string()),
            other => ProjectError::Edit(other),
        }
    }
}
