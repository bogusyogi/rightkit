//! Typed error envelope for shell commands that need category handling in the renderer.
//!
//! Donor: ViewRight `error.rs` (`{code, message}` with snake_case codes). The wire shape is
//! `{"code": "broker_denied", "message": "..."}` and a Tauri command returning
//! `Result<_, ShellError>` rejects its promise with exactly that object. Older shell commands
//! still reject with plain strings; the bridge normalises both (`ShellError` in `@rightkit/shell`).

use serde::Serialize;

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
#[serde(tag = "code", content = "message", rename_all = "snake_case")]
pub enum ShellError {
    /// The path broker (or a stream/site token) denied access.
    BrokerDenied(String),
    FileNotFound(String),
    /// The OS denied access (macOS TCC, folder ACLs, notification permission). User-facing text.
    PermissionDenied(String),
    UnsupportedFormat(String),
    DocumentTooLarge(String),
    Io(String),
    Encoding(String),
    WriteConflict(String),
    /// A request was malformed (bad argument, bad snapshot, unknown version).
    Invalid(String),
    /// The capability does not exist on this platform / build.
    Unsupported(String),
    /// An OS call did not answer in time.
    Timeout(String),
    Other(String),
}

impl ShellError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::BrokerDenied(_) => "broker_denied",
            Self::FileNotFound(_) => "file_not_found",
            Self::PermissionDenied(_) => "permission_denied",
            Self::UnsupportedFormat(_) => "unsupported_format",
            Self::DocumentTooLarge(_) => "document_too_large",
            Self::Io(_) => "io",
            Self::Encoding(_) => "encoding",
            Self::WriteConflict(_) => "write_conflict",
            Self::Invalid(_) => "invalid",
            Self::Unsupported(_) => "unsupported",
            Self::Timeout(_) => "timeout",
            Self::Other(_) => "other",
        }
    }
    pub fn message(&self) -> &str {
        match self {
            Self::BrokerDenied(m)
            | Self::FileNotFound(m)
            | Self::PermissionDenied(m)
            | Self::UnsupportedFormat(m)
            | Self::DocumentTooLarge(m)
            | Self::Io(m)
            | Self::Encoding(m)
            | Self::WriteConflict(m)
            | Self::Invalid(m)
            | Self::Unsupported(m)
            | Self::Timeout(m)
            | Self::Other(m) => m,
        }
    }
}

impl std::fmt::Display for ShellError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Self::BrokerDenied(_) => "access denied",
            Self::FileNotFound(_) => "file not found",
            Self::PermissionDenied(_) => "permission denied",
            Self::UnsupportedFormat(_) => "unsupported format",
            Self::DocumentTooLarge(_) => "document too large",
            Self::Io(_) => "I/O error",
            Self::Encoding(_) => "encoding error",
            Self::WriteConflict(_) => "write conflict",
            Self::Invalid(_) => "invalid request",
            Self::Unsupported(_) => "unsupported",
            Self::Timeout(_) => "timed out",
            Self::Other(_) => return write!(f, "{}", self.message()),
        };
        write!(f, "{label}: {}", self.message())
    }
}

impl std::error::Error for ShellError {}

impl From<String> for ShellError {
    fn from(s: String) -> Self {
        broker_error(s)
    }
}
impl From<&str> for ShellError {
    fn from(s: &str) -> Self {
        broker_error(s.to_owned())
    }
}

impl From<std::io::Error> for ShellError {
    fn from(e: std::io::Error) -> Self {
        use std::io::ErrorKind::*;
        match e.kind() {
            NotFound => Self::FileNotFound(e.to_string()),
            PermissionDenied => Self::PermissionDenied(e.to_string()),
            _ => Self::Io(e.to_string()),
        }
    }
}

/// Classify a string error from the broker or file helpers, preserving the message text.
pub fn broker_error(message: String) -> ShellError {
    let lower = message.to_ascii_lowercase();
    if lower.contains("not authorized")
        || lower.contains("not just written")
        || lower.contains("unknown or revoked")
        || lower.contains("unknown or already-used")
    {
        ShellError::BrokerDenied(message)
    } else if lower.contains("no such file")
        || lower.contains("cannot find the path")
        || lower.contains("cannot find the file")
        || lower.contains("not a file")
    {
        ShellError::FileNotFound(message)
    } else if lower.contains("permission denied") || lower.contains("operation not permitted") {
        ShellError::PermissionDenied(message)
    } else {
        ShellError::Other(message)
    }
}
