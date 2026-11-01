use crate::cuts::CutError;
use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum EditorialError {
    #[error("cut engine: {0}")]
    Cuts(#[from] CutError),
    #[error("transcript is for source {found}, the plan was made for {expected}")]
    SourceMismatch { expected: String, found: String },
    #[error("review names unknown item {0}")]
    UnknownItem(String),
}
