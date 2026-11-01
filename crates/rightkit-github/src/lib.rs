//! GitHub App authority contracts, forge request specs, a file-backed App state store, webhook
//! verification, installation-token lifecycle, a `github` CLI command model, and desktop/CLI
//! device-flow sign-in ([`signin`]).
//!
//! The authority core is deliberately adapter-free: an integration owner wires these
//! contracts into daemon routes, storage, and the existing GitHub provider.
//! Secrets, installation scope, operation permissions, token expiry, webhook
//! identity, and replay admission stay explicit at that boundary.

pub mod cli;
pub mod daemon;
#[cfg(feature = "observer")]
pub mod delivery_observer;
#[cfg(feature = "observer")]
pub mod engineering;
pub mod file_store;
pub mod forge;
pub mod forge_client;
pub mod installation;
pub mod service;
pub mod signin;
pub mod store;
pub mod token;
pub mod webhook;

pub use forge::Forge;
pub use installation::{
    AccessLevel, GitHubOperation, GitHubPermission, InstallationBinding, OperationRegistry,
    OperationScope, RepositoryScope,
};
pub use store::{GitHubAppStateBackend, GitHubAppStore, GitHubStoreError, MemoryStateBackend};
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
    pub fn denied(denial: GitHubDenial) -> Self {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denial_is_serializable_without_secret_material() {
        let denial = GitHubDenial::RepositoryOutOfScope { repository_id: 42 };
        let json = serde_json::to_string(&denial).unwrap();
        assert!(json.contains("repository_out_of_scope"));
        assert!(json.contains("42"));
    }
}
