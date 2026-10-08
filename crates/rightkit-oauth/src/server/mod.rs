//! Confidential-client authorization for daemons. Blocking I/O; use a blocking executor
//! in async routes. No listener, browser launch, token logging or automatic retry.
mod provider;
pub(crate) mod sensitive;
mod store;
pub use provider::{ClientIdParameter, ServerProvider};
pub use store::{SealedFileTokenStore, TokenStore, TokenStoreGuard};

use crate::TokenSet;
use base64::Engine as _;
use rightkit_http::{percent_encode, Client, Request, RetryPolicy, Secret};
use sensitive::{SensitiveJson, SensitiveRequest, SensitiveResponse};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, Zeroizing};

/// Errors carry no callback/provider text or HTTP bodies, even when providers echo secrets.
#[non_exhaustive]
pub enum ServerAuthError {
    InvalidConfig,
    InvalidCallback,
    StateMismatch,
    StateExpired,
    PendingCapacity,
    ProviderError,
    ExchangeFailure {
        status: Option<u16>,
    },
    ReauthRequired,
    Storage,
    OidcValidation,
    /// Rotation succeeded but persistence failed. Recover these tokens through secure
    /// storage; do not spend the old refresh token again. Debug redacts all fields.
    /// Boxed to keep this error small; dereference to recover an owned `TokenSet`.
    RefreshPersistence {
        tokens: Box<TokenSet>,
    },
}
impl fmt::Debug for ServerAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExchangeFailure { status } => f
                .debug_struct("ExchangeFailure")
                .field("status", status)
                .finish(),
            Self::RefreshPersistence { .. } => {
                f.write_str("RefreshPersistence { tokens: [redacted] }")
            }
            other => fmt::Display::fmt(other, f),
        }
    }
}
impl fmt::Display for ServerAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidConfig => "invalid server OAuth configuration",
            Self::InvalidCallback => "invalid OAuth callback",
            Self::StateMismatch => "OAuth state mismatch or already consumed",
            Self::StateExpired => "OAuth state expired",
            Self::PendingCapacity => "pending OAuth capacity reached",
            Self::ProviderError => "OAuth provider declined authorization",
            Self::ExchangeFailure { .. } => "OAuth token exchange failed",
            Self::ReauthRequired => "OAuth reauthorization required",
            Self::Storage => "OAuth storage failed",
            Self::OidcValidation => "OIDC validation failed or validator missing",
            Self::RefreshPersistence { .. } => "rotated OAuth tokens require persistence recovery",
        })
    }
}
impl std::error::Error for ServerAuthError {}

#[derive(Clone, Copy, Debug)]
pub enum ClientAuth {
    None,
    SecretPost,
    SecretBasic,
}

/// Epoch clock for portable durable TTLs; independent of the desktop device-flow clock.
pub trait ServerClock: Send + Sync {
    fn now(&self) -> SystemTime;
}
pub struct SystemServerClock;
impl ServerClock for SystemServerClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// Durable adapters may serialize this record. Encrypt verifier/nonce/state at rest.
/// `account_hint` is host-selected ownership context, not a provider-verified identity.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct PendingAuthorization {
    pub state: String,
    pub provider: String,
    pub account_hint: String,
    pub expires_at: SystemTime,
    pub verifier: Option<String>,
    pub nonce: Option<String>,
    /// Prevents a pending request being completed under changed provider policy.
    pub binding: String,
}
impl Drop for PendingAuthorization {
    fn drop(&mut self) {
        self.state.zeroize();
        self.verifier.zeroize();
        self.nonce.zeroize();
    }
}
impl fmt::Debug for PendingAuthorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PendingAuthorization { [redacted] }")
    }
}

/// Implement insert-if-absent and atomic destructive take across all daemon instances.
/// Keys are SHA-256(state), not bearer state values. Purge expired records periodically;
/// retain them until purge if callers need `StateExpired` rather than `StateMismatch`.
/// Never log records or errors containing their values. Bound capacity and TTL.
pub trait PendingStateStore: Send + Sync {
    fn insert(
        &self,
        key: &str,
        pending: PendingAuthorization,
        now: SystemTime,
    ) -> Result<(), ServerAuthError>;
    fn take(&self, key: &str) -> Result<Option<PendingAuthorization>, ServerAuthError>;
}

pub struct MemoryPendingStateStore {
    entries: Mutex<HashMap<String, PendingAuthorization>>,
    capacity: usize,
}
impl Default for MemoryPendingStateStore {
    fn default() -> Self {
        Self::new(1024)
    }
}
impl MemoryPendingStateStore {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            capacity,
        }
    }
}
impl PendingStateStore for MemoryPendingStateStore {
    fn insert(
        &self,
        key: &str,
        pending: PendingAuthorization,
        now: SystemTime,
    ) -> Result<(), ServerAuthError> {
        let mut entries = self.entries.lock().map_err(|_| ServerAuthError::Storage)?;
        entries.retain(|_, p| p.expires_at > now);
        if entries.len() >= self.capacity {
            return Err(ServerAuthError::PendingCapacity);
        }
        if entries.contains_key(key) {
            return Err(ServerAuthError::StateMismatch);
        }
        entries.insert(key.into(), pending);
        Ok(())
    }
    fn take(&self, key: &str) -> Result<Option<PendingAuthorization>, ServerAuthError> {
        Ok(self
            .entries
            .lock()
            .map_err(|_| ServerAuthError::Storage)?
            .remove(key))
    }
}

pub struct ServerAuthorization {
    pub authorize_url: String,
    pub state: String,
}
impl Drop for ServerAuthorization {
    fn drop(&mut self) {
        self.authorize_url.zeroize();
        self.state.zeroize();
    }
}
impl fmt::Debug for ServerAuthorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ServerAuthorization { [redacted] }")
    }
}

#[derive(Default, serde::Deserialize)]
pub struct CallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
}
impl Drop for CallbackQuery {
    fn drop(&mut self) {
        self.code.zeroize();
        self.state.zeroize();
        self.error.zeroize();
    }
}
impl fmt::Debug for CallbackQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CallbackQuery { [redacted] }")
    }
}

pub struct CompletedAuthorization {
    pub provider: String,
    pub account_hint: String,
    pub tokens: TokenSet,
}
impl fmt::Debug for CompletedAuthorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CompletedAuthorization { [redacted] }")
    }
}

/// Verify ID-token signature, issuer, audience/azp, expiry and nonce using provider JWKS.
/// Compare nonce in constant time. Host owns OIDC discovery/key caching.
/// Returning Ok authorizes token acceptance.
pub trait OidcValidator: Send + Sync {
    fn validate(
        &self,
        provider: &ServerProvider,
        id_token: &Secret,
        expected_nonce: &str,
    ) -> Result<(), ServerAuthError>;
}

pub struct ServerAuthFlow {
    http: Client,
    providers: HashMap<String, ServerProvider>,
    pending: Arc<dyn PendingStateStore>,
    ttl: Duration,
    clock: Arc<dyn ServerClock>,
    oidc: Option<Arc<dyn OidcValidator>>,
}
impl ServerAuthFlow {
    pub fn new(
        http: Client,
        providers: impl IntoIterator<Item = ServerProvider>,
        pending: Arc<dyn PendingStateStore>,
        ttl: Duration,
    ) -> Result<Self, ServerAuthError> {
        // Redirects can replay a secret_post body or spend a rotating grant elsewhere.
        if ttl.is_zero()
            || ttl > Duration::from_secs(3600)
            || http.config().max_redirects != 0
            || http.config().request_timeout.is_zero()
            || http.config().connect_timeout.is_none_or(|t| t.is_zero())
        {
            return Err(ServerAuthError::InvalidConfig);
        }
        let mut map = HashMap::new();
        for p in providers {
            p.validate()?;
            if map.insert(p.id.clone(), p).is_some() {
                return Err(ServerAuthError::InvalidConfig);
            }
        }
        if map.is_empty() {
            return Err(ServerAuthError::InvalidConfig);
        }
        Ok(Self {
            http,
            providers: map,
            pending,
            ttl,
            clock: Arc::new(SystemServerClock),
            oidc: None,
        })
    }
    pub fn with_clock(mut self, clock: Arc<dyn ServerClock>) -> Self {
        self.clock = clock;
        self
    }
    pub fn with_oidc_validator(mut self, validator: Arc<dyn OidcValidator>) -> Self {
        self.oidc = Some(validator);
        self
    }

    /// Scopes supplied here replace descriptor defaults; empty uses config.scopes.
    /// Only an authenticated host user may begin; bind account_hint to that user's account.
    pub fn begin(
        &self,
        provider: &str,
        account_hint: &str,
        scopes: &[String],
    ) -> Result<ServerAuthorization, ServerAuthError> {
        let p = self.provider(provider)?;
        if account_hint.trim().is_empty() {
            return Err(ServerAuthError::InvalidConfig);
        }
        let scopes = if scopes.is_empty() {
            &p.config.scopes
        } else {
            scopes
        };
        if scopes
            .iter()
            .any(|s| s.is_empty() || s.chars().any(|c| c.is_whitespace() || c == ','))
        {
            return Err(ServerAuthError::InvalidConfig);
        }
        let oidc = scopes.iter().any(|s| s == "openid");
        if oidc && (!p.oidc || self.oidc.is_none()) {
            return Err(ServerAuthError::OidcValidation);
        }
        let state = Zeroizing::new(random(32)?);
        let mut verifier = Zeroizing::new(if p.pkce { Some(random(48)?) } else { None });
        let mut nonce = Zeroizing::new(if oidc { Some(random(32)?) } else { None });
        let now = self.clock.now();
        let expires_at = now
            .checked_add(self.ttl)
            .ok_or(ServerAuthError::InvalidConfig)?;
        let mut pairs = vec![
            ("response_type".into(), "code".into()),
            (
                p.client_id_parameter.name().into(),
                p.config.client_id.clone(),
            ),
            ("redirect_uri".into(), p.redirect_uri.clone()),
            ("state".into(), state.to_string()),
        ];
        if !scopes.is_empty() {
            pairs.push(("scope".into(), scopes.join(&p.scope_separator.to_string())));
        }
        if let Some(v) = &*verifier {
            pairs.push((
                "code_challenge".into(),
                crate::base64url(&Sha256::digest(v.as_bytes())),
            ));
            pairs.push(("code_challenge_method".into(), "S256".into()));
        }
        if let Some(n) = &*nonce {
            pairs.push(("nonce".into(), n.clone()));
        }
        pairs.extend(p.config.authorization_params.iter().cloned());
        let mut query = Zeroizing::new(String::new());
        for (k, v) in &pairs {
            if !query.is_empty() {
                query.push('&');
            }
            let encoded = Zeroizing::new(percent_encode(v));
            query.push_str(&percent_encode(k));
            query.push('=');
            query.push_str(&encoded);
        }
        let mut authorize_url =
            Zeroizing::new(format!("{}?{}", p.config.authorization_url, *query));
        for (k, v) in &mut pairs {
            k.zeroize();
            v.zeroize();
        }
        self.pending.insert(
            &digest(&state),
            PendingAuthorization {
                state: state.to_string(),
                provider: p.id.clone(),
                account_hint: account_hint.into(),
                expires_at,
                verifier: verifier.take(),
                nonce: nonce.take(),
                binding: binding(p),
            },
            now,
        )?;
        Ok(ServerAuthorization {
            authorize_url: std::mem::take(&mut *authorize_url),
            state: state.to_string(),
        })
    }

    /// Atomically consumes state before handling denial or spending a code. Every valid
    /// callback is single-use, including malformed callbacks and failed exchanges.
    /// Host MUST verify browser-session correlation and expected provider before calling.
    /// Prefer `complete_bound` when this flow should enforce account/route binding itself.
    pub fn complete(
        &self,
        query: CallbackQuery,
    ) -> Result<CompletedAuthorization, ServerAuthError> {
        self.complete_inner(query, None)
    }

    /// Bind completion to an authenticated browser account AND provider-specific route.
    /// Host must derive both expectations independently of callback query parameters.
    pub fn complete_bound(
        &self,
        query: CallbackQuery,
        provider: &str,
        account_hint: &str,
    ) -> Result<CompletedAuthorization, ServerAuthError> {
        self.complete_inner(query, Some((provider, account_hint)))
    }

    fn complete_inner(
        &self,
        query: CallbackQuery,
        expected: Option<(&str, &str)>,
    ) -> Result<CompletedAuthorization, ServerAuthError> {
        let state = query
            .state
            .as_deref()
            .filter(|s| s.len() == 43)
            .ok_or(ServerAuthError::StateMismatch)?;
        let mut pending = self
            .pending
            .take(&digest(state))?
            .ok_or(ServerAuthError::StateMismatch)?;
        if !bool::from(pending.state.as_bytes().ct_eq(state.as_bytes())) {
            return Err(ServerAuthError::StateMismatch);
        }
        if self.clock.now() >= pending.expires_at {
            return Err(ServerAuthError::StateExpired);
        }
        if let Some((provider, account)) = expected {
            if !bool::from(pending.provider.as_bytes().ct_eq(provider.as_bytes()))
                || !bool::from(pending.account_hint.as_bytes().ct_eq(account.as_bytes()))
            {
                return Err(ServerAuthError::StateMismatch);
            }
        }
        let p = self.provider(&pending.provider)?;
        if !bool::from(pending.binding.as_bytes().ct_eq(binding(p).as_bytes())) {
            return Err(ServerAuthError::StateMismatch);
        }
        if query.error.is_some() {
            return Err(ServerAuthError::ProviderError);
        }
        let code = query
            .code
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or(ServerAuthError::InvalidCallback)?;
        let mut pairs = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", p.redirect_uri.as_str()),
        ];
        if let Some(verifier) = &pending.verifier {
            pairs.push(("code_verifier", verifier));
        }
        let (tokens, value) = self.token_request(p, &pairs, None, false)?;
        if let Some(nonce) = &pending.nonce {
            let id_token = value
                .get("id_token")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .ok_or(ServerAuthError::OidcValidation)?;
            self.oidc
                .as_ref()
                .ok_or(ServerAuthError::OidcValidation)?
                .validate(p, &Secret::new(id_token), nonce)
                .map_err(|_| ServerAuthError::OidcValidation)?;
        }
        Ok(CompletedAuthorization {
            provider: std::mem::take(&mut pending.provider),
            account_hint: std::mem::take(&mut pending.account_hint),
            tokens,
        })
    }

    /// Host schedules this before expiry; no internal timer. Lock covers read, network
    /// refresh and atomic replacement, including other processes using this store.
    pub fn refresh(
        &self,
        store: &dyn TokenStore,
        account: &str,
        provider: &str,
        skew: Duration,
    ) -> Result<TokenSet, ServerAuthError> {
        let p = self.provider(provider)?;
        let mut guard = store.lock(account, provider)?;
        let current = guard.get()?.ok_or(ServerAuthError::ReauthRequired)?;
        let now = self.clock.now();
        let due = current
            .expires_in
            .map(|d| {
                current
                    .obtained_at
                    .checked_add(d)
                    .and_then(|t| t.duration_since(now).ok())
                    .map(|left| left <= skew)
                    .unwrap_or(true)
            })
            .unwrap_or(false);
        if !due {
            return Ok(current);
        }
        let refresh = current
            .refresh_token
            .as_ref()
            .ok_or(ServerAuthError::ReauthRequired)?;
        let result = self.token_request(
            p,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh.expose()),
            ],
            Some(refresh),
            true,
        );
        match result {
            Ok((tokens, _)) => {
                if guard.put(&tokens).is_err() {
                    return Err(ServerAuthError::RefreshPersistence {
                        tokens: Box::new(tokens),
                    });
                }
                Ok(tokens)
            }
            Err(ServerAuthError::ReauthRequired) => {
                guard.delete()?;
                Err(ServerAuthError::ReauthRequired)
            }
            Err(e) => Err(e),
        }
    }

    fn provider(&self, id: &str) -> Result<&ServerProvider, ServerAuthError> {
        self.providers.get(id).ok_or(ServerAuthError::InvalidConfig)
    }

    fn token_request(
        &self,
        p: &ServerProvider,
        pairs: &[(&str, &str)],
        old_refresh: Option<&Secret>,
        refreshing: bool,
    ) -> Result<(TokenSet, SensitiveJson), ServerAuthError> {
        let mut pairs = pairs.to_vec();
        let mut request = Request::post(&p.config.token_url)
            .header("Accept", "application/json")
            .retry(RetryPolicy::none());
        match p.client_auth {
            ClientAuth::SecretBasic => {
                let secret = p
                    .config
                    .client_secret
                    .as_ref()
                    .ok_or(ServerAuthError::InvalidConfig)?;
                let encoded_secret = Zeroizing::new(percent_encode(secret.expose()));
                let credentials = Zeroizing::new(format!(
                    "{}:{}",
                    percent_encode(&p.config.client_id),
                    *encoded_secret
                ));
                let encoded = Zeroizing::new(
                    base64::engine::general_purpose::STANDARD.encode(credentials.as_bytes()),
                );
                request = request.header("Authorization", format!("Basic {}", *encoded));
            }
            ClientAuth::SecretPost => {
                pairs.push((p.client_id_parameter.name(), &p.config.client_id));
                pairs.push((
                    "client_secret",
                    p.config
                        .client_secret
                        .as_ref()
                        .ok_or(ServerAuthError::InvalidConfig)?
                        .expose(),
                ));
            }
            ClientAuth::None => pairs.push((p.client_id_parameter.name(), &p.config.client_id)),
        }
        // Construct form here so per-value encoded copies are wiped too.
        let mut body = Zeroizing::new(String::new());
        for (k, v) in pairs {
            if !body.is_empty() {
                body.push('&');
            }
            let value = Zeroizing::new(percent_encode(v));
            body.push_str(&percent_encode(k));
            body.push('=');
            body.push_str(&value);
        }
        request.body = std::mem::take(&mut *body).into_bytes();
        request = request.header("Content-Type", "application/x-www-form-urlencoded");
        let request = SensitiveRequest(request);
        // send_any avoids status-error body copies; inspect status before accepting tokens.
        let response = SensitiveResponse(
            self.http
                .send_any(&request.0)
                .map_err(|_| ServerAuthError::ExchangeFailure { status: None })?,
        );
        if (300..400).contains(&response.0.status) {
            return Err(ServerAuthError::ExchangeFailure {
                status: Some(response.0.status),
            });
        }
        let value = SensitiveJson::parse(&response.0.body).map_err(|_| {
            ServerAuthError::ExchangeFailure {
                status: Some(response.0.status),
            }
        })?;
        if !(200..300).contains(&response.0.status) || value.get("error").is_some() {
            return Err(exchange_error(Some(response.0.status), &value, refreshing));
        }
        let mut tokens = crate::parse_tokens(&value, old_refresh).map_err(|_| {
            ServerAuthError::ExchangeFailure {
                status: Some(response.0.status),
            }
        })?;
        tokens.obtained_at = self.clock.now();
        Ok((tokens, value))
    }
}

fn exchange_error(
    status: Option<u16>,
    value: &serde_json::Value,
    refreshing: bool,
) -> ServerAuthError {
    if refreshing
        && matches!(
            value.get("error").and_then(|v| v.as_str()),
            Some("invalid_grant" | "bad_refresh_token" | "invalid_refresh_token")
        )
    {
        ServerAuthError::ReauthRequired
    } else {
        ServerAuthError::ExchangeFailure { status }
    }
}
fn random(bytes: usize) -> Result<String, ServerAuthError> {
    let mut data = Zeroizing::new(vec![0; bytes]);
    getrandom::fill(&mut data).map_err(|_| ServerAuthError::InvalidConfig)?;
    Ok(crate::base64url(&data))
}
pub(super) fn digest(text: &str) -> String {
    crate::base64url(&Sha256::digest(text.as_bytes()))
}
fn binding(p: &ServerProvider) -> String {
    digest(
        &serde_json::json!([
            p.id,
            p.config.client_id,
            p.config.authorization_url,
            p.config.token_url,
            p.redirect_uri,
            p.pkce,
            p.oidc,
            p.client_id_parameter.name(),
            p.scope_separator.to_string(),
            format!("{:?}", p.client_auth),
            p.config.authorization_params
        ])
        .to_string(),
    )
}
