//! PL-002 — Unified native-inference credential broker.
//!
//! Donor: Pi `auth.json` resolution/precedence
//! (`packages/ai/src/auth/resolve.ts`, `packages/ai/src/auth/credential-store.ts`);
//! the host app keystore (`engine/crates/providers/src/keystore.rs`) retained as
//! substrate, read-only. The `SupportedSubscriptionOAuth` leg is a
//! [`SubscriptionLeg`]: a `rightkit_oauth::credentials::Resolver` (stored sign-in,
//! refresh before expiry under a cross-process lock, revoked-grant cleanup) over the
//! keychain token store in `subscription_auth::token_store`. The broker keeps the
//! tri-state eligibility gate, the typed auth states and the error mapping.
//! `SecureStore`, `Command` and `Extension` are the host app-original sources with no
//! donor equivalent (canon PL-002 row): they extend the donor's single auth.json store
//! to the OS keychain, an operator-configured shell reference and the PL-004
//! extension-secret boundary, under the same one-daemon-owned resolution seam and
//! strict precedence.
//!
//! ## Product boundary (binding)
//!
//! * **Strict precedence.** For a given provider, the broker tries sources
//!   in [`CredentialSourceKind::PRECEDENCE`] order and returns the first one
//!   that is both configured and resolves. A configured-but-unresolvable
//!   source (e.g. an OAuth credential mid-refresh-failure) is a hard error
//!   for that provider, not a silent fall-through — this mirrors PL-001's
//!   "no silent env fallback after a failed refresh" invariant, generalized
//!   across sources.
//! * **Tri-state eligibility gate.** `SupportedSubscriptionOAuth` is
//!   consulted only when the flow's [`SubscriptionEligibility`] is
//!   `Supported`; `Unsupported`/`Unknown` skip the source entirely — the
//!   broker never resolves subscription OAuth outside that gate.
//! * **No credential bytes in broker records.** [`BrokerRecord`] and
//!   [`CredentialSourceKind`] are structurally incapable of carrying secret
//!   bytes. The one credential-bearing type is [`BrokerResolution::auth`]
//!   (a [`ModelAuth`]), which exists to be consumed by a provider transport
//!   on the same call — never persisted, logged, or serialized — exactly
//!   the PL-001 contract.
//! * **Foreign-CLI credentials stay vendor-owned.** This broker resolves
//!   only the host app-owned sources for the host app-initiated native inference;
//!   it has no path to another harness's credential storage (canon PL-002
//!   boundary line).
//!

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use crate::agent_providers::keystore::{KeyStoreBackend, KeyStoreError};
use crate::agent_providers::subscription_auth::eligibility::{
    auth_state_of_stored, AuthState, SubscriptionEligibility,
};
use crate::agent_providers::subscription_auth::types::{
    AuthContext, AuthError, AuthErrorCode, AuthInteraction, AuthSignal, BoxFuture, ModelAuth,
    REDACTED,
};

#[cfg(feature = "agent_providers-os-keyring")]
use crate::agent_providers::subscription_auth::eligibility::{
    token_expires_at_ms, DEFAULT_OAUTH_MINIMUM_VALIDITY_MS,
};
#[cfg(feature = "agent_providers-os-keyring")]
use crate::agent_providers::subscription_auth::native_oauth::SubscriptionLogin;
#[cfg(feature = "agent_providers-os-keyring")]
use rightkit_oauth::credentials::Resolver;
#[cfg(feature = "agent_providers-os-keyring")]
use rightkit_oauth::{OAuthError, TokenSet, TokenStore};

/// One resolvable credential source, in strict precedence order.
///
/// Donor lineage: `SupportedSubscriptionOAuth` is PL-001's tri-state-gated
/// OAuth leg; `Environment` is the donor's ambient-env leg. `ApiKey` (a stored
/// literal credential record) kept its precedence stop, but its store was the
/// retired subscription credential store and no production code ever configured
/// it, so nothing currently resolves with this source; the variant stays so the
/// serialized records and [`CredentialSourceKind::PRECEDENCE`] are unchanged.
/// `SecureStore`, `Command` and `Extension` are the host app-original.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialSourceKind {
    SupportedSubscriptionOAuth,
    ApiKey,
    Environment,
    SecureStore,
    Command,
    Extension,
}

impl CredentialSourceKind {
    /// Strict precedence order the broker tries sources in.
    pub const PRECEDENCE: [CredentialSourceKind; 6] = [
        CredentialSourceKind::SupportedSubscriptionOAuth,
        CredentialSourceKind::ApiKey,
        CredentialSourceKind::Environment,
        CredentialSourceKind::SecureStore,
        CredentialSourceKind::Command,
        CredentialSourceKind::Extension,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            CredentialSourceKind::SupportedSubscriptionOAuth => "supported_subscription_oauth",
            CredentialSourceKind::ApiKey => "api_key",
            CredentialSourceKind::Environment => "environment",
            CredentialSourceKind::SecureStore => "secure_store",
            CredentialSourceKind::Command => "command",
            CredentialSourceKind::Extension => "extension",
        }
    }
}

/// Operator-configured shell command execution seam for the `Command`
/// source. Injectable so tests never spawn a real process; the host wires a
/// real executor (`tokio::process::Command`) at the CLI/daemon seam.
///
/// Must return the resolved secret bytes on success; the broker never logs
/// or stores the returned value, only wraps it in a non-persisted
/// [`ModelAuth`].
pub trait CommandCredentialResolver: Send + Sync {
    fn resolve<'a>(&'a self, command: &'a str) -> BoxFuture<'a, Result<String, AuthError>>;
}

/// PL-004 extension-secret boundary seam for the `Extension` source. PL-004
/// (custom provider/extension registration) owns the real extension
/// registry; this broker only needs a typed resolver per provider so an
/// extension's declared secret can participate in precedence without the
/// broker knowing anything about extension internals.
pub trait ExtensionCredentialResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        provider_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<ModelAuth>, AuthError>>;
}

/// The `SupportedSubscriptionOAuth` leg for one provider: RightKit's `Resolver` (stored
/// sign-in, refresh under a cross-process lock, dead-grant cleanup), the token store it
/// reads (kept here for status, which must not trigger a refresh), and the provider's
/// login flow. `Resolver::resolve` and the store are blocking (keychain, file lock,
/// token endpoint), so every call here runs on `tokio::task::spawn_blocking`.
#[cfg(feature = "agent_providers-os-keyring")]
pub struct SubscriptionLeg {
    resolver: Arc<Resolver>,
    tokens: Arc<dyn TokenStore>,
    flow: Arc<dyn SubscriptionLogin>,
    account: String,
    pub eligibility: SubscriptionEligibility,
    pub name: String,
}

#[cfg(feature = "agent_providers-os-keyring")]
impl SubscriptionLeg {
    /// `tokens` must be the store the `resolver` was built over. `account` is the
    /// `Resolver` account (the provider id).
    pub fn new(
        resolver: Resolver,
        tokens: Arc<dyn TokenStore>,
        flow: Arc<dyn SubscriptionLogin>,
        account: impl Into<String>,
    ) -> Self {
        Self {
            resolver: Arc::new(resolver),
            tokens,
            eligibility: flow.subscription(),
            name: flow.name().to_string(),
            flow,
            account: account.into(),
        }
    }

    async fn blocking<T: Send + 'static>(
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, AuthError> {
        tokio::task::spawn_blocking(work).await.map_err(|_| {
            AuthError::new(
                AuthErrorCode::Provider,
                "subscription credential worker did not complete",
            )
        })
    }

    fn store_error(&self, what: &str, error: OAuthError) -> AuthError {
        AuthError::new(
            AuthErrorCode::Storage,
            format!(
                "subscription credential store {what} failed for {}",
                self.account
            ),
        )
        .with_context(&AuthError::storage(error.to_string()))
    }

    /// The stored sign-in as is (possibly expired); never refreshes.
    async fn load(&self) -> Result<Option<TokenSet>, AuthError> {
        let tokens = self.tokens.clone();
        let account = self.account.clone();
        Self::blocking(move || tokens.load(&account))
            .await?
            .map_err(|error| self.store_error("read", error))
    }

    /// A request-ready credential, refreshed when due. `Ok(None)` means signed out (nothing
    /// stored); a stored sign-in whose grant is dead is an error, not a silent fall-through.
    async fn resolve(&self, signal: &AuthSignal) -> Result<Option<ModelAuth>, AuthError> {
        signal.check()?;
        let resolver = self.resolver.clone();
        let tokens = self.tokens.clone();
        let account = self.account.clone();
        let skew = std::time::Duration::from_millis(DEFAULT_OAUTH_MINIMUM_VALIDITY_MS as u64);
        let outcome = Self::blocking(
            move || -> Result<(bool, Option<rightkit_http::Secret>), OAuthError> {
                let stored = tokens.load(&account)?;
                if let Some(current) = &stored {
                    if !current.is_expired(skew) {
                        return Ok((true, Some(current.access_token.clone())));
                    }
                }
                let had_sign_in = stored.is_some();
                let token = match resolver.resolve()? {
                    rightkit_oauth::credentials::Resolution::Token { token, .. } => Some(token),
                    rightkit_oauth::credentials::Resolution::NeedsSignIn { .. } => None,
                };
                Ok((had_sign_in, token))
            },
        )
        .await?;
        let (had_sign_in, token) = outcome.map_err(|error| match error {
            OAuthError::Storage(_) => self.store_error("read", error),
            other => AuthError::new(
                AuthErrorCode::OAuth,
                format!("OAuth refresh failed for {}", self.account),
            )
            .with_context(&AuthError::oauth(other.to_string())),
        })?;
        signal.check()?;
        match token {
            Some(token) => self
                .flow
                .to_auth(token.expose())
                .map(Some)
                .map_err(|error| {
                    AuthError::new(
                        AuthErrorCode::OAuth,
                        format!("OAuth auth derivation failed for {}", self.account),
                    )
                    .with_context(&error)
                }),
            // The resolver already deleted a grant the provider rejected (or a sign-in that
            // expired with no refresh token): re-authentication, not a retry, is required.
            None if had_sign_in => Err(AuthError::new(
                AuthErrorCode::OAuth,
                format!(
                    "subscription sign-in for {} is no longer valid (invalid_grant); sign in again",
                    self.account
                ),
            )),
            None => Ok(None),
        }
    }

    async fn save(&self, tokens: TokenSet) -> Result<(), AuthError> {
        let resolver = self.resolver.clone();
        Self::blocking(move || resolver.save(&tokens))
            .await?
            .map_err(|error| self.store_error("write", error))
    }

    async fn sign_out(&self) -> Result<(), AuthError> {
        let resolver = self.resolver.clone();
        Self::blocking(move || resolver.sign_out())
            .await?
            .map_err(|error| self.store_error("delete", error))
    }
}

/// Stable account identifier hash, never credential material. The refresh token (stable per
/// sign-in, never echoed) is the material when the provider issued one, as at sign-in before;
/// otherwise the access token (Command Code's API key).
#[cfg(feature = "agent_providers-os-keyring")]
fn token_fingerprint(tokens: &TokenSet) -> String {
    let material = tokens
        .refresh_token
        .as_ref()
        .map(|refresh| refresh.expose())
        .unwrap_or_else(|| tokens.access_token());
    crate::agent_providers::subscription_auth::account_fingerprint(material)
}

/// Per-provider source configuration. Any subset of fields may be `None` /
/// empty; the broker skips unconfigured sources.
pub struct ProviderCredentialConfig {
    pub provider_id: String,
    /// `SupportedSubscriptionOAuth` leg. The broker consults the leg's
    /// [`SubscriptionEligibility`] before ever reading the store. Host-only: the iPhone
    /// runtime owns its credentials in Swift, so this leg does not exist without
    /// `os-keyring`.
    #[cfg(feature = "agent_providers-os-keyring")]
    pub subscription: Option<Arc<SubscriptionLeg>>,
    /// `Environment` leg: env var names checked in order, independent of
    /// anything stored.
    pub env_vars: Vec<String>,
    /// `SecureStore` leg: OS-keychain key name. Defaults to `provider_id`
    /// when configured with `Some(None)`... callers pass the explicit name.
    pub secure_store_key: Option<String>,
    /// `Command` leg: the operator-authored command string.
    pub command: Option<String>,
    /// `Extension` leg: PL-004 boundary resolver.
    pub extension: Option<Arc<dyn ExtensionCredentialResolver>>,
}

impl ProviderCredentialConfig {
    pub fn new(provider_id: impl Into<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
            #[cfg(feature = "agent_providers-os-keyring")]
            subscription: None,
            env_vars: Vec::new(),
            secure_store_key: None,
            command: None,
            extension: None,
        }
    }

    #[cfg(feature = "agent_providers-os-keyring")]
    pub fn with_subscription(mut self, leg: SubscriptionLeg) -> Self {
        self.subscription = Some(Arc::new(leg));
        self
    }

    pub fn with_env_vars(mut self, vars: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.env_vars = vars.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_secure_store_key(mut self, key: impl Into<String>) -> Self {
        self.secure_store_key = Some(key.into());
        self
    }

    pub fn with_command(mut self, command: impl Into<String>) -> Self {
        self.command = Some(command.into());
        self
    }

    pub fn with_extension(mut self, extension: Arc<dyn ExtensionCredentialResolver>) -> Self {
        self.extension = Some(extension);
        self
    }

    fn has_subscription(&self) -> bool {
        #[cfg(feature = "agent_providers-os-keyring")]
        {
            self.subscription.is_some()
        }
        #[cfg(not(feature = "agent_providers-os-keyring"))]
        {
            false
        }
    }

    fn subscription_eligibility(&self) -> SubscriptionEligibility {
        #[cfg(feature = "agent_providers-os-keyring")]
        {
            self.subscription
                .as_ref()
                .map(|leg| leg.eligibility)
                .unwrap_or(SubscriptionEligibility::Unknown)
        }
        #[cfg(not(feature = "agent_providers-os-keyring"))]
        {
            SubscriptionEligibility::Unknown
        }
    }
}

/// A resolved credential ready for one provider transport call. The single
/// credential-bearing type this module produces; never persisted, logged or
/// serialized past the call site (PL-001 `AuthResult` contract).
#[derive(Clone, PartialEq)]
pub struct BrokerResolution {
    pub auth: ModelAuth,
    pub source: CredentialSourceKind,
    /// Human-readable label for status UI, mirroring PL-001's `AuthResult`.
    pub label: Option<String>,
}

impl std::fmt::Debug for BrokerResolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrokerResolution")
            .field("auth", &self.auth)
            .field("source", &self.source)
            .field("label", &self.label)
            .finish()
    }
}

/// Non-secret status record for account/status enumeration. Structurally
/// incapable of carrying credential bytes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BrokerRecord {
    pub provider_id: String,
    pub configured: bool,
    pub source: Option<CredentialSourceKind>,
    /// Subscription lifecycle independent of the selected inference source.
    /// Unreadable storage reports `Unavailable` rather than signed-out.
    pub subscription_auth_state: Option<AuthState>,
    /// Stable account identifier hash, never credential material.
    pub account_fingerprint: Option<String>,
    /// Provider-scoped model availability learned from subscription auth.
    pub available_model_ids: Vec<String>,
    pub subscription_eligibility: SubscriptionEligibility,
}

/// The broker itself: one daemon-owned resolution seam over every
/// registered provider's configured sources.
pub struct CredentialBroker {
    providers: BTreeMap<String, ProviderCredentialConfig>,
    key_store: Arc<dyn KeyStoreBackend + Send + Sync>,
    key_store_service: String,
    command_resolver: Option<Arc<dyn CommandCredentialResolver>>,
    auth_context: Arc<dyn AuthContext>,
    /// Lifecycle states the broker itself observed (authenticating, failed, revoked, ...),
    /// per provider. Process-lifetime only; the stored sign-in itself is the durable truth
    /// and `status` derives signed-out / ready / expired from it.
    auth_states: Mutex<HashMap<String, AuthState>>,
}

impl CredentialBroker {
    pub fn new(
        key_store: Arc<dyn KeyStoreBackend + Send + Sync>,
        key_store_service: impl Into<String>,
        auth_context: Arc<dyn AuthContext>,
    ) -> Self {
        Self {
            providers: BTreeMap::new(),
            key_store,
            key_store_service: key_store_service.into(),
            command_resolver: None,
            auth_context,
            auth_states: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_command_resolver(mut self, resolver: Arc<dyn CommandCredentialResolver>) -> Self {
        self.command_resolver = Some(resolver);
        self
    }

    pub fn register(&mut self, config: ProviderCredentialConfig) {
        self.providers.insert(config.provider_id.clone(), config);
    }

    /// The lifecycle state the broker last recorded for a provider in this process, if any.
    pub fn auth_state(&self, provider_id: &str) -> Option<AuthState> {
        self.auth_states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(provider_id)
            .cloned()
    }

    fn set_auth_state(&self, provider_id: &str, state: AuthState) {
        self.auth_states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(provider_id.to_string(), state);
    }

    /// Run supported-subscription login through the daemon-owned store seam.
    /// The provider flow owns callback/device mechanics; this method owns
    /// eligibility, persistence, and the typed result boundary.
    pub async fn login(
        &self,
        provider_id: &str,
        interaction: &dyn AuthInteraction,
    ) -> Result<AuthState, AuthError> {
        let config = self.provider(provider_id)?;
        #[cfg(feature = "agent_providers-os-keyring")]
        {
            let leg = config.subscription.as_ref().ok_or_else(|| {
                AuthError::new(
                    AuthErrorCode::Provider,
                    format!("provider {provider_id} has no subscription login"),
                )
            })?;
            if !leg.eligibility.enabled_in_core() {
                return Err(AuthError::new(
                    AuthErrorCode::Provider,
                    format!("subscription login is not eligible for {provider_id}"),
                ));
            }
            interaction.signal().check()?;
            self.set_auth_state(provider_id, AuthState::Authenticating);
            let tokens = match leg.flow.login(interaction).await {
                Ok(tokens) => tokens,
                Err(error) => {
                    self.set_auth_state(
                        provider_id,
                        AuthState::RefreshFailed {
                            message: "provider login failed".to_string(),
                        },
                    );
                    return Err(error);
                }
            };
            let expires_at_ms = token_expires_at_ms(&tokens);
            leg.save(tokens).await?;
            let state = AuthState::Ready { expires_at_ms };
            self.set_auth_state(provider_id, state.clone());
            Ok(state)
        }
        #[cfg(not(feature = "agent_providers-os-keyring"))]
        {
            let _ = (config, interaction);
            Err(AuthError::new(
                AuthErrorCode::Provider,
                format!("provider {provider_id} has no subscription login"),
            ))
        }
    }

    fn provider(&self, provider_id: &str) -> Result<&ProviderCredentialConfig, AuthError> {
        self.providers.get(provider_id).ok_or_else(|| {
            AuthError::new(
                AuthErrorCode::Provider,
                format!("no credential sources registered for provider {provider_id}"),
            )
        })
    }

    /// Resolve one provider's credential following strict precedence.
    /// `Ok(None)` means no configured source resolved (unauthenticated); a
    /// configured-but-broken source (refresh failure, unreadable secure
    /// store entry, failing command) is `Err`, never a silent skip to the
    /// next source.
    pub async fn resolve(
        &self,
        provider_id: &str,
        signal: AuthSignal,
    ) -> Result<Option<BrokerResolution>, AuthError> {
        let result = self.resolve_inner(provider_id, signal).await;
        if let Err(error) = &result {
            if let Some(config) = self.providers.get(provider_id) {
                if config.has_subscription() {
                    let state = if error.code == AuthErrorCode::OAuth {
                        if is_revocation(error) {
                            // The provider itself rejected the refresh token
                            // as no longer valid (invalid_grant / 401 / 403):
                            // distinct from a transient refresh failure —
                            // re-authentication, not a retry, is required.
                            AuthState::Revoked
                        } else {
                            AuthState::RefreshFailed {
                                message: "provider token refresh failed".to_string(),
                            }
                        }
                    } else {
                        AuthState::Expired
                    };
                    self.set_auth_state(provider_id, state);
                }
            }
        }
        result
    }

    async fn resolve_inner(
        &self,
        provider_id: &str,
        signal: AuthSignal,
    ) -> Result<Option<BrokerResolution>, AuthError> {
        let config = self.provider(provider_id)?;

        // 1. SupportedSubscriptionOAuth.
        #[cfg(feature = "agent_providers-os-keyring")]
        {
            if let Some(leg) = &config.subscription {
                signal.check()?;
                if leg.eligibility.enabled_in_core() {
                    if let Some(auth) = leg.resolve(&signal).await? {
                        return Ok(Some(BrokerResolution {
                            auth,
                            source: CredentialSourceKind::SupportedSubscriptionOAuth,
                            label: Some("OAuth".to_string()),
                        }));
                    }
                    // Signed out (no stored sign-in): fall through to the next source,
                    // exactly as a logout mid-refresh did.
                }
            }
        }

        // 2. ApiKey — retired with the subscription credential store (see
        // `CredentialSourceKind`); no stored-literal leg remains.

        // 3. Environment.
        if !config.env_vars.is_empty() {
            signal.check()?;
            for var in &config.env_vars {
                if let Some(value) = self.auth_context.env(var).await {
                    return Ok(Some(BrokerResolution {
                        auth: ModelAuth::api_key(value),
                        source: CredentialSourceKind::Environment,
                        label: Some(var.clone()),
                    }));
                }
            }
        }

        // 4. SecureStore (OS keychain).
        if let Some(key_name) = &config.secure_store_key {
            signal.check()?;
            match self.key_store.get(&self.key_store_service, key_name) {
                Ok(value) => {
                    return Ok(Some(BrokerResolution {
                        auth: ModelAuth::api_key(value),
                        source: CredentialSourceKind::SecureStore,
                        label: Some("keychain".to_string()),
                    }));
                }
                Err(KeyStoreError::NotFound { .. }) => {}
                Err(error) => {
                    return Err(AuthError::new(
                        AuthErrorCode::Storage,
                        format!("secure store read failed for {provider_id}: {error}"),
                    ));
                }
            }
        }

        // 5. Command.
        if let Some(command) = &config.command {
            signal.check()?;
            let resolver = self.command_resolver.as_ref().ok_or_else(|| {
                AuthError::new(
                    AuthErrorCode::Provider,
                    format!("provider {provider_id} configures a command credential source but no CommandCredentialResolver is wired"),
                )
            })?;
            let value = resolver.resolve(command).await.map_err(|error| {
                AuthError::new(
                    AuthErrorCode::Auth,
                    format!("command credential resolution failed for {provider_id}"),
                )
                .with_context(&error)
            })?;
            return Ok(Some(BrokerResolution {
                auth: ModelAuth::api_key(value),
                source: CredentialSourceKind::Command,
                label: Some(REDACTED.to_string()),
            }));
        }

        // 6. Extension.
        if let Some(extension) = &config.extension {
            signal.check()?;
            if let Some(auth) = extension.resolve(provider_id).await? {
                return Ok(Some(BrokerResolution {
                    auth,
                    source: CredentialSourceKind::Extension,
                    label: Some("extension".to_string()),
                }));
            }
        }

        Ok(None)
    }

    /// Non-secret status enumeration (donor `CredentialStore.list`
    /// contract, extended across sources). Never executes a configured
    /// command or reads secure-store bytes past existence.
    pub async fn status(
        &self,
        provider_id: &str,
        signal: AuthSignal,
    ) -> Result<BrokerRecord, AuthError> {
        match self.status_inner(provider_id, signal).await {
            Err(error) if error.code == AuthErrorCode::Storage => {
                let config = self.provider(provider_id)?;
                Ok(BrokerRecord {
                    provider_id: provider_id.to_string(),
                    configured: false,
                    source: None,
                    subscription_auth_state: Some(AuthState::Unavailable {
                        reason: "credential_store_unavailable".to_string(),
                    }),
                    account_fingerprint: None,
                    available_model_ids: Vec::new(),
                    subscription_eligibility: config.subscription_eligibility(),
                })
            }
            result => result,
        }
    }

    async fn status_inner(
        &self,
        provider_id: &str,
        signal: AuthSignal,
    ) -> Result<BrokerRecord, AuthError> {
        let config = self.provider(provider_id)?;
        let eligibility = config.subscription_eligibility();

        #[cfg(feature = "agent_providers-os-keyring")]
        {
            if let Some(leg) = &config.subscription {
                if leg.eligibility.enabled_in_core() {
                    signal.check()?;
                    let stored = leg.load().await?;
                    if let Some(tokens) = stored.as_ref() {
                        return Ok(BrokerRecord {
                            provider_id: provider_id.to_string(),
                            configured: true,
                            source: Some(CredentialSourceKind::SupportedSubscriptionOAuth),
                            subscription_auth_state: Some(
                                self.auth_state(provider_id)
                                    .unwrap_or_else(|| auth_state_of_stored(stored.as_ref())),
                            ),
                            account_fingerprint: Some(token_fingerprint(tokens)),
                            // Model availability was never populated by the supported flows;
                            // the field stays for the response schema.
                            available_model_ids: Vec::new(),
                            subscription_eligibility: eligibility,
                        });
                    }
                }
            }
        }

        let resolution = self.resolve(provider_id, signal).await?;
        Ok(BrokerRecord {
            provider_id: provider_id.to_string(),
            configured: resolution.is_some(),
            source: resolution.as_ref().map(|r| r.source),
            subscription_auth_state: if config.has_subscription() {
                Some(
                    self.auth_state(provider_id)
                        .unwrap_or_else(|| auth_state_of_stored(None)),
                )
            } else {
                None
            },
            account_fingerprint: None,
            available_model_ids: Vec::new(),
            subscription_eligibility: eligibility,
        })
    }

    /// List every registered provider's non-secret status, sorted by
    /// provider id (donor `CredentialStore.list` enumeration contract).
    pub async fn list_status(&self, signal: AuthSignal) -> Result<Vec<BrokerRecord>, AuthError> {
        let mut records = Vec::with_capacity(self.providers.len());
        for provider_id in self.providers.keys() {
            records.push(self.status(provider_id, signal.clone()).await?);
        }
        Ok(records)
    }

    /// Replace the `SecureStore` credential for a provider (the
    /// `replacement` contract leg). The value supplied is the one the
    /// caller just obtained interactively — same discipline as
    /// `KeyStore::set`.
    pub fn replace_secure_store_key(
        &self,
        provider_id: &str,
        value: &str,
    ) -> Result<(), AuthError> {
        let config = self.provider(provider_id)?;
        let key_name = config.secure_store_key.as_deref().unwrap_or(provider_id);
        self.key_store
            .set(&self.key_store_service, key_name, value)
            .map_err(|error| {
                AuthError::new(
                    AuthErrorCode::Storage,
                    format!("secure store write failed for {provider_id}: {error}"),
                )
            })
    }

    /// Logout/delete: removes the stored subscription sign-in and the
    /// secure-store entry for a provider. Idempotent — deleting an
    /// already-absent entry is not an error. `Environment`/`Command`/
    /// `Extension` sources are operator/extension-owned
    /// configuration, not broker-managed secrets, so logout does not touch
    /// them (mirrors the donor: only the stored credential the store owns
    /// is deleted; ambient env is never mutated by the app).
    pub async fn logout(&self, provider_id: &str, signal: AuthSignal) -> Result<(), AuthError> {
        let config = self.provider(provider_id)?;

        #[cfg(feature = "agent_providers-os-keyring")]
        {
            if let Some(leg) = &config.subscription {
                signal.check()?;
                leg.sign_out().await?;
                self.set_auth_state(provider_id, AuthState::SignedOut);
            }
        }
        #[cfg(not(feature = "agent_providers-os-keyring"))]
        let _ = &signal;
        if let Some(key_name) = &config.secure_store_key {
            match self.key_store.delete(&self.key_store_service, key_name) {
                Ok(()) | Err(KeyStoreError::NotFound { .. }) => {}
                Err(error) => {
                    return Err(AuthError::new(
                        AuthErrorCode::Storage,
                        format!("secure store delete failed for {provider_id}: {error}"),
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Distinguish a provider-side revocation from a transient refresh failure.
/// OAuth providers report a revoked/expired refresh token as `invalid_grant`
/// (RFC 6749 §5.2) or an HTTP 401/403; anything else (network error, 5xx,
/// timeout) is transient and stays `RefreshFailed` so a later retry is not
/// mistaken for requiring re-authentication.
fn is_revocation(error: &AuthError) -> bool {
    let message = error.message.to_ascii_lowercase();
    message.contains("invalid_grant")
        || message.contains("status=401")
        || message.contains("status=403")
        || message.contains("(http 401)")
        || message.contains("(http 403)")
        || message.contains("revoked")
}
