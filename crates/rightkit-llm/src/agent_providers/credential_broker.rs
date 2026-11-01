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
    AuthError, AuthErrorCode, AuthInteraction, AuthSignal, BoxFuture, ModelAuth, REDACTED,
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
/// OAuth leg. `ApiKey` (a stored
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
    SecureStore,
    /// A no-store host's process environment (providers R4). Exclusive with `SecureStore`: the
    /// host has a store or it does not.
    Environment,
    /// A no-store host's `<state root>/.env` (providers R4).
    DotEnvFile,
    Command,
    Extension,
}

impl CredentialSourceKind {
    /// Strict precedence order the broker tries sources in.
    pub const PRECEDENCE: [CredentialSourceKind; 7] = [
        CredentialSourceKind::SupportedSubscriptionOAuth,
        CredentialSourceKind::ApiKey,
        CredentialSourceKind::SecureStore,
        CredentialSourceKind::Environment,
        CredentialSourceKind::DotEnvFile,
        CredentialSourceKind::Command,
        CredentialSourceKind::Extension,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            CredentialSourceKind::SupportedSubscriptionOAuth => "supported_subscription_oauth",
            CredentialSourceKind::ApiKey => "api_key",
            CredentialSourceKind::SecureStore => "secure_store",
            CredentialSourceKind::Environment => "environment",
            CredentialSourceKind::DotEnvFile => "dotenv_file",
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
    /// The host's probe result and its environment and `.env` keys (providers R4). `None` reads
    /// the credential store only.
    #[cfg(feature = "agent_providers-os-keyring")]
    host: Option<Arc<crate::agent_providers::host_credentials::HostCredentials>>,
    /// Lifecycle states the broker itself observed (authenticating, failed, revoked, ...),
    /// per provider. Process-lifetime only; the stored sign-in itself is the durable truth
    /// and `status` derives signed-out / ready / expired from it.
    auth_states: Mutex<HashMap<String, AuthState>>,
}

impl CredentialBroker {
    pub fn new(
        key_store: Arc<dyn KeyStoreBackend + Send + Sync>,
        key_store_service: impl Into<String>,
    ) -> Self {
        Self {
            providers: BTreeMap::new(),
            key_store,
            key_store_service: key_store_service.into(),
            command_resolver: None,
            #[cfg(feature = "agent_providers-os-keyring")]
            host: None,
            auth_states: Mutex::new(HashMap::new()),
        }
    }

    /// Resolve provider keys the way this host keeps them (providers R4): the credential store
    /// when the host has one, otherwise the process environment, then `<state root>/.env`.
    #[cfg(feature = "agent_providers-os-keyring")]
    pub fn with_host_credentials(
        mut self,
        host: Arc<crate::agent_providers::host_credentials::HostCredentials>,
    ) -> Self {
        self.host = Some(host);
        self
    }

    /// The host's no-store credentials, when this broker has a host and the host has no store.
    #[cfg(feature = "agent_providers-os-keyring")]
    fn no_store_host(&self) -> Option<&crate::agent_providers::host_credentials::HostCredentials> {
        self.host.as_deref().filter(|host| !host.has_store())
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

        // 3. The host's key source (providers R4). With an OS credential store: the store, and the
        // environment is not read. With none: the process environment, then `<state root>/.env`,
        // both read once at start.
        if let Some(key_name) = &config.secure_store_key {
            signal.check()?;
            if let Some(resolution) = self.resolve_host_key(provider_id, key_name)? {
                return Ok(Some(resolution));
            }
        }

        // 4. Command.
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

    /// The host's key source for one provider (providers R4): the no-store host's environment
    /// and `.env`, or the credential store.
    fn resolve_host_key(
        &self,
        provider_id: &str,
        key_name: &str,
    ) -> Result<Option<BrokerResolution>, AuthError> {
        #[cfg(feature = "agent_providers-os-keyring")]
        if let Some(host) = self.no_store_host() {
            return match host.provider_key(provider_id) {
                Ok(Some(found)) => {
                    let (source, label) = match found.source {
                        crate::agent_providers::host_credentials::HostKeySource::Environment => {
                            (CredentialSourceKind::Environment, "environment")
                        }
                        crate::agent_providers::host_credentials::HostKeySource::DotEnv => {
                            (CredentialSourceKind::DotEnvFile, ".env")
                        }
                    };
                    Ok(Some(BrokerResolution {
                        auth: ModelAuth::api_key(found.value),
                        source,
                        label: Some(label.to_string()),
                    }))
                }
                Ok(None) => Ok(None),
                Err(reason) => Err(AuthError::new(
                    AuthErrorCode::Storage,
                    format!("key file read failed for {provider_id}: {reason}"),
                )),
            };
        }
        self.read_secure_store(provider_id, key_name)
    }

    fn read_secure_store(
        &self,
        provider_id: &str,
        key_name: &str,
    ) -> Result<Option<BrokerResolution>, AuthError> {
        match self.key_store.get(&self.key_store_service, key_name) {
            Ok(value) => Ok(Some(BrokerResolution {
                auth: ModelAuth::api_key(value),
                source: CredentialSourceKind::SecureStore,
                label: Some("keychain".to_string()),
            })),
            Err(KeyStoreError::NotFound { .. }) => Ok(None),
            Err(error) => Err(AuthError::new(
                AuthErrorCode::Storage,
                format!("secure store read failed for {provider_id}: {error}"),
            )),
        }
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
        #[cfg(feature = "agent_providers-os-keyring")]
        if let Some(host) = self.no_store_host() {
            return host
                .save_provider_key(provider_id, value)
                .map_err(|reason| {
                    AuthError::new(
                        AuthErrorCode::Storage,
                        format!("key file write failed for {provider_id}: {reason}"),
                    )
                });
        }
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
    /// already-absent entry is not an error. `Command`/
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
        #[cfg(feature = "agent_providers-os-keyring")]
        if let Some(host) = self.no_store_host() {
            return host.remove_provider_key(provider_id).map_err(|reason| {
                AuthError::new(
                    AuthErrorCode::Storage,
                    format!("key file delete failed for {provider_id}: {reason}"),
                )
            });
        }
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

#[cfg(all(test, feature = "agent_providers-os-keyring"))]
mod tests {
    use super::*;
    use crate::agent_providers::host_credentials::{CredentialStoreState, HostCredentials};
    use crate::agent_providers::keystore::InMemoryKeyStore;
    use rightkit_http::Secret;
    use rightkit_oauth::{MemoryTokenStore, OAuthClient, OAuthConfig};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::OnceLock;
    use std::time::{Duration, SystemTime};

    /// A scripted subscription flow: the `Resolver`, `MemoryTokenStore` and broker under
    /// test are the real ones; only the interactive login is faked.
    struct TestFlow {
        name: String,
        eligibility: SubscriptionEligibility,
        login_script: Mutex<Vec<Result<TokenSet, AuthError>>>,
        saw_authenticating: Option<Arc<AtomicBool>>,
        broker: Option<Arc<OnceLock<Arc<CredentialBroker>>>>,
    }

    impl TestFlow {
        fn supported(name: &str) -> Self {
            Self {
                name: name.to_string(),
                eligibility: SubscriptionEligibility::Supported,
                login_script: Mutex::new(Vec::new()),
                saw_authenticating: None,
                broker: None,
            }
        }

        fn with_subscription(mut self, eligibility: SubscriptionEligibility) -> Self {
            self.eligibility = eligibility;
            self
        }

        fn with_login_script(self, script: Vec<Result<TokenSet, AuthError>>) -> Self {
            *self.login_script.lock().unwrap() = script;
            self
        }
    }

    impl SubscriptionLogin for TestFlow {
        fn name(&self) -> &str {
            &self.name
        }
        fn subscription(&self) -> SubscriptionEligibility {
            self.eligibility
        }
        fn login<'a>(
            &'a self,
            _interaction: &'a dyn AuthInteraction,
        ) -> BoxFuture<'a, Result<TokenSet, AuthError>> {
            Box::pin(async move {
                if let (Some(flag), Some(cell)) = (&self.saw_authenticating, &self.broker) {
                    let state = cell.get().and_then(|b| b.auth_state("anthropic"));
                    flag.store(
                        matches!(state, Some(AuthState::Authenticating)),
                        Ordering::SeqCst,
                    );
                }
                self.login_script
                    .lock()
                    .unwrap()
                    .pop()
                    .unwrap_or_else(|| Err(AuthError::oauth("login not scripted")))
            })
        }
        fn to_auth(&self, access_token: &str) -> Result<ModelAuth, AuthError> {
            Ok(ModelAuth::api_key(access_token.to_string()))
        }
    }

    struct NoopInteraction;
    impl AuthInteraction for NoopInteraction {
        fn signal(&self) -> AuthSignal {
            AuthSignal::default()
        }
        fn prompt<'a>(
            &'a self,
            _prompt: crate::agent_providers::subscription_auth::types::AuthPrompt,
        ) -> BoxFuture<'a, Result<String, AuthError>> {
            Box::pin(async move { Err(AuthError::auth("no prompt expected")) })
        }
        fn notify(&self, _event: crate::agent_providers::subscription_auth::types::AuthEvent) {}
    }

    fn tokens(
        access: &str,
        refresh: Option<&str>,
        expires_in: Duration,
        age: Duration,
    ) -> TokenSet {
        TokenSet {
            access_token: Secret::new(access),
            token_type: "Bearer".into(),
            expires_in: Some(expires_in),
            refresh_token: refresh.map(Secret::new),
            scope: None,
            id_token: None,
            obtained_at: SystemTime::now() - age,
        }
    }

    /// Valid for another hour.
    fn valid_tokens(access: &str) -> TokenSet {
        tokens(
            access,
            Some("refresh-token"),
            Duration::from_secs(3600),
            Duration::ZERO,
        )
    }

    /// Expired two hours ago.
    fn expired_tokens(access: &str, refresh: Option<&str>) -> TokenSet {
        tokens(
            access,
            refresh,
            Duration::from_secs(3600),
            Duration::from_secs(2 * 3600),
        )
    }

    fn leg(store: Arc<MemoryTokenStore>, flow: TestFlow, account: &str) -> SubscriptionLeg {
        // The token endpoint is unreachable on purpose: a refresh that does run fails as a
        // transient transport error, never as a provider answer.
        let config = OAuthConfig::new(
            "test-client",
            "https://auth.example.test/authorize",
            "http://127.0.0.1:1/token",
        );
        let client = OAuthClient::new(
            config,
            rightkit_http::Client::new(rightkit_http::ClientConfig {
                retry: rightkit_http::RetryPolicy::none(),
                ..rightkit_http::ClientConfig::default()
            }),
        )
        .unwrap();
        let resolver = Resolver::new(client, store.clone(), account)
            .lock_dir(std::env::temp_dir().join("rightkit-agent-broker-test-locks"))
            .refresh_skew(Duration::from_millis(
                DEFAULT_OAUTH_MINIMUM_VALIDITY_MS as u64,
            ));
        SubscriptionLeg::new(resolver, store, Arc::new(flow), account)
    }

    fn broker(
        key_store: InMemoryKeyStore,
        env: impl IntoIterator<Item = (&'static str, &'static str)>,
    ) -> CredentialBroker {
        let credential_broker = CredentialBroker::new(Arc::new(key_store), "rightkit-agent-test");
        let env: Vec<(String, String)> = env
            .into_iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        if env.is_empty() {
            return credential_broker;
        }
        // A host with no OS credential store reads the process environment (providers R4).
        let host = HostCredentials::load(
            CredentialStoreState::None {
                reason: "test".into(),
            },
            None,
            &env,
        );
        credential_broker.with_host_credentials(Arc::new(host))
    }

    fn store_with(account: &str, tokens: TokenSet) -> Arc<MemoryTokenStore> {
        let store = Arc::new(MemoryTokenStore::new());
        store.save(account, &tokens).unwrap();
        store
    }

    #[tokio::test]
    async fn subscription_oauth_wins_over_every_other_configured_source() {
        let store = store_with("anthropic", valid_tokens("oauth-access"));
        let mut credential_broker =
            broker(InMemoryKeyStore::new(), [("ANTHROPIC_API_KEY", "env-key")]);
        credential_broker.register(
            ProviderCredentialConfig::new("anthropic")
                .with_subscription(leg(store, TestFlow::supported("Anthropic"), "anthropic"))
                .with_secure_store_key("anthropic"),
        );

        let resolution = credential_broker
            .resolve("anthropic", AuthSignal::default())
            .await
            .unwrap()
            .expect("subscription oauth must resolve");
        assert_eq!(
            resolution.source,
            CredentialSourceKind::SupportedSubscriptionOAuth
        );
        assert_eq!(resolution.auth.api_key.as_deref(), Some("oauth-access"));
        assert!(!format!("{resolution:?}").contains("oauth-access"));
    }

    #[tokio::test]
    async fn signed_out_subscription_falls_through_to_the_next_source() {
        let store = Arc::new(MemoryTokenStore::new());
        let mut credential_broker =
            broker(InMemoryKeyStore::new(), [("OPENAI_API_KEY", "env-key")]);
        credential_broker.register(
            ProviderCredentialConfig::new("openai")
                .with_subscription(leg(store, TestFlow::supported("OpenAI"), "openai"))
                .with_secure_store_key("openai"),
        );

        let resolution = credential_broker
            .resolve("openai", AuthSignal::default())
            .await
            .unwrap()
            .expect("environment must resolve when signed out");
        assert_eq!(resolution.source, CredentialSourceKind::Environment);
    }

    #[tokio::test]
    async fn unsupported_eligibility_skips_subscription_source_entirely() {
        // Binding product boundary: Unsupported/Unknown never resolve
        // subscription OAuth, even when a credential is stored.
        let store = store_with("vendor", valid_tokens("oauth-access"));
        let mut credential_broker = broker(InMemoryKeyStore::new(), []);
        credential_broker.register(
            ProviderCredentialConfig::new("vendor").with_subscription(leg(
                store,
                TestFlow::supported("vendor")
                    .with_subscription(SubscriptionEligibility::Unsupported),
                "vendor",
            )),
        );

        let resolution = credential_broker
            .resolve("vendor", AuthSignal::default())
            .await
            .unwrap();
        assert!(
            resolution.is_none(),
            "unsupported eligibility must not resolve"
        );
    }

    #[tokio::test]
    async fn unknown_eligibility_also_skips_subscription_source() {
        let store = store_with("vendor", valid_tokens("oauth-access"));
        let mut credential_broker = broker(InMemoryKeyStore::new(), []);
        credential_broker.register(
            ProviderCredentialConfig::new("vendor").with_subscription(leg(
                store,
                TestFlow::supported("vendor").with_subscription(SubscriptionEligibility::Unknown),
                "vendor",
            )),
        );

        assert!(credential_broker
            .resolve("vendor", AuthSignal::default())
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn environment_resolves_when_nothing_is_stored() {
        let mut credential_broker =
            broker(InMemoryKeyStore::new(), [("OPENAI_API_KEY", "env-key")]);
        credential_broker
            .register(ProviderCredentialConfig::new("openai").with_secure_store_key("openai"));

        let resolution = credential_broker
            .resolve("openai", AuthSignal::default())
            .await
            .unwrap()
            .expect("environment must resolve");
        assert_eq!(resolution.source, CredentialSourceKind::Environment);
        assert_eq!(resolution.auth.api_key.as_deref(), Some("env-key"));
    }

    #[tokio::test]
    async fn a_host_with_a_credential_store_never_reads_the_environment() {
        let env = [("OPENAI_API_KEY".to_string(), "env-key".to_string())];
        let host = HostCredentials::load(CredentialStoreState::Present, None, &env);
        let mut credential_broker =
            CredentialBroker::new(Arc::new(InMemoryKeyStore::new()), "rightkit-agent-test")
                .with_host_credentials(Arc::new(host));
        credential_broker
            .register(ProviderCredentialConfig::new("openai").with_secure_store_key("openai"));

        let resolution = credential_broker
            .resolve("openai", AuthSignal::default())
            .await
            .unwrap();
        assert!(resolution.is_none(), "the environment must not be read");
    }

    #[tokio::test]
    async fn secure_store_resolves_below_api_key_and_environment() {
        let key_store = InMemoryKeyStore::new();
        key_store
            .set("rightkit-agent-test", "openai", "keychain-secret")
            .unwrap();
        let mut credential_broker = broker(key_store, []);
        credential_broker
            .register(ProviderCredentialConfig::new("openai").with_secure_store_key("openai"));

        let resolution = credential_broker
            .resolve("openai", AuthSignal::default())
            .await
            .unwrap()
            .expect("secure store must resolve");
        assert_eq!(resolution.source, CredentialSourceKind::SecureStore);
        assert_eq!(resolution.auth.api_key.as_deref(), Some("keychain-secret"));
    }

    #[tokio::test]
    async fn command_resolves_last_before_extension_and_never_logs_the_command_text() {
        struct EchoCommand;
        impl CommandCredentialResolver for EchoCommand {
            fn resolve<'a>(&'a self, command: &'a str) -> BoxFuture<'a, Result<String, AuthError>> {
                let command = command.to_string();
                Box::pin(async move { Ok(format!("resolved:{command}")) })
            }
        }

        let mut credential_broker =
            broker(InMemoryKeyStore::new(), []).with_command_resolver(Arc::new(EchoCommand));
        credential_broker
            .register(ProviderCredentialConfig::new("custom").with_command("op read secret-ref"));

        let resolution = credential_broker
            .resolve("custom", AuthSignal::default())
            .await
            .unwrap()
            .expect("command must resolve");
        assert_eq!(resolution.source, CredentialSourceKind::Command);
        assert_eq!(
            resolution.auth.api_key.as_deref(),
            Some("resolved:op read secret-ref")
        );
        // Label is redacted, not the resolved secret or the raw command.
        assert_eq!(resolution.label.as_deref(), Some(REDACTED));
        let rendered = format!("{resolution:?}");
        assert!(!rendered.contains("resolved:op read secret-ref"));
    }

    #[tokio::test]
    async fn command_source_without_a_wired_resolver_errors_instead_of_silently_skipping() {
        let mut credential_broker = broker(InMemoryKeyStore::new(), []);
        credential_broker
            .register(ProviderCredentialConfig::new("custom").with_command("op read x"));

        let error = credential_broker
            .resolve("custom", AuthSignal::default())
            .await
            .unwrap_err();
        assert_eq!(error.code, AuthErrorCode::Provider);
    }

    #[tokio::test]
    async fn extension_resolves_last() {
        struct AlwaysExtension;
        impl ExtensionCredentialResolver for AlwaysExtension {
            fn resolve<'a>(
                &'a self,
                _provider_id: &'a str,
            ) -> BoxFuture<'a, Result<Option<ModelAuth>, AuthError>> {
                Box::pin(async move { Ok(Some(ModelAuth::api_key("extension-secret"))) })
            }
        }

        let mut credential_broker = broker(InMemoryKeyStore::new(), []);
        credential_broker.register(
            ProviderCredentialConfig::new("custom").with_extension(Arc::new(AlwaysExtension)),
        );

        let resolution = credential_broker
            .resolve("custom", AuthSignal::default())
            .await
            .unwrap()
            .expect("extension must resolve");
        assert_eq!(resolution.source, CredentialSourceKind::Extension);
    }

    #[tokio::test]
    async fn resolving_an_unregistered_provider_errors() {
        let credential_broker = broker(InMemoryKeyStore::new(), []);
        let error = credential_broker
            .resolve("nope", AuthSignal::default())
            .await
            .unwrap_err();
        assert_eq!(error.code, AuthErrorCode::Provider);
    }

    #[tokio::test]
    async fn unconfigured_provider_resolves_unauthenticated_not_an_error() {
        let mut credential_broker = broker(InMemoryKeyStore::new(), []);
        credential_broker.register(ProviderCredentialConfig::new("bare"));

        assert!(credential_broker
            .resolve("bare", AuthSignal::default())
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn status_reports_subscription_auth_state_without_credential_bytes() {
        let store = store_with("anthropic", valid_tokens("oauth-access"));
        let mut credential_broker = broker(InMemoryKeyStore::new(), []);
        credential_broker.register(
            ProviderCredentialConfig::new("anthropic").with_subscription(leg(
                store,
                TestFlow::supported("Anthropic"),
                "anthropic",
            )),
        );

        let record = credential_broker
            .status("anthropic", AuthSignal::default())
            .await
            .unwrap();
        assert!(record.configured);
        assert_eq!(
            record.source,
            Some(CredentialSourceKind::SupportedSubscriptionOAuth)
        );
        assert!(matches!(
            record.subscription_auth_state,
            Some(AuthState::Ready { .. })
        ));
        assert!(record.available_model_ids.is_empty());
        let rendered = serde_json::to_string(&record).unwrap();
        assert!(!rendered.contains("oauth-access"));
        assert!(!rendered.contains("refresh-token"));
    }

    #[tokio::test]
    async fn status_of_an_expired_stored_sign_in_does_not_refresh_it() {
        // Status inspects the store only: no refresh (network) happens.
        let store = store_with(
            "anthropic",
            expired_tokens("old-access", Some("refresh-token")),
        );
        let mut credential_broker = broker(InMemoryKeyStore::new(), []);
        credential_broker.register(
            ProviderCredentialConfig::new("anthropic").with_subscription(leg(
                store,
                TestFlow::supported("Anthropic"),
                "anthropic",
            )),
        );

        let record = credential_broker
            .status("anthropic", AuthSignal::default())
            .await
            .unwrap();
        assert!(record.configured);
        assert_eq!(record.subscription_auth_state, Some(AuthState::Expired));
    }

    #[tokio::test]
    async fn list_status_enumerates_every_registered_provider_sorted_by_id() {
        let mut credential_broker =
            broker(InMemoryKeyStore::new(), [("OPENAI_API_KEY", "env-key")]);
        credential_broker.register(ProviderCredentialConfig::new("zeta"));
        credential_broker
            .register(ProviderCredentialConfig::new("openai").with_secure_store_key("openai"));

        let records = credential_broker
            .list_status(AuthSignal::default())
            .await
            .unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].provider_id, "openai");
        assert!(records[0].configured);
        assert_eq!(records[1].provider_id, "zeta");
        assert!(!records[1].configured);
    }

    #[tokio::test]
    async fn replace_secure_store_key_then_resolve_picks_up_the_new_value() {
        let mut credential_broker = broker(InMemoryKeyStore::new(), []);
        credential_broker
            .register(ProviderCredentialConfig::new("openai").with_secure_store_key("openai"));

        credential_broker
            .replace_secure_store_key("openai", "new-secret")
            .unwrap();
        let resolution = credential_broker
            .resolve("openai", AuthSignal::default())
            .await
            .unwrap()
            .expect("secure store must resolve after replacement");
        assert_eq!(resolution.auth.api_key.as_deref(), Some("new-secret"));

        // Replacing again overwrites rather than erroring on a duplicate.
        credential_broker
            .replace_secure_store_key("openai", "newer-secret")
            .unwrap();
        let resolution = credential_broker
            .resolve("openai", AuthSignal::default())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resolution.auth.api_key.as_deref(), Some("newer-secret"));
    }

    #[tokio::test]
    async fn logout_deletes_subscription_and_secure_store_entries_idempotently() {
        let store = store_with("anthropic", valid_tokens("oauth-access"));
        let key_store = InMemoryKeyStore::new();
        key_store
            .set("rightkit-agent-test", "anthropic", "keychain-secret")
            .unwrap();
        let mut credential_broker = broker(key_store, []);
        credential_broker.register(
            ProviderCredentialConfig::new("anthropic")
                .with_subscription(leg(
                    store.clone(),
                    TestFlow::supported("Anthropic"),
                    "anthropic",
                ))
                .with_secure_store_key("anthropic"),
        );

        credential_broker
            .logout("anthropic", AuthSignal::default())
            .await
            .unwrap();
        assert!(credential_broker
            .resolve("anthropic", AuthSignal::default())
            .await
            .unwrap()
            .is_none());
        assert!(store.load("anthropic").unwrap().is_none());
        assert!(matches!(
            credential_broker.auth_state("anthropic"),
            Some(AuthState::SignedOut)
        ));

        // Logging out again is idempotent, not an error.
        credential_broker
            .logout("anthropic", AuthSignal::default())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn logout_of_an_unregistered_provider_errors() {
        let credential_broker = broker(InMemoryKeyStore::new(), []);
        let error = credential_broker
            .logout("nope", AuthSignal::default())
            .await
            .unwrap_err();
        assert_eq!(error.code, AuthErrorCode::Provider);
    }

    #[tokio::test]
    async fn login_transitions_signed_out_through_authenticating_to_ready() {
        // Typed auth-state transition: SignedOut -> Authenticating -> Ready.
        let seen = Arc::new(AtomicBool::new(false));
        let cell: Arc<OnceLock<Arc<CredentialBroker>>> = Arc::new(OnceLock::new());
        let mut flow = TestFlow::supported("anthropic")
            .with_login_script(vec![Ok(valid_tokens("fresh-access"))]);
        flow.saw_authenticating = Some(seen.clone());
        flow.broker = Some(cell.clone());

        let store = Arc::new(MemoryTokenStore::new());
        let mut credential_broker = broker(InMemoryKeyStore::new(), []);
        credential_broker.register(
            ProviderCredentialConfig::new("anthropic").with_subscription(leg(
                store.clone(),
                flow,
                "anthropic",
            )),
        );
        let credential_broker = Arc::new(credential_broker);
        assert!(cell.set(credential_broker.clone()).is_ok());
        // No login has run yet: no explicit state was ever set.
        assert_eq!(credential_broker.auth_state("anthropic"), None);

        let outcome = credential_broker
            .login("anthropic", &NoopInteraction)
            .await
            .unwrap();
        assert!(matches!(outcome, AuthState::Ready { .. }));
        assert!(
            seen.load(Ordering::SeqCst),
            "auth state must be Authenticating while the flow's login runs"
        );
        assert!(matches!(
            credential_broker.auth_state("anthropic"),
            Some(AuthState::Ready { .. })
        ));
        // Login ends with `Resolver::save`: the tokens are stored and resolve.
        assert_eq!(
            store.load("anthropic").unwrap().unwrap().access_token(),
            "fresh-access"
        );
        let resolution = credential_broker
            .resolve("anthropic", AuthSignal::default())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resolution.auth.api_key.as_deref(), Some("fresh-access"));
    }

    #[tokio::test]
    async fn login_failure_transitions_to_refresh_failed() {
        let mut credential_broker = broker(InMemoryKeyStore::new(), []);
        credential_broker.register(
            ProviderCredentialConfig::new("anthropic").with_subscription(leg(
                Arc::new(MemoryTokenStore::new()),
                TestFlow::supported("anthropic"),
                "anthropic",
            )),
        );

        // The scripted flow's login is unscripted -> errors ("login not scripted").
        let error = credential_broker
            .login("anthropic", &NoopInteraction)
            .await
            .unwrap_err();
        assert_eq!(error.code, AuthErrorCode::OAuth);
        assert!(matches!(
            credential_broker.auth_state("anthropic"),
            Some(AuthState::RefreshFailed { .. })
        ));
    }

    #[tokio::test]
    async fn login_for_an_ineligible_provider_is_refused() {
        let mut credential_broker = broker(InMemoryKeyStore::new(), []);
        credential_broker.register(
            ProviderCredentialConfig::new("vendor").with_subscription(leg(
                Arc::new(MemoryTokenStore::new()),
                TestFlow::supported("vendor").with_subscription(SubscriptionEligibility::Unknown),
                "vendor",
            )),
        );
        let error = credential_broker
            .login("vendor", &NoopInteraction)
            .await
            .unwrap_err();
        assert_eq!(error.code, AuthErrorCode::Provider);
    }

    #[tokio::test]
    async fn dead_grant_transitions_to_revoked_not_refresh_failed() {
        // A stored sign-in that expired with no refresh token (the shape of a grant the
        // provider rejected: the resolver deletes it) is Revoked, distinct from a transient
        // RefreshFailed that a later retry might recover from.
        let store = store_with("anthropic", expired_tokens("expired-access", None));
        let mut credential_broker = broker(InMemoryKeyStore::new(), []);
        credential_broker.register(
            ProviderCredentialConfig::new("anthropic").with_subscription(leg(
                store.clone(),
                TestFlow::supported("anthropic"),
                "anthropic",
            )),
        );

        let error = credential_broker
            .resolve("anthropic", AuthSignal::default())
            .await
            .unwrap_err();
        assert_eq!(error.code, AuthErrorCode::OAuth);
        assert!(matches!(
            credential_broker.auth_state("anthropic"),
            Some(AuthState::Revoked)
        ));
        assert!(
            store.load("anthropic").unwrap().is_none(),
            "the resolver deletes a dead sign-in"
        );
        // Next resolve: signed out, no longer an error.
        assert!(credential_broker
            .resolve("anthropic", AuthSignal::default())
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn refresh_transient_failure_stays_refresh_failed_and_keeps_the_sign_in() {
        let store = store_with(
            "anthropic",
            expired_tokens("expired-access", Some("refresh-token")),
        );
        let mut credential_broker = broker(InMemoryKeyStore::new(), []);
        credential_broker.register(
            ProviderCredentialConfig::new("anthropic").with_subscription(leg(
                store.clone(),
                TestFlow::supported("anthropic"),
                "anthropic",
            )),
        );

        let error = credential_broker
            .resolve("anthropic", AuthSignal::default())
            .await
            .unwrap_err();
        assert_eq!(error.code, AuthErrorCode::OAuth);
        assert!(!is_revocation(&error), "{error}");
        assert!(matches!(
            credential_broker.auth_state("anthropic"),
            Some(AuthState::RefreshFailed { .. })
        ));
        assert!(
            store.load("anthropic").unwrap().is_some(),
            "a transport failure must not sign the user out"
        );
        assert!(!error.message.contains("refresh-token"));
    }

    #[tokio::test]
    async fn login_derives_an_account_fingerprint_that_never_contains_credential_bytes() {
        let flow = TestFlow::supported("anthropic").with_login_script(vec![Ok(tokens(
            "access-secret-bytes",
            Some("refresh-secret-bytes"),
            Duration::from_secs(3600),
            Duration::ZERO,
        ))]);
        let mut credential_broker = broker(InMemoryKeyStore::new(), []);
        credential_broker.register(
            ProviderCredentialConfig::new("anthropic").with_subscription(leg(
                Arc::new(MemoryTokenStore::new()),
                flow,
                "anthropic",
            )),
        );

        credential_broker
            .login("anthropic", &NoopInteraction)
            .await
            .unwrap();

        let record = credential_broker
            .status("anthropic", AuthSignal::default())
            .await
            .unwrap();
        let fingerprint = record
            .account_fingerprint
            .clone()
            .expect("a stored sign-in has a fingerprint");
        assert_eq!(fingerprint.len(), 16);
        assert!(!fingerprint.contains("access-secret-bytes"));
        assert!(!fingerprint.contains("refresh-secret-bytes"));
        let rendered = serde_json::to_string(&record).unwrap();
        assert!(!rendered.contains("access-secret-bytes"));
        assert!(!rendered.contains("refresh-secret-bytes"));
    }

    #[test]
    fn is_revocation_detects_invalid_grant_and_401_403_not_generic_failures() {
        assert!(is_revocation(&AuthError::oauth(
            "OAuth refresh failed for anthropic: invalid_grant"
        )));
        assert!(is_revocation(&AuthError::oauth(
            "HTTP request failed. status=401; url=ignored; body=unauthorized"
        )));
        assert!(is_revocation(&AuthError::oauth(
            "HTTP request failed. status=403; url=ignored; body=forbidden"
        )));
        assert!(is_revocation(&AuthError::oauth(
            "OAuth refresh failed for openai: OAuth provider error invalid_client (HTTP 401)"
        )));
        assert!(!is_revocation(&AuthError::oauth(
            "OAuth refresh failed for anthropic: network timeout"
        )));
        assert!(!is_revocation(&AuthError::oauth(
            "HTTP request failed. status=500; url=ignored; body=server error"
        )));
        assert!(!is_revocation(&AuthError::oauth(
            "OAuth refresh failed for openai: OAuth provider error server_error (HTTP 500)"
        )));
    }

    #[test]
    fn precedence_order_matches_the_canon_row() {
        assert_eq!(
            CredentialSourceKind::PRECEDENCE,
            [
                CredentialSourceKind::SupportedSubscriptionOAuth,
                CredentialSourceKind::ApiKey,
                CredentialSourceKind::SecureStore,
                CredentialSourceKind::Environment,
                CredentialSourceKind::DotEnvFile,
                CredentialSourceKind::Command,
                CredentialSourceKind::Extension,
            ]
        );
    }
}
