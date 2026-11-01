//! Daemon integration for the GitHub App authority lane.
//!
//! This module owns the narrow integration seam between the typed
//! `github_app` contracts and the Axum daemon: state-path resolution, store
//! + authority construction, webhook admission, and bearer mapping. It does
//!   not mutate `DaemonState` directly — the router owner mounts the routes
//!   produced here and supplies storage + lifecycle implementations.

use super::{
    file_store::{FileStateBackend, FileStoreError},
    forge::{Forge, ForgeOperation},
    installation::{InstallationBinding, RepositoryScope},
    service::{AuthorityAudit, AuthorityOutcome, GitHubAppAuthority, GitHubAuthorityError},
    store::{GitHubAppStateBackend, GitHubAppStore, GitHubStoreError},
    token::{InstallationAccessToken, InstallationTokenLifecycle, TokenRequest},
    webhook::{GitHubWebhookHeaders, VerifiedWebhook, WebhookIdentity, WebhookVerifier},
    GitHubAppError, GitHubDenial, GitHubOperation,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Daemon-owned defaults. Keep replay bounds identical to the store so
/// webhook and token lanes share one TTL/capacity story.
pub const DEFAULT_REPLAY_CAPACITY: usize = 8;
pub const DEFAULT_REPLAY_TTL_SECONDS: u64 = 300;
pub const DEFAULT_MAX_CACHED_TOKENS: usize = 32;
pub const DEFAULT_REFRESH_SKEW_SECONDS: u64 = 30;

/// Resolve the GitHub App state file path from the daemon database path: the
/// file sits next to the database, or under `state_root` when no database path
/// is configured.
pub fn github_app_state_path(database_path: &str, state_root: &Path) -> PathBuf {
    let trimmed = database_path.trim();
    if !trimmed.is_empty() {
        let db = PathBuf::from(trimmed);
        return db
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("github-app-state.json");
    }
    state_root.join("github-app-state.json")
}

/// Open a file-backed `GitHubAppStore` at the daemon's state path.
pub fn open_file_store(
    state_path: impl Into<PathBuf>,
) -> Result<GitHubAppStore<FileStateBackend>, FileStoreError> {
    let backend = FileStateBackend::open(state_path)?;
    GitHubAppStore::open(backend, DEFAULT_REPLAY_CAPACITY, DEFAULT_REPLAY_TTL_SECONDS)
        .map_err(|error| FileStoreError::Unavailable(error.to_string()))
}

/// Open a `GitHubAppAuthority` from a ready store + lifecycle.
pub fn open_authority<B, L>(
    store: GitHubAppStore<B>,
    lifecycle: L,
) -> Result<GitHubAppAuthority<B, L>, GitHubAuthorityError>
where
    B: GitHubAppStateBackend + Send + Sync,
    L: InstallationTokenLifecycle + Send + Sync,
{
    GitHubAppAuthority::open(
        store,
        lifecycle,
        DEFAULT_MAX_CACHED_TOKENS,
        DEFAULT_REFRESH_SKEW_SECONDS,
    )
}

/// Typed request body for `POST /github/app/installations`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallRequest {
    pub app_id: u64,
    pub installation_id: u64,
    pub account_id: Option<u64>,
    pub account_login: String,
    pub repository_ids: Vec<u64>,
    pub permissions: std::collections::BTreeMap<super::GitHubPermission, super::AccessLevel>,
    pub generation: u64,
}

impl InstallRequest {
    pub fn into_binding(self) -> Result<InstallationBinding, GitHubAppError> {
        let scope = if self.repository_ids.is_empty() {
            RepositoryScope::AllGranted
        } else {
            super::forge::repository_scope_from_ids(&self.repository_ids)?
        };
        InstallationBinding::new(
            self.app_id,
            self.installation_id,
            self.account_id,
            self.account_login,
            scope,
            self.permissions,
            self.generation,
        )
    }
}

/// Typed request for `POST /github/app/installations/{app_id}/{installation_id}/tokens`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueTokenRequest {
    pub forge: Option<String>,
    pub operation: ForgeOperation,
    pub repository_ids: Vec<u64>,
}

/// Typed webhook delivery envelope for `POST /github/app/webhooks/{app_id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookDeliveryRequest {
    pub headers: GitHubWebhookHeaders,
    pub body: Vec<u8>,
    pub identity: WebhookIdentity,
}

/// Minimal daemon response for installation writes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallationResponse {
    pub app_id: u64,
    pub installation_id: u64,
    pub generation: u64,
    pub enabled: bool,
    pub audit: AuthorityAudit,
}

/// Minimal daemon response for token issuance (never serializes the token
/// value itself — callers receive a redacted handle plus metadata).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenIssuedResponse {
    pub app_id: u64,
    pub installation_id: u64,
    pub operation: ForgeOperation,
    pub repository_ids: Vec<u64>,
    pub expires_at: u64,
    pub audit: AuthorityAudit,
}

impl TokenIssuedResponse {
    pub fn from_outcome(outcome: AuthorityOutcome<InstallationAccessToken>) -> Self {
        Self {
            app_id: outcome.value.app_id,
            installation_id: outcome.value.installation_id,
            operation: super::forge::ForgeRegistry::forge_for_github_operation(
                outcome.value.operation,
            ),
            repository_ids: outcome.value.repository_ids.clone(),
            expires_at: outcome.value.expires_at,
            audit: outcome.audit,
        }
    }
}

/// Verify one webhook delivery against the app secret, then reserve its
/// delivery id in the durable store. Returns `VerifiedWebhook` on success.
pub fn verify_and_reserve_webhook<B>(
    verifier: &WebhookVerifier,
    store: &GitHubAppStore<B>,
    headers: &GitHubWebhookHeaders,
    body: &[u8],
    identity: WebhookIdentity,
    now_unix: u64,
) -> Result<VerifiedWebhook, GitHubAppError>
where
    B: GitHubAppStateBackend,
{
    // Use a transient replay guard for signature/identity checks, but anchor
    // replay in the durable store so restarts cannot double-deliver.
    let mut guard =
        super::webhook::ReplayGuard::new(DEFAULT_REPLAY_CAPACITY, DEFAULT_REPLAY_TTL_SECONDS)
            .map_err(|error| GitHubAppError::Adapter(error.to_string()))?;
    let verified = verifier.verify(headers, body, identity, &mut guard, now_unix)?;
    let app_id = verified.identity.app_id;
    let installation_id = verified
        .identity
        .installation_id
        .ok_or_else(|| GitHubAppError::denied(GitHubDenial::MissingInstallationIdentity))?;
    store
        .reserve_delivery(
            app_id,
            installation_id,
            verified.delivery_id.clone(),
            now_unix,
        )
        .map_err(|error| match error {
            GitHubStoreError::Denied(denial) => GitHubAppError::Denied(denial),
            other => GitHubAppError::Adapter(other.to_string()),
        })?;
    Ok(verified)
}

/// Build a `TokenRequest` for a forge operation, validating repository scope
/// and permission shape before the lifecycle is invoked.
pub fn token_request_for_forge(
    binding: &InstallationBinding,
    operation: ForgeOperation,
    repository_ids: &[u64],
) -> Result<TokenRequest, GitHubAppError> {
    let github_operation = operation.github_operation().ok_or_else(|| {
        GitHubAppError::denied(GitHubDenial::UnsupportedOperation {
            operation: GitHubOperation::WebhookDelivery,
        })
    })?;
    binding.token_request(github_operation, repository_ids)
}

/// Map a `GitHubAppError` / `GitHubAuthorityError` / `GitHubStoreError` into
/// an HTTP status code understood by the daemon router. Kept in one place so
/// every GitHub App route returns the same typed mapping.
pub fn http_status_for_denial(denial: &GitHubDenial) -> u16 {
    match denial {
        GitHubDenial::AppDisabled => 403,
        GitHubDenial::InstallationNotFound => 404,
        GitHubDenial::InstallationMismatch => 409,
        GitHubDenial::RepositoryRequired => 400,
        GitHubDenial::RepositoryNotAllowed { .. } => 400,
        GitHubDenial::RepositoryOutOfScope { .. } => 403,
        GitHubDenial::PermissionNotGranted { .. } => 403,
        GitHubDenial::UnsupportedOperation { .. } => 400,
        GitHubDenial::MissingDeliveryId => 400,
        GitHubDenial::InvalidDeliveryId => 400,
        GitHubDenial::MissingEvent => 400,
        GitHubDenial::InvalidEvent => 400,
        GitHubDenial::MissingSignature => 401,
        GitHubDenial::InvalidSignature => 401,
        GitHubDenial::PayloadTooLarge => 413,
        GitHubDenial::MissingInstallationIdentity => 400,
        GitHubDenial::InvalidWebhookIdentity => 400,
        GitHubDenial::Replay { .. } => 409,
        GitHubDenial::ReplayWindowFull => 429,
        GitHubDenial::TokenMissing => 404,
        GitHubDenial::TokenExpired => 401,
        GitHubDenial::TokenBindingMismatch => 409,
    }
}

pub fn http_status_for_app_error(error: &GitHubAppError) -> u16 {
    match error {
        GitHubAppError::Denied(denial) => http_status_for_denial(denial),
        GitHubAppError::InvalidBinding(_) | GitHubAppError::InvalidToken(_) => 400,
        GitHubAppError::Adapter(_) => 500,
    }
}

pub fn http_status_for_authority_error(error: &GitHubAuthorityError) -> u16 {
    match error {
        GitHubAuthorityError::Denied(denial) => http_status_for_denial(denial),
        GitHubAuthorityError::Store(store) => match store {
            GitHubStoreError::NotFound { .. } => 404,
            GitHubStoreError::StaleGeneration { .. } => 409,
            GitHubStoreError::Denied(denial) => http_status_for_denial(denial),
            GitHubStoreError::InvalidState(_) => 400,
            GitHubStoreError::LoadCorrupt(_) => 500,
            GitHubStoreError::LoadUnavailable(_) | GitHubStoreError::WriteUnavailable(_) => 503,
        },
        GitHubAuthorityError::Lifecycle(app) => http_status_for_app_error(app),
        GitHubAuthorityError::Invalid(_) => 400,
        GitHubAuthorityError::CacheFull => 429,
        GitHubAuthorityError::Concurrency(_) => 500,
    }
}

/// Describe the forge's API base URL (used by `engine/crates/tools/src/github.rs`
/// when the tool runs under a GitHub App installation token).
pub fn forge_api_base(forge: Forge) -> &'static str {
    match forge {
        Forge::GitHub => "https://api.github.com",
        Forge::GitLab => "https://gitlab.com/api/v4",
        Forge::Bitbucket => "https://api.bitbucket.org/2.0",
        Forge::AzureDevOps => "https://dev.azure.com",
    }
}

/// Whether the daemon's bearer authority should gate this path. Webhook
/// deliveries carry their own HMAC; every other GitHub App route requires the
/// daemon bearer.
pub fn requires_daemon_bearer(path: &str) -> bool {
    !path.contains("/webhooks/") || path.contains("/installations")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AccessLevel, GitHubPermission, MemoryStateBackend, RepositoryScope};
    use std::collections::{BTreeMap, BTreeSet};

    fn binding() -> InstallationBinding {
        InstallationBinding::new(
            7,
            9,
            Some(11),
            "acme",
            RepositoryScope::Explicit(BTreeSet::from([42])),
            BTreeMap::from([(GitHubPermission::Contents, AccessLevel::Read)]),
            1,
        )
        .unwrap()
    }

    #[test]
    fn state_path_is_daemon_scoped() {
        let base = std::env::temp_dir().join("rk-github-app");
        let db = base.join("db.sqlite");
        let state_root = base.join("state");
        assert_eq!(
            github_app_state_path(&db.to_string_lossy(), &state_root),
            base.join("github-app-state.json")
        );
        assert_eq!(
            github_app_state_path("", &state_root),
            state_root.join("github-app-state.json")
        );
    }

    #[test]
    fn install_request_builds_binding() {
        let request = InstallRequest {
            app_id: 7,
            installation_id: 9,
            account_id: Some(11),
            account_login: "acme".to_string(),
            repository_ids: vec![42],
            permissions: BTreeMap::from([(GitHubPermission::Contents, AccessLevel::Read)]),
            generation: 1,
        };
        let binding = request.into_binding().unwrap();
        assert_eq!(binding.app_id, 7);
        assert_eq!(binding.generation, 1);
    }

    #[test]
    fn token_request_for_forge_validates_scope() {
        let binding = binding();
        assert!(token_request_for_forge(&binding, ForgeOperation::ContentsRead, &[42]).is_ok());
        assert!(matches!(
            token_request_for_forge(&binding, ForgeOperation::ContentsRead, &[99]),
            Err(GitHubAppError::Denied(
                GitHubDenial::RepositoryOutOfScope { .. }
            ))
        ));
    }

    #[test]
    fn http_status_mapping_covers_denials() {
        assert_eq!(http_status_for_denial(&GitHubDenial::InvalidSignature), 401);
        assert_eq!(
            http_status_for_denial(&GitHubDenial::Replay {
                delivery_id: "one".to_string()
            }),
            409
        );
        assert_eq!(http_status_for_denial(&GitHubDenial::ReplayWindowFull), 429);
    }

    #[test]
    fn forge_api_base_is_stable() {
        assert_eq!(forge_api_base(Forge::GitHub), "https://api.github.com");
        assert_eq!(forge_api_base(Forge::GitLab), "https://gitlab.com/api/v4");
    }

    #[test]
    fn verify_and_reserve_webhook_uses_durable_store() {
        let backend = MemoryStateBackend::default();
        let store = GitHubAppStore::open(backend, 8, 300).unwrap();
        store.replace_binding(binding()).unwrap();
        let _backend = MemoryStateBackend::default();
        // Reuse same bytes so the second open sees the binding, but verify
        // reserves under the original store's view.
        let verifier =
            super::super::webhook::WebhookVerifier::new(7, b"sixteen-byte-key!!".to_vec()).unwrap();
        let body = br#"{"action":"opened"}"#;
        let secret = b"sixteen-byte-key!!";
        let signature = format!(
            "sha256={}",
            rightkit_crypto::hex_encode(&rightkit_crypto::hmac_sha256(secret, body))
        );
        let headers = GitHubWebhookHeaders::new("delivery-1", "issues", None, signature).unwrap();
        let identity = WebhookIdentity {
            app_id: 7,
            installation_id: Some(9),
            account_id: Some(11),
            repository_id: Some(42),
            actor_id: Some(17),
            actor_login: Some("alice".to_string()),
        };
        let verified =
            verify_and_reserve_webhook(&verifier, &store, &headers, body, identity, 100).unwrap();
        assert_eq!(verified.delivery_id, "delivery-1");
        // Duplicate should be rejected via the durable store.
        let headers2 = GitHubWebhookHeaders::new(
            "delivery-1",
            "issues",
            None,
            format!(
                "sha256={}",
                rightkit_crypto::hex_encode(&rightkit_crypto::hmac_sha256(secret, body))
            ),
        )
        .unwrap();
        let identity2 = WebhookIdentity {
            app_id: 7,
            installation_id: Some(9),
            account_id: Some(11),
            repository_id: Some(42),
            actor_id: Some(17),
            actor_login: Some("alice".to_string()),
        };
        assert!(
            verify_and_reserve_webhook(&verifier, &store, &headers2, body, identity2, 101).is_err()
        );
    }
}
