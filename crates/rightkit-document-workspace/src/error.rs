//! Typed error envelope for workspace command failures.

use rightkit_db::rusqlite;
use serde::Serialize;

/// Typed error codes for frontend-facing command failures. The serialized shape
/// (`{"code": "...", "message": "..."}`) is the contract the renderer classifies on.
#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
#[serde(tag = "code", content = "message", rename_all = "snake_case")]
pub enum WorkspaceError {
    /// The path-grant broker denied access to the requested path.
    BrokerDenied(String),
    /// The file was not found on disk.
    FileNotFound(String),
    /// The OS denied access to a location. The message is user-facing.
    PermissionDenied(String),
    /// The file type is not supported.
    UnsupportedFormat(String),
    /// The document is too large to open or save safely.
    DocumentTooLarge(String),
    /// An I/O error occurred.
    Io(String),
    /// The encoding could not be determined or applied.
    Encoding(String),
    /// The source changed after it was opened and before an in-place save committed.
    WriteConflict(String),
    /// A dialog operation was cancelled or failed.
    Dialog(String),
    /// The filesystem watcher could not be armed or torn down.
    Watch(String),
    /// The local SQLite persistence layer failed (query, migration, or lock).
    Persistence(String),
    /// A generic error that does not fit the other categories.
    Other(String),
}

impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BrokerDenied(m) => write!(f, "access denied: {m}"),
            Self::FileNotFound(m) => write!(f, "file not found: {m}"),
            Self::PermissionDenied(m) => write!(f, "permission denied: {m}"),
            Self::UnsupportedFormat(m) => write!(f, "unsupported format: {m}"),
            Self::DocumentTooLarge(m) => write!(f, "document too large: {m}"),
            Self::Io(m) => write!(f, "I/O error: {m}"),
            Self::Encoding(m) => write!(f, "encoding error: {m}"),
            Self::WriteConflict(m) => write!(f, "write conflict: {m}"),
            Self::Dialog(m) => write!(f, "dialog error: {m}"),
            Self::Watch(m) => write!(f, "watch error: {m}"),
            Self::Persistence(m) => write!(f, "storage error: {m}"),
            Self::Other(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for WorkspaceError {}

impl From<String> for WorkspaceError {
    fn from(s: String) -> Self {
        Self::Other(s)
    }
}

impl From<&str> for WorkspaceError {
    fn from(s: &str) -> Self {
        Self::Other(s.to_string())
    }
}

impl From<std::io::Error> for WorkspaceError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

impl From<rusqlite::Error> for WorkspaceError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Persistence(e.to_string())
    }
}

/// Classify a broker or document string error into the right envelope variant, preserving the
/// exact message text. Grant and document backends return `Result<_, String>`, so command sites
/// call this at the `map_err` boundary instead of relying on the blanket `From<String>`, which
/// would flatten every broker rejection to `Other` and lose the not-found/denied distinction.
pub fn broker_error(message: String) -> WorkspaceError {
    let lower = message.to_ascii_lowercase();
    if lower.contains("not authorized") || lower.contains("not just written") {
        WorkspaceError::BrokerDenied(message)
    } else if lower.contains("no such file")
        || lower.contains("cannot find the path")
        || lower.contains("cannot find the file")
        || lower.contains("not a file")
    {
        WorkspaceError::FileNotFound(message)
    } else {
        WorkspaceError::Other(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_error_classifies_by_message() {
        assert!(matches!(
            broker_error("path is not authorized".into()),
            WorkspaceError::BrokerDenied(_)
        ));
        assert!(matches!(
            broker_error("No such file or directory".into()),
            WorkspaceError::FileNotFound(_)
        ));
        assert!(matches!(
            broker_error("boom".into()),
            WorkspaceError::Other(_)
        ));
    }

    #[test]
    fn serializes_with_code_and_message() {
        let json = serde_json::to_string(&WorkspaceError::Io("x".into())).unwrap();
        assert_eq!(json, r#"{"code":"io","message":"x"}"#);
    }
}
