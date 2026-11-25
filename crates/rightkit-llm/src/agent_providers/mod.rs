//! `host_providers` — LLM client layer for the host app.
//!
//! Modules:
//! - [`client`]       — Real HTTP streaming client (`OpenAiCompatClient`, key loading)
//! - [`transport`]    — Wire types and `ProviderError`
//! - [`registry`]     — Model name → route mapping
//! - [`keystore`]     — Cross-platform API key storage via `keyring`
//! - [`key_migration`] — Keychain-only, fail-closed provider key storage
//! - [`health`]       — Route health state machine
//! - [`github`]       — Thin GitHub REST API client
//! - [`model_metadata`] — Bundled, loadable pricing + capability registry (`model-metadata.toml`)

pub mod anthropic_client;
mod anthropic_thinking;
pub mod cache_rules;
pub mod client;
pub mod credential_broker;
pub mod extension_registration;
pub mod github;
mod github_guard;
pub mod health;
pub mod history_transform;
pub mod host;
pub mod host_credentials;
mod http;
pub mod key_migration;
pub mod keystore;
pub mod model_metadata;
mod partial_tool_args;
pub mod pricing;
mod reasoning_store;
pub mod registry;
pub mod registry_meta;
pub mod replay;
pub mod subscription_auth;
pub mod token_plan_auth;
pub mod tool_emulation;
pub mod transport;

// Re-exports of key types
pub use anthropic_client::{
    is_thinking_binding_mismatch, AnthropicCompatClient, CacheRetention, ThinkingBlockBinding,
};
pub use client::{
    find_provider, find_provider_by_base_url, is_subscription_billed_route, load_api_key,
    provider_model_id, strip_think_tags, token_plan_provider_for_base_url, transcript_to_wire,
    CacheKeyMode, CapacityError, CapacityProfileV1, CompletedTurn, OpenAiCompatClient,
    PreparedModalityV1, PreparedProviderRequestV1, PreparedRequestGateV1, ProviderInfo,
    ProviderRequestMetadata, ProviderResponseStream, ProviderStreamEvent, RouteCompat,
    CRA10_ESTIMATOR_BASIS, CRA10_ESTIMATOR_BASIS_ID, CRA10_ESTIMATOR_BASIS_VERSION,
    IMAGE_PLACEHOLDER_MARKER, IMAGE_TOKEN_COST, PROVIDERS,
};
pub use extension_registration::{
    ExtensionProviderRegistry, ExtensionRegistrationError, ExtensionRegistrationPolicy,
    ExtensionTrust,
};
pub use github::{GitHubClient, PrFile, PullRequest};
pub use health::{HealthState, RouteHealth};
pub use host::{
    codex_originator, set_app_label, set_codex_originator, set_context_window_resolver,
    set_host_options, set_input_transformation_logger, set_keychain_service,
    set_openrouter_attribution, ProviderHostOptions,
};
pub use host_credentials::{
    credential_env_names, is_credential_env_name, provider_env_names, CredentialStoreState,
    HostCredentials, HostKey, HostKeySource,
};
#[cfg(feature = "agent_providers-os-keyring")]
pub use host_credentials::{probe_credential_store, HostKeyStore};
pub use key_migration::{
    provider_key_storage_health, read_key, remove_key, status_source, write_key, KeySource,
    ProviderKeyStorageHealth,
};
#[cfg(feature = "agent_providers-os-keyring")]
pub use keystore::{keychain_disabled, KeyStore, OsKeyStore};
pub use keystore::{KeyStoreBackend, KeyStoreError};
pub use model_metadata::{
    capability_of, table as model_metadata_table, ModelCapability, ModelTier,
};
pub use pricing::{
    cache_price_per_1k, compaction_capability, context_window_tokens, estimate_cost,
    is_priced_model, long_context_tier, price_per_1k, projected_task_cost,
};
pub use registry::ModelRoute;
pub use subscription_auth::{
    CommandCodeOAuth, OpenAICodexOAuth, COMMAND_CODE_API_BASE, COMMAND_CODE_PROVIDER_BASE,
    OPENAI_AUTHORIZE_URL, OPENAI_CLIENT_ID, OPENAI_CODEX_BASE_URL, OPENAI_CODEX_DEFAULT_MODEL,
    OPENAI_REDIRECT_URI, OPENAI_SCOPE, OPENAI_TOKEN_URL,
};
pub use token_plan_auth::{
    refresh_mobile_token_plan, TokenPlanCredential, TokenPlanError, TokenPlanRefresh,
    TokenPlanRefreshClient, TokenPlanRefreshError, TokenPlanStore,
};
pub use transport::{CompletionDelta, ProtocolFamily, ProviderError, RateLimitMetadata};
