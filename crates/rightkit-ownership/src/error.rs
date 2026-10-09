//! Scan failures. Node throws `Error` with an optional `RK_OWNERSHIP_*` code; the worker
//! boundary reports the code only when it has that prefix and `RK_OWNERSHIP_UNVERIFIED`
//! otherwise.

use crate::verdict::codes;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanError {
    pub message: String,
    pub code: Option<&'static str>,
}

impl ScanError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: None,
        }
    }

    pub fn with_code(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: Some(code),
        }
    }

    pub fn invalid_inputs(message: impl Into<String>) -> Self {
        Self::with_code(codes::INVALID_INPUTS, message)
    }

    /// The code the worker writes to its error line.
    pub fn reported_code(&self) -> &'static str {
        self.code.unwrap_or(codes::UNVERIFIED)
    }
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ScanError {}

impl From<String> for ScanError {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

pub type Res<T> = Result<T, ScanError>;
