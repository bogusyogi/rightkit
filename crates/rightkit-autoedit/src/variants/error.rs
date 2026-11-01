use crate::cuts::CutError;
use rightkit_timeline::TimelineError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum VariantError {
    #[error(
        "invalid variant name {0:?}: use 1-48 of a-z 0-9 - _ (first character a letter or digit)"
    )]
    BadName(String),
    #[error("variant {0:?} already exists")]
    Exists(String),
    #[error("variant {0:?} not found")]
    NotFound(String),
    #[error("cut engine: {0}")]
    Cuts(#[from] CutError),
    #[error("timeline: {0}")]
    Timeline(#[from] TimelineError),
    #[error("io {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("json {path}: {detail}")]
    Json { path: String, detail: String },
    #[error("variant {name:?} failed verification: {detail}")]
    Integrity { name: String, detail: String },
    #[error("{0}")]
    Mismatch(String),
    #[error("word {index} is not in both variants ({detail})")]
    WordNotShared { index: usize, detail: String },
    #[error("word index {0} is outside the transcript")]
    NoSuchWord(usize),
    #[error("invalid compare render: {0}")]
    Render(String),
}
