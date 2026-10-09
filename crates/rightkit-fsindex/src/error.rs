//! Error type.

use std::fmt;

#[derive(Debug)]
pub enum Error {
    /// The backend does not exist on this platform (e.g. the Windows stub).
    Unsupported(&'static str),
    Io(std::io::Error),
    /// A snapshot or state file could not be used.
    State(String),
    /// Content search was requested while it is disabled (feature or runtime switch).
    ContentDisabled,
    InvalidQuery(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unsupported(s) => write!(f, "unsupported: {s}"),
            Error::Io(e) => write!(f, "io: {e}"),
            Error::State(s) => write!(f, "state: {s}"),
            Error::ContentDisabled => write!(f, "content search is disabled"),
            Error::InvalidQuery(s) => write!(f, "invalid query: {s}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
