use rightkit_autoedit::cuts::CutError;
use rightkit_timeline::TimelineError;
use rightkit_transcript::TranscriptError;

#[derive(Debug, thiserror::Error)]
pub enum ShortsError {
    #[error("invalid shorts profile: {0}")]
    Profile(String),
    #[error("transcript: {0}")]
    Transcript(#[from] TranscriptError),
    #[error("proposal does not belong to this transcript/source: {0}")]
    Mismatch(String),
    #[error("unknown platform {0:?}")]
    UnknownPlatform(String),
    #[error("cut plan: {0}")]
    Cuts(#[from] CutError),
    #[error("timeline: {0}")]
    Timeline(#[from] TimelineError),
    #[error("{0}")]
    Invalid(String),
}
