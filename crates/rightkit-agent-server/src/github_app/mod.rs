//! Typed GitHub App authority contracts.
//!
//! This module is deliberately adapter-free: an integration owner wires these
//! contracts into daemon routes, storage, and the existing GitHub provider.
//! Secrets, installation scope, operation permissions, token expiry, webhook
//! identity, and replay admission stay explicit at that boundary.

mod cli;
mod daemon;
mod file_store;
mod forge;
pub(crate) mod forge_client;
mod installation;
mod service;
mod store;
mod token;
mod webhook;

pub use forge::Forge;
pub use installation::{
    AccessLevel, GitHubOperation, GitHubPermission, InstallationBinding, OperationRegistry,
    OperationScope, RepositoryScope,
};
pub use store::{GitHubAppStateBackend, GitHubAppStore, GitHubStoreError};
pub use token::{InstallationAccessToken, InstallationTokenLifecycle, TokenRequest};

/// Typed fail-closed outcomes shared by installation, token, and webhook
/// adapters. Variants are stable enough for API mapping and audit receipts.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "detail")]
pub enum GitHubDenial {
    AppDisabled,
    InstallationNotFound,
    InstallationMismatch,
    RepositoryRequired,
    RepositoryNotAllowed { operation: GitHubOperation },
    RepositoryOutOfScope { repository_id: u64 },
    PermissionNotGranted { operation: GitHubOperation },
    UnsupportedOperation { operation: GitHubOperation },
    MissingDeliveryId,
    InvalidDeliveryId,
    MissingEvent,
    InvalidEvent,
    MissingSignature,
    InvalidSignature,
    PayloadTooLarge,
    MissingInstallationIdentity,
    InvalidWebhookIdentity,
    Replay { delivery_id: String },
    ReplayWindowFull,
    TokenMissing,
    TokenExpired,
    TokenBindingMismatch,
}

/// Error envelope preserving typed denials while allowing adapter failures to
/// be surfaced without converting them into an authorization success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitHubAppError {
    Denied(GitHubDenial),
    InvalidBinding(String),
    InvalidToken(String),
    Adapter(String),
}

impl GitHubAppError {
    pub(crate) fn denied(denial: GitHubDenial) -> Self {
        Self::Denied(denial)
    }
}

impl std::fmt::Display for GitHubAppError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Denied(denial) => write!(formatter, "GitHub App denied: {denial:?}"),
            Self::InvalidBinding(reason) => {
                write!(formatter, "invalid GitHub App binding: {reason}")
            }
            Self::InvalidToken(reason) => write!(formatter, "invalid GitHub App token: {reason}"),
            Self::Adapter(reason) => write!(formatter, "GitHub App adapter failure: {reason}"),
        }
    }
}

impl std::error::Error for GitHubAppError {}
