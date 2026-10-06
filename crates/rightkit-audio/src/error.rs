use std::path::PathBuf;

use rightkit_media::runner::RunError;

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("invalid audio parameter: {0}")]
    Invalid(String),
    #[error("input does not exist: {0}")]
    MissingInput(PathBuf),
    #[error("output must not overwrite the input")]
    OutputIsInput,
    #[error("no quiet non-speech region found for room tone: {0}")]
    NoRoomTone(String),
    /// FFmpeg could not run, was cancelled/timed out, or exited non-zero (the
    /// `RunError::Failed` variant carries the bounded stderr tail).
    #[error("ffmpeg: {0}")]
    Run(#[from] RunError),
    #[error("could not read ffmpeg measurement: {0}")]
    Measurement(String),
    #[error("preset JSON: {0}")]
    Preset(#[from] serde_json::Error),
    #[error("wav: {0}")]
    Wav(String),
}
