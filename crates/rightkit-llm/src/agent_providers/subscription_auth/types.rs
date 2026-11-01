//! Donor: Pi `packages/ai/src/auth/types.ts` (port) plus the shared error
//! type from `packages/ai/src/auth/resolve.ts` (`ModelsError` → [`AuthError`]).
//!
//! Typed auth shapes shared by the broker and the login flows. Stored OAuth
//! tokens, their refresh and the cross-process refresh lock are not modelled
//! here: they are `rightkit_oauth::TokenSet` and
//! `rightkit_oauth::credentials::Resolver` (see `token_store`). Every
//! credential-bearing type here has a redacting `Debug` impl so an accidental
//! `{:?}` can never become the leak it must not be.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Boxing alias for the async seam. The donor is async end to end because
/// JavaScript I/O is; the host app keeps the async seam only where it is real
/// (network refresh, command execution, file locks) and uses the same boxed
/// future style as the rest of this crate (boxed `Pin<Box<dyn Future + Send>>` seams).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Placeholder printed wherever credential material would appear in
/// non-credential-bearing output. Same vocabulary as `keystore::REDACTED`.
pub const REDACTED: &str = "[redacted]";

/// Provider-scoped environment/config values (donor `ProviderEnv`).
pub type ProviderEnv = BTreeMap<String, String>;

/// Milliseconds since the Unix epoch. The donor uses `Date.now()`; tests
/// construct credentials relative to this.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// Request auth for a single model request (donor `ModelAuth`). If a value
/// cannot be expressed as `api_key`, `headers`, or `base_url`, it is provider
/// config, not auth.
///
/// This is the one credential-bearing operational shape in this module: it
/// exists to be consumed by a provider transport on the same call, never
/// persisted, logged or serialized into records. `Debug` redacts every field
/// that can carry bytes.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ModelAuth {
    pub api_key: Option<String>,
    pub headers: Option<BTreeMap<String, String>>,
    pub base_url: Option<String>,
}

impl std::fmt::Debug for ModelAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelAuth")
            .field("api_key", &self.api_key.as_ref().map(|_| REDACTED))
            .field(
                "headers",
                &self
                    .headers
                    .as_ref()
                    .map(|headers| headers.keys().cloned().collect::<Vec<_>>()),
            )
            .field("base_url", &self.base_url)
            .finish()
    }
}

impl ModelAuth {
    pub fn api_key(key: impl Into<String>) -> Self {
        Self {
            api_key: Some(key.into()),
            headers: None,
            base_url: None,
        }
    }

    pub fn bearer_header(token: impl Into<String>) -> Self {
        Self {
            api_key: None,
            headers: Some(BTreeMap::from([(
                "Authorization".to_string(),
                format!("Bearer {}", token.into()),
            )])),
            base_url: None,
        }
    }
}

/// Stored api-key credential (donor `ApiKeyCredential`). `env` holds
/// provider-scoped environment/config values such as Cloudflare
/// account/gateway ids. `key` may hold a literal key **or** a config-value
/// reference (`$VAR` template or `!command`); resolution is the store's job,
/// mirroring the donor `AuthStorage.read` contract.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ApiKeyCredential {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<ProviderEnv>,
}

impl std::fmt::Debug for ApiKeyCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKeyCredential")
            .field("key", &self.key.as_ref().map(|_| REDACTED))
            .field("env", &self.env)
            .finish()
    }
}

/// Errors from auth resolution and credential storage (donor `ModelsError`
/// with its `ModelsErrorCode` discriminator, plus the the host app seam codes
/// for the eligibility gate, cancellation and storage failures).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct AuthError {
    pub code: AuthErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthErrorCode {
    ModelSource,
    ModelValidation,
    Provider,
    Stream,
    Auth,
    OAuth,
    /// the host app seam: the caller aborted the operation.
    Cancelled,
    /// the host app seam: the tri-state eligibility gate refused subscription
    /// use (`Unsupported`/`Unknown` are never enabled in core).
    UnsupportedSubscription,
    /// the host app seam: the provider's credentials are owned by a foreign
    /// harness CLI and stay vendor-owned (PL-002 boundary).
    ForeignVendorOwned,
    /// the host app seam: the credential store itself failed (read/lock/write).
    Storage,
}

impl AuthErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            AuthErrorCode::ModelSource => "model_source",
            AuthErrorCode::ModelValidation => "model_validation",
            AuthErrorCode::Provider => "provider",
            AuthErrorCode::Stream => "stream",
            AuthErrorCode::Auth => "auth",
            AuthErrorCode::OAuth => "oauth",
            AuthErrorCode::Cancelled => "cancelled",
            AuthErrorCode::UnsupportedSubscription => "unsupported_subscription",
            AuthErrorCode::ForeignVendorOwned => "foreign_vendor_owned",
            AuthErrorCode::Storage => "storage",
        }
    }
}

impl AuthError {
    pub fn new(code: AuthErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn auth(message: impl Into<String>) -> Self {
        Self::new(AuthErrorCode::Auth, message)
    }

    pub fn oauth(message: impl Into<String>) -> Self {
        Self::new(AuthErrorCode::OAuth, message)
    }

    pub fn storage(message: impl Into<String>) -> Self {
        Self::new(AuthErrorCode::Storage, message)
    }

    pub fn cancelled() -> Self {
        Self::new(AuthErrorCode::Cancelled, "operation cancelled")
    }

    /// Donor `withCauseDetail`: callers surface `error.message` only, so keep
    /// the underlying reason in it.
    pub fn with_context(mut self, cause: &AuthError) -> Self {
        let detail = cause.message.trim();
        if detail.is_empty() || self.message.contains(detail) {
            return self;
        }
        self.message = format!("{}: {}", self.message, detail);
        self
    }
}

/// Cooperative cancellation handle standing in for the donor's `AbortSignal`.
/// Queued store operations check it before running; committed writes check it
/// again before persisting, so a cancelled mutation holds the lock until it
/// settles but never commits (the donor's held-lock-cancel contract).
#[derive(Clone, Debug, Default)]
pub struct AuthSignal(Arc<AtomicBool>);

impl AuthSignal {
    pub fn abort(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_aborted(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    pub fn check(&self) -> Result<(), AuthError> {
        if self.is_aborted() {
            Err(AuthError::cancelled())
        } else {
            Ok(())
        }
    }
}

/// Environment access for auth resolution (donor `AuthContext`), injectable
/// for tests.
pub trait AuthContext: Send + Sync {
    fn env<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Option<String>>;
    /// Whether a file exists. Supports a leading `~`.
    fn file_exists<'a>(&'a self, path: &'a str) -> BoxFuture<'a, bool>;
}

/// Default auth context (donor `defaultProviderAuthContext`): environment
/// variables from the process, file existence on the real filesystem.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessAuthContext;

impl AuthContext for ProcessAuthContext {
    fn env<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            // The process environment supplies auth values only in `host-env`
            // and test builds; the mobile runtime resolves none from it.
            #[cfg(any(test, feature = "agent_providers-host-env"))]
            {
                let value = std::env::var(name).ok()?;
                // Donor keeps the raw value when it has non-whitespace content.
                (!value.trim().is_empty()).then_some(value)
            }
            #[cfg(not(any(test, feature = "agent_providers-host-env")))]
            {
                let _ = name;
                None
            }
        })
    }

    fn file_exists<'a>(&'a self, path: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async move { std::path::Path::new(&expand_tilde(path)).exists() })
    }
}

/// Expand a leading `~` to the user's home directory (donor `fileExists`
/// tilde handling).
pub fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix('~') {
        if let Some(home) = dirs::home_dir() {
            return format!("{}{}", home.display(), rest);
        }
    }
    path.to_string()
}

/// In-memory [`AuthContext`] fake for tests (env map, no filesystem).
#[derive(Debug, Default, Clone)]
pub struct EnvMapAuthContext {
    pub env: BTreeMap<String, String>,
}

impl EnvMapAuthContext {
    pub fn new(entries: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>) -> Self {
        Self {
            env: entries
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        }
    }
}

impl AuthContext for EnvMapAuthContext {
    fn env<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Option<String>> {
        let value = self.env.get(name).cloned();
        Box::pin(async move { value })
    }

    fn file_exists<'a>(&'a self, _path: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async move { false })
    }
}

/// Result of resolving auth for a model (donor `AuthResult`).
#[derive(Clone, PartialEq)]
pub struct AuthResult {
    pub auth: ModelAuth,
    /// Provider-scoped environment/config values resolved from credentials
    /// and ambient context.
    pub env: Option<ProviderEnv>,
    /// Human-readable label for status UI: "ANTHROPIC_API_KEY", "OAuth",
    /// "stored credential", "keychain".
    pub source: Option<String>,
}

impl std::fmt::Debug for AuthResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthResult")
            .field("auth", &self.auth)
            .field("env", &self.env)
            .field("source", &self.source)
            .finish()
    }
}

/// Prompt shown to the user during login (donor `AuthPrompt`). Cancellation
/// of a pending prompt uses the interaction's signal (the donor's per-prompt
/// `signal` field is folded into the interaction to keep the seam small).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthPrompt {
    Text {
        message: String,
        placeholder: Option<String>,
    },
    Secret {
        message: String,
        placeholder: Option<String>,
    },
    Select {
        message: String,
        options: Vec<(String, String)>,
    },
    ManualCode {
        message: String,
        placeholder: Option<String>,
    },
}

/// An informational link surfaced with an [`AuthEvent::Info`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthInfoLink {
    pub url: String,
    pub label: Option<String>,
}

/// Events emitted during login (donor `AuthEvent`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthEvent {
    Info {
        message: String,
        links: Vec<AuthInfoLink>,
    },
    AuthUrl {
        url: String,
        instructions: Option<String>,
    },
    DeviceCode {
        user_code: String,
        verification_uri: String,
        interval_seconds: Option<u64>,
        expires_in_seconds: Option<u64>,
    },
    Progress {
        message: String,
    },
}

/// Login interaction callbacks serving both api-key and OAuth flows (donor
/// `AuthInteraction`). `prompt` resolves with the entered/selected string and
/// errors on cancel/abort; `notify` surfaces events to the UI.
pub trait AuthInteraction: Send + Sync {
    /// Signal for the whole login flow; aborting it cancels the flow.
    fn signal(&self) -> AuthSignal;
    fn prompt<'a>(&'a self, prompt: AuthPrompt) -> BoxFuture<'a, Result<String, AuthError>>;
    fn notify(&self, event: AuthEvent);
}

/// Api-key auth (donor `ApiKeyAuth`): a stored key/provider env plus ambient
/// sources. Ambient-only providers omit login (login orchestration is
/// app-owned).
pub trait ApiKeyAuth: Send + Sync {
    /// Display name, e.g. "Anthropic API key".
    fn name(&self) -> &str;

    /// Provider env var names this auth can resolve from, for status
    /// enumeration. Must never be executed — status listing resolves names
    /// only (donor `CredentialStore::list` contract).
    fn env_vars(&self) -> Vec<String> {
        Vec::new()
    }

    /// Resolve auth from the stored credential and/or ambient sources,
    /// merging per field (`credential.key ?? env("...")`). `None` = not
    /// configured. Resolution is provider-scoped; model-specific endpoint
    /// preparation happens after auth has been resolved.
    fn resolve<'a>(
        &'a self,
        ctx: &'a dyn AuthContext,
        credential: Option<&'a ApiKeyCredential>,
        signal: AuthSignal,
    ) -> BoxFuture<'a, Result<Option<AuthResult>, AuthError>>;
}

/// SHA-256 hex prefix (16 chars) over stable account material — the account
/// fingerprint contract: identifies the account without carrying credential
/// bytes. Callers hash non-secret account material (a provider account id)
/// or, where none exists, the refresh token (stable per login, never echoed).
pub fn account_fingerprint(material: &str) -> String {
    let digest = rightkit_crypto::sha256(material.as_bytes());
    rightkit_crypto::hex_encode(&digest)[..16].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "sk-ant-super-secret-bytes";

    #[test]
    fn model_auth_debug_never_contains_credential_bytes() {
        let auth = ModelAuth::api_key(SECRET);
        let rendered = format!("{auth:?}");
        assert!(!rendered.contains(SECRET), "{rendered}");
        assert!(rendered.contains(REDACTED));

        let bearer = ModelAuth::bearer_header(SECRET);
        let rendered = format!("{bearer:?}");
        assert!(!rendered.contains(SECRET), "{rendered}");
        assert!(rendered.contains("Authorization"));
    }

    #[test]
    fn auth_result_debug_never_contains_credential_bytes() {
        let result = AuthResult {
            auth: ModelAuth::api_key(SECRET),
            env: None,
            source: Some("stored credential".to_string()),
        };
        let rendered = format!("{result:?}");
        assert!(!rendered.contains(SECRET), "{rendered}");
    }

    #[test]
    fn account_fingerprint_is_stable_and_never_the_material() {
        let a = account_fingerprint("account-material-1");
        let b = account_fingerprint("account-material-1");
        let c = account_fingerprint("account-material-2");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 16);
        assert!(!a.contains("account"));
    }

    #[test]
    fn expand_tilde_maps_home_prefix() {
        assert_eq!(expand_tilde("/plain/path"), "/plain/path");
        if dirs::home_dir().is_some() {
            let expanded = expand_tilde("~/file");
            assert!(expanded.contains("/file") || expanded.contains("\\file"));
            assert!(!expanded.starts_with('~'));
        }
    }
}
