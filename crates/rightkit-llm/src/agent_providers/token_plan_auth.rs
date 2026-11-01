//! Token-plan provider auth (ENG-88).
//!
//! A provider auth mode billed through a pre-purchased quota of tokens
//! ("token plan" / "credits pack"), distinct from a per-call API key
//! ([`crate::agent_providers::keystore::KeyStore::get_api_key`]) and from interactive OAuth.
//! [`ModelRoute::auth_mode`](crate::agent_providers::registry::ModelRoute::auth_mode) declares
//! which of the three a route uses via
//! [`crate::agent_providers::registry::AuthMode`] (added alongside this module, in
//! `registry.rs`).
//!
//! Private account proof remains external: no token-plan account is
//! provisioned in this workspace. The refresh client below uses a configured
//! provider endpoint & refresh credential, then preserves the typed
//! quota-exhausted behavior when a plan cannot cover a request.

use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
use std::time::Duration;

use crate::agent_providers::keystore::{KeyStoreBackend, KeyStoreError};

/// A token-plan credential: an identifier for the purchased plan, the
/// quota remaining on it, and the endpoint used to refresh/replenish that
/// quota. Serialized as JSON and stored as a single opaque string through
/// [`KeyStoreBackend`] (the same seam [`crate::agent_providers::keystore::KeyStore`] uses for
/// API keys), so no new storage backend is introduced for this auth mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenPlanCredential {
    /// Provider-assigned plan identifier, e.g. `"plan_9f2a"`.
    pub plan_id: String,
    /// Tokens (or provider-defined credit units) remaining on the plan.
    pub remaining_quota: u64,
    /// URL the client calls to check/replenish quota for this plan.
    pub refresh_endpoint: String,
    /// Opaque provider refresh credential. This is stored only inside the
    /// keychain-backed JSON record, never emitted in quota/tool output.
    #[serde(default)]
    pub refresh_token: Option<String>,
}

impl TokenPlanCredential {
    pub fn new(
        plan_id: impl Into<String>,
        remaining_quota: u64,
        refresh_endpoint: impl Into<String>,
    ) -> Self {
        Self {
            plan_id: plan_id.into(),
            remaining_quota,
            refresh_endpoint: refresh_endpoint.into(),
            refresh_token: None,
        }
    }

    /// Whether this plan has no quota left to spend.
    pub fn is_exhausted(&self) -> bool {
        self.remaining_quota == 0
    }

    /// Deduct `amount` from the remaining quota, returning the typed
    /// exhaustion error instead of a generic HTTP failure when the plan
    /// cannot cover the request.
    pub fn try_spend(&mut self, amount: u64) -> Result<(), TokenPlanError> {
        if amount > self.remaining_quota {
            return Err(TokenPlanError::QuotaExhausted {
                plan_id: self.plan_id.clone(),
                remaining_quota: self.remaining_quota,
                requested: amount,
            });
        }
        self.remaining_quota -= amount;
        Ok(())
    }
}

/// Result returned by a token-plan refresh endpoint. Providers commonly use
/// either `remaining_quota` or `quota`; both are accepted below so routing
/// remains explicit while provider response spelling stays local.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenPlanRefresh {
    pub remaining_quota: u64,
    pub refresh_token: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum TokenPlanRefreshError {
    #[error("token-plan refresh endpoint is not configured")]
    MissingEndpoint,
    #[error("token-plan refresh credential is not configured")]
    MissingRefreshToken,
    #[error("could not create token-plan refresh client: {0}")]
    Client(String),
    #[error("token-plan refresh request failed: {0}")]
    Request(String),
    #[error("token-plan refresh returned HTTP {status}")]
    Response { status: u16 },
    #[error("token-plan refresh returned invalid JSON")]
    InvalidResponse,
}

/// Concrete refresh client for prepaid token plans. It is deliberately not an
/// OAuth helper: a plan is identified by `plan_id` and refreshed with its
/// provider-issued refresh credential, separate from API-key/OAuth routes.
#[derive(Clone)]
pub struct TokenPlanRefreshClient {
    timeout: Duration,
}

impl TokenPlanRefreshClient {
    pub fn new() -> Result<Self, TokenPlanRefreshError> {
        Ok(Self {
            timeout: Duration::from_secs(15),
        })
    }

    /// Refresh quota in-place. A failed request leaves the existing quota and
    /// refresh credential untouched so callers can surface/retry safely.
    pub async fn refresh(
        &self,
        credential: &mut TokenPlanCredential,
    ) -> Result<TokenPlanRefresh, TokenPlanRefreshError> {
        if credential.refresh_endpoint.trim().is_empty() {
            return Err(TokenPlanRefreshError::MissingEndpoint);
        }
        let refresh_token = credential
            .refresh_token
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or(TokenPlanRefreshError::MissingRefreshToken)?;
        // The refresh endpoint is the plan's own configured host, so it alone may
        // resolve to a non-public address; redirects are never followed.
        let http = crate::agent_providers::http::provider_client_with_ceiling(
            &credential.refresh_endpoint,
            crate::agent_providers::http::Route::Ambient,
            self.timeout,
        );
        let request = rightkit_http::Request::post(credential.refresh_endpoint.clone())
            .bearer(refresh_token)
            .json(&serde_json::json!({"plan_id": credential.plan_id}))
            .map_err(|error| TokenPlanRefreshError::Request(error.to_string()))?;
        let response = http
            .send_any(&request)
            .await
            .map_err(|error| TokenPlanRefreshError::Request(error.to_string()))?;
        let status = response.status;
        let body = response.text();
        if !(200..300).contains(&status) {
            return Err(TokenPlanRefreshError::Response { status });
        }
        let value: serde_json::Value =
            serde_json::from_str(&body).map_err(|_| TokenPlanRefreshError::InvalidResponse)?;
        let remaining_quota = value
            .get("remaining_quota")
            .or_else(|| value.get("quota"))
            .and_then(serde_json::Value::as_u64)
            .ok_or(TokenPlanRefreshError::InvalidResponse)?;
        let rotated_refresh = value
            .get("refresh_token")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string);
        credential.remaining_quota = remaining_quota;
        if let Some(token) = &rotated_refresh {
            credential.refresh_token = Some(token.clone());
        }
        Ok(TokenPlanRefresh {
            remaining_quota,
            refresh_token: rotated_refresh,
        })
    }
}

/// Refresh one iOS-supplied token-plan credential through the shared bounded
/// provider client. iOS owns Keychain persistence, so this accepts only an
/// in-memory credential and returns the same instance with quota/rotation
/// applied; mobile runtime's FFI bridge must persist it only after updating
/// its active session state.
pub async fn refresh_mobile_token_plan(
    credential: &mut TokenPlanCredential,
) -> Result<TokenPlanRefresh, TokenPlanRefreshError> {
    TokenPlanRefreshClient::new()?.refresh(credential).await
}

/// Errors specific to token-plan billed auth. `QuotaExhausted` is a typed,
/// distinguishable outcome — callers must be able to branch on "this plan
/// ran out" (e.g. prompt to top up, fail over to a different `AuthMode`
/// route) without parsing an HTTP status code or provider error string.
#[derive(Debug, thiserror::Error)]
pub enum TokenPlanError {
    #[error(
        "token plan '{plan_id}' exhausted: {remaining_quota} remaining, {requested} requested"
    )]
    QuotaExhausted {
        plan_id: String,
        remaining_quota: u64,
        requested: u64,
    },

    #[error("token-plan credential store error: {0}")]
    Store(#[from] KeyStoreError),

    #[error("stored token-plan credential is not valid JSON: {0}")]
    Deserialize(String),
}

/// Key-store service namespace token-plan credentials are stored under,
/// separate from [`crate::agent_providers::keystore::KeyStore::default_service`]'s
/// `"host app"` API-key service so a plan credential's JSON blob can never
/// collide with (or be misread as) a plain API-key string under the same
/// key name.
pub const TOKEN_PLAN_SERVICE: &str = "rightkit-agent-token-plan";

/// Store/load a [`TokenPlanCredential`] through any [`KeyStoreBackend`] —
/// the real OS keychain ([`crate::agent_providers::keystore::OsKeyStore`]) or the in-memory
/// test fake ([`crate::agent_providers::keystore::InMemoryKeyStore`]) — the same seam
/// provider API keys already use.
pub struct TokenPlanStore;

impl TokenPlanStore {
    /// Persist `credential` under `provider_slug`, replacing any existing
    /// entry.
    pub fn save(
        backend: &dyn KeyStoreBackend,
        provider_slug: &str,
        credential: &TokenPlanCredential,
    ) -> Result<(), TokenPlanError> {
        let json = serde_json::to_string(credential)
            .map_err(|e| TokenPlanError::Deserialize(e.to_string()))?;
        backend
            .set(TOKEN_PLAN_SERVICE, provider_slug, &json)
            .map_err(TokenPlanError::Store)
    }

    /// Load the stored credential for `provider_slug`.
    pub fn load(
        backend: &dyn KeyStoreBackend,
        provider_slug: &str,
    ) -> Result<TokenPlanCredential, TokenPlanError> {
        let json = backend
            .get(TOKEN_PLAN_SERVICE, provider_slug)
            .map_err(TokenPlanError::Store)?;
        serde_json::from_str(&json).map_err(|e| TokenPlanError::Deserialize(e.to_string()))
    }
}

/// One daemon request reserves one provider-defined plan unit before text
/// transport begins. This is intentionally separate from API-key/OAuth route
/// lookup: callers opt into it only for a provider whose stored token-plan
/// record exists. A missing record leaves that provider on its normal route.
pub const DAEMON_TEXT_REQUEST_QUOTA: u64 = 1;

/// Result of resolving a daemon text route. `ApiKeyOrOAuth` is not a failure:
/// it means no token-plan record was selected for this provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonTextAuthSelection {
    ApiKeyOrOAuth,
    TokenPlan,
}

#[derive(Debug, thiserror::Error)]
pub enum DaemonTextTokenPlanError {
    #[error(transparent)]
    Plan(#[from] TokenPlanError),
    #[error(transparent)]
    Refresh(#[from] TokenPlanRefreshError),
}

// Keychain read/refresh/save must remain one critical section so concurrent
// daemon turns cannot both spend a final plan unit. The refresh client has a
// bounded timeout; this lock is only held while a selected plan is reconciled.
static DAEMON_TEXT_PLAN_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

/// Select & reserve a prepaid plan for one daemon text request when one is
/// stored under `provider_slug`. No credential is ever returned or logged.
///
/// At zero quota this refreshes once before spending. If refresh still cannot
/// cover a request, the caller receives [`TokenPlanError::QuotaExhausted`]
/// rather than issuing a plausible provider request.
#[cfg(feature = "agent_providers-os-keyring")]
pub async fn reserve_daemon_text_quota(
    provider_slug: &str,
) -> Result<DaemonTextAuthSelection, DaemonTextTokenPlanError> {
    let _guard = DAEMON_TEXT_PLAN_LOCK.lock().await;
    let backend = crate::agent_providers::keystore::OsKeyStore;
    let mut credential = match TokenPlanStore::load(&backend, provider_slug) {
        Ok(credential) => credential,
        Err(TokenPlanError::Store(KeyStoreError::NotFound { .. })) => {
            return Ok(DaemonTextAuthSelection::ApiKeyOrOAuth)
        }
        Err(error) => return Err(error.into()),
    };

    if credential.remaining_quota < DAEMON_TEXT_REQUEST_QUOTA {
        TokenPlanRefreshClient::new()?
            .refresh(&mut credential)
            .await?;
    }
    credential.try_spend(DAEMON_TEXT_REQUEST_QUOTA)?;
    TokenPlanStore::save(&backend, provider_slug, &credential)?;
    Ok(DaemonTextAuthSelection::TokenPlan)
}

/// Targets without an OS keychain cannot persist a token-plan credential, so
/// they retain their explicitly supplied API-key/OAuth route.
#[cfg(not(feature = "agent_providers-os-keyring"))]
pub async fn reserve_daemon_text_quota(
    _provider_slug: &str,
) -> Result<DaemonTextAuthSelection, DaemonTextTokenPlanError> {
    Ok(DaemonTextAuthSelection::ApiKeyOrOAuth)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_providers::keystore::InMemoryKeyStore;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn sample_credential() -> TokenPlanCredential {
        TokenPlanCredential::new("plan_9f2a", 1000, "https://provider.example/plan/refresh")
    }

    #[test]
    fn try_spend_deducts_quota_on_success() {
        let mut cred = sample_credential();
        cred.try_spend(400).expect("plan has quota");
        assert_eq!(cred.remaining_quota, 600);
    }

    #[test]
    fn try_spend_returns_typed_quota_exhausted_error_not_generic_failure() {
        let mut cred = sample_credential();
        let err = cred
            .try_spend(1_500)
            .expect_err("must fail: not enough quota");
        match err {
            TokenPlanError::QuotaExhausted {
                plan_id,
                remaining_quota,
                requested,
            } => {
                assert_eq!(plan_id, "plan_9f2a");
                assert_eq!(remaining_quota, 1000);
                assert_eq!(requested, 1_500);
            }
            other => panic!("expected QuotaExhausted, got: {other}"),
        }
        // Quota must be unchanged after a failed spend.
        assert_eq!(cred.remaining_quota, 1000);
    }

    #[test]
    fn is_exhausted_true_only_at_zero_quota() {
        let mut cred = sample_credential();
        assert!(!cred.is_exhausted());
        cred.try_spend(1000).unwrap();
        assert!(cred.is_exhausted());
    }

    #[test]
    fn token_plan_store_roundtrips_through_in_memory_backend() {
        let backend = InMemoryKeyStore::new();
        let cred = sample_credential();
        TokenPlanStore::save(&backend, "openai-images", &cred).expect("save");

        let loaded = TokenPlanStore::load(&backend, "openai-images").expect("load");
        assert_eq!(loaded, cred);
    }

    #[test]
    fn token_plan_store_missing_entry_surfaces_store_error() {
        let backend = InMemoryKeyStore::new();
        let err = TokenPlanStore::load(&backend, "nonexistent").unwrap_err();
        match err {
            TokenPlanError::Store(KeyStoreError::NotFound { .. }) => {}
            other => panic!("expected Store(NotFound), got: {other}"),
        }
    }

    #[test]
    fn token_plan_service_namespace_is_distinct_from_api_key_service() {
        assert_ne!(
            TOKEN_PLAN_SERVICE,
            crate::agent_providers::keystore::KeyStore::default_service()
        );
    }

    #[tokio::test]
    async fn live_refresh_updates_quota_and_rotates_refresh_credential() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            let header_end = loop {
                let length = stream.read(&mut chunk).await.unwrap();
                request.extend_from_slice(&chunk[..length]);
                if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.strip_prefix("content-length: ")
                        .or_else(|| line.strip_prefix("Content-Length: "))
                })
                .unwrap()
                .parse::<usize>()
                .unwrap();
            while request.len() < header_end + content_length {
                let length = stream.read(&mut chunk).await.unwrap();
                request.extend_from_slice(&chunk[..length]);
            }
            let request = String::from_utf8_lossy(&request);
            assert!(request.starts_with("POST / HTTP/1.1"), "{request}");
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer refresh-before"),
                "{request}"
            );
            assert!(request.contains("{\"plan_id\":\"plan_9f2a\"}"), "{request}");
            let body = r#"{"remaining_quota":2400,"refresh_token":"refresh-after"}"#;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(), body
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let mut credential = TokenPlanCredential::new("plan_9f2a", 0, endpoint);
        credential.refresh_token = Some("refresh-before".into());

        let refreshed = refresh_mobile_token_plan(&mut credential).await.unwrap();
        server.await.unwrap();

        assert_eq!(refreshed.remaining_quota, 2400);
        assert_eq!(refreshed.refresh_token.as_deref(), Some("refresh-after"));
        assert_eq!(credential.remaining_quota, 2400);
        assert_eq!(credential.refresh_token.as_deref(), Some("refresh-after"));
    }
}
