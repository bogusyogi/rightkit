//! PL-001 — Supported-subscription credential adapter.
//!
//! The Pi provider-auth layer (`@earendil-works/pi-ai` 0.84.3,
//! `packages/ai/src/auth/*`) carried through the the host app seam. Its stored-credential,
//! refresh and refresh-lock machinery is now RightKit's: tokens are
//! `rightkit_oauth::TokenSet`, refreshed under a cross-process lock by
//! `rightkit_oauth::credentials::Resolver` over [`token_store::KeychainTokenStore`]
//! (feature `os-keyring`). What stays here is the host app policy and UX:
//!
//! - [`types`]      — `ModelAuth`, `AuthError`/`AuthErrorCode`, login interaction types
//! - [`native_oauth`] — the OpenAI browser/paste login (P-4), the Command Code key login,
//!   and the RightKit OAuth configs the resolver is built from
//! - [`token_store`] — the keychain-backed `TokenStore` (split per value for Windows
//!   Credential Manager's blob limit) and the one-time migration of the old chunked entries
//! - PKCE, state and the loopback callback listener come from `rightkit-oauth`
//! - The RFC 8628 device-code poll lives in `rightkit-oauth` (`OAuthClient::begin_device` /
//!   `finish_device`)
//!
//! the host app-original invariants layered on the port (no donor equivalent):
//!
//! - The **tri-state subscription eligibility gate**
//!   ([`eligibility::SubscriptionEligibility`]): `Supported` / `Unsupported` /
//!   `Unknown`, where only [`SubscriptionEligibility::Supported`] may enable
//!   subscription-backed inference in core (canon "Product boundary"). The
//!   broker (PL-002) refuses to resolve subscription OAuth for any other
//!   state.
//! - **No credential bytes in record types.** The broker records carry metadata
//!   only; `Debug` impls for credential-bearing types (`ModelAuth`, `AuthResult`)
//!   redact values.
//!
pub mod eligibility;
pub mod native_oauth;
#[cfg(feature = "agent_providers-os-keyring")]
pub mod token_store;
pub mod types;

pub use eligibility::{
    auth_state_of_stored, classify_stored_auth_state, token_expires_at_ms, AuthState,
    SubscriptionEligibility, DEFAULT_OAUTH_MINIMUM_VALIDITY_MS, STATUS_MIN_VALIDITY_MS,
};
pub use native_oauth::{
    commandcode_oauth_config, openai_oauth_config, parse_openai_authorization_input,
    sanitize_command_code_api_key, ApiKeyValidator, CommandCodeApiKeyValidator, CommandCodeOAuth,
    FormPoster, HttpFormPoster, OpenAICodexOAuth, SubscriptionLogin, COMMAND_CODE_API_BASE,
    COMMAND_CODE_KEY_LIFETIME_MS, COMMAND_CODE_PROVIDER_BASE, COMMAND_CODE_STUDIO_BASE,
    OPENAI_AUTHORIZE_URL, OPENAI_CALLBACK_PATH, OPENAI_CALLBACK_PORT, OPENAI_CLIENT_ID,
    OPENAI_CODEX_BASE_URL, OPENAI_CODEX_DEFAULT_MODEL, OPENAI_JWT_CLAIM_PATH, OPENAI_REDIRECT_URI,
    OPENAI_SCOPE, OPENAI_TOKEN_URL,
};
#[cfg(feature = "agent_providers-os-keyring")]
pub use token_store::{
    open_subscription_store, subscription_leg, subscription_resolver, KeychainTokenStore,
    SUBSCRIPTION_KEYCHAIN_SERVICE,
};
pub use types::{
    account_fingerprint, expand_tilde, now_ms, ApiKeyAuth, ApiKeyCredential, AuthContext,
    AuthError, AuthErrorCode, AuthEvent, AuthInfoLink, AuthInteraction, AuthPrompt, AuthResult,
    AuthSignal, BoxFuture, ModelAuth, ProcessAuthContext, ProviderEnv, REDACTED,
};
