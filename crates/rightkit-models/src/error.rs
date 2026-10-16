use serde::Serialize;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Cancelled,
    /// Install paused: staging and `.part` files are kept for a later resume.
    Paused,
    /// A shared-runtime file already exists with different bytes.
    Conflict,
    DiskSpace,
    Download,
    SizeMismatch,
    HashMismatch,
    Extract,
    Io,
    Busy,
    NotFound,
    NoRollback,
    NoFiles,
    InvalidSpec,
    /// Every URL of a file (`FileSpec::url` and its `mirrors`) failed; the message lists each
    /// mirror's error. Only produced when a file has more than one URL.
    AllMirrorsFailed,
}

#[derive(Debug, Clone, Serialize)]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
}

impl Error {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    pub fn io(context: impl fmt::Display, err: impl fmt::Display) -> Self {
        Self::new(ErrorCode::Io, format!("{context}: {err}"))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
