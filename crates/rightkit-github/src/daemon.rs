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
