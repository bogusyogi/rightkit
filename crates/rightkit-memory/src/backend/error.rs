//! Typed failure values for every memory backend operation.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryBackendErrorKind {
    /// No authenticated, identity-bound Membrane backend is resident.
    BackendUnavailable,
    /// A durable mutation may have committed; reconcile its idempotency key.
    CommitUnknown,
    Unavailable,
    Invalid,
    Denied,
    Timeout,
    Cancelled,
    Incompatible,
    Store,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryBackendError {
    BackendUnavailable { code: String, message: String },
    CommitUnknown { code: String, message: String },
    Unavailable { code: String, message: String },
    Invalid { code: String, message: String },
    Denied { code: String, message: String },
    Timeout { code: String, message: String },
    Cancelled { code: String, message: String },
    Incompatible { code: String, message: String },
    Store { code: String, message: String },
    Internal { code: String, message: String },
}

impl MemoryBackendError {
    pub fn kind(&self) -> MemoryBackendErrorKind {
        match self {
            Self::BackendUnavailable { .. } => MemoryBackendErrorKind::BackendUnavailable,
            Self::CommitUnknown { .. } => MemoryBackendErrorKind::CommitUnknown,
            Self::Unavailable { .. } => MemoryBackendErrorKind::Unavailable,
            Self::Invalid { .. } => MemoryBackendErrorKind::Invalid,
            Self::Denied { .. } => MemoryBackendErrorKind::Denied,
            Self::Timeout { .. } => MemoryBackendErrorKind::Timeout,
            Self::Cancelled { .. } => MemoryBackendErrorKind::Cancelled,
            Self::Incompatible { .. } => MemoryBackendErrorKind::Incompatible,
            Self::Store { .. } => MemoryBackendErrorKind::Store,
            Self::Internal { .. } => MemoryBackendErrorKind::Internal,
        }
    }

    pub fn code(&self) -> &str {
        match self {
            Self::BackendUnavailable { code, .. }
            | Self::CommitUnknown { code, .. }
            | Self::Unavailable { code, .. }
            | Self::Invalid { code, .. }
            | Self::Denied { code, .. }
            | Self::Timeout { code, .. }
            | Self::Cancelled { code, .. }
            | Self::Incompatible { code, .. }
            | Self::Store { code, .. }
            | Self::Internal { code, .. } => code,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::BackendUnavailable { message, .. }
            | Self::CommitUnknown { message, .. }
            | Self::Unavailable { message, .. }
            | Self::Invalid { message, .. }
            | Self::Denied { message, .. }
            | Self::Timeout { message, .. }
            | Self::Cancelled { message, .. }
            | Self::Incompatible { message, .. }
            | Self::Store { message, .. }
            | Self::Internal { message, .. } => message,
        }
    }

    pub fn cancelled(message: impl Into<String>) -> Self {
        Self::Cancelled {
            code: "cancelled".into(),
            message: message.into(),
        }
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::Timeout {
            code: "timeout".into(),
            message: message.into(),
        }
    }

    /// Hub is not resident; callers may continue non-memory routes but must
    /// not fall back to a local memory authority.
    pub fn backend_unavailable(message: impl Into<String>) -> Self {
        Self::BackendUnavailable {
            code: "backend_unavailable".into(),
            message: message.into(),
        }
    }

    /// A mutating request may have reached Hub before transport loss. It is
    /// unsafe to report a normal failure or retry with a new key.
    pub fn commit_unknown(message: impl Into<String>) -> Self {
        Self::CommitUnknown {
            code: "commit_unknown".into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for MemoryBackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code(), self.message())
    }
}

impl std::error::Error for MemoryBackendError {}
