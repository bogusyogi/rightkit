use rightkit_timeline::TimelineError;
use rightkit_transcript::TranscriptError;
use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum CutError {
    #[error("invalid transcript: {0}")]
    Transcript(#[from] TranscriptError),
    #[error("transcript source id is not a BLAKE3 hex digest: {0}")]
    SourceId(String),
    #[error("nothing left to keep: the transcript has no speech that survives the edit")]
    NothingKept,
    #[error("timeline: {0}")]
    Timeline(#[from] TimelineError),
    #[error("{0}")]
    Group(String),
}
