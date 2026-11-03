//! PL-004 provider-extension registration adapter.
//!
//! Donor lineage: Pi `packages/ai/src/models.ts` and the custom-provider
//! helpers in `packages/ai/src/providers/*`. This is the Rust runtime half of
//! the serializable contract in `host_plugins::provider_extension`.
//! The two crates intentionally meet at the JSON manifest boundary so the
//! plugin loader does not depend on provider transports.
//!
//! This module is additive: it registers routes and composed `RegistryMeta`
//! rows through the existing registries, and resolves extension credentials
//! through PL-002's `ExtensionCredentialResolver`. It does not replace the
//! built-in route/catalog stores, import foreign-harness credentials, or grant
//! an extension core trust.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use crate::agent_providers::credential_broker::ExtensionCredentialResolver;
use crate::agent_providers::model_metadata::{ModelCapability, ModelTier};
use crate::agent_providers::registry::{ModelRegistry, ModelRoute};
use crate::agent_providers::registry_meta::{
    from_route_and_capability, AuthMode, AvailabilityReason, CatalogFreshness, CatalogSourceKind,
    RegistryMeta,
};
use crate::agent_providers::subscription_auth::eligibility::SubscriptionEligibility;
use crate::agent_providers::subscription_auth::types::{AuthError, BoxFuture, ModelAuth};
use crate::agent_providers::{ProviderError, ProviderResponseStream};

pub const PROVIDER_EXTENSION_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExtensionRegistrationError {
    #[error("provider extension is invalid: {0}")]
    Invalid(String),
    #[error("provider extension `{0}` is already registered")]
    DuplicateProvider(String),
    #[error("model route `{0}` collides with an existing built-in or configured route")]
    RouteCollision(String),
    #[error("extension `{provider}` requested core subscription eligibility without an explicit trust grant")]
    TrustRequired { provider: String },
    #[error("extension `{provider}` has unsupported capability tier `{tier}`")]
    InvalidTier { provider: String, tier: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionTrust {
    /// Default for installed/enabled extensions. It may provide API-backed
    /// inference, but cannot turn a subscription declaration into core trust.
    Untrusted,
    /// A separate host policy decision, never inferred from the manifest.
    ExplicitlyGranted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionRegistrationPolicy {
    pub trust: ExtensionTrust,
    pub availability: AvailabilityReason,
    pub catalog_source: CatalogSourceKind,
    pub freshness: CatalogFreshness,
}

impl Default for ExtensionRegistrationPolicy {
    fn default() -> Self {
        Self {
            trust: ExtensionTrust::Untrusted,
            availability: AvailabilityReason::Unchecked,
            // The host supplies the actual provenance. Embedded here means
            // only "the host chose this source", not "extension is built-in".
            catalog_source: CatalogSourceKind::Embedded,
            freshness: CatalogFreshness::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProtocol {
    OpenAiCompatible,
    Anthropic,
    Custom { name: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EndpointOrigin {
    BuiltIn,
    Extension { extension_id: String },
    UserConfigured { config_key: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointDeclaration {
    pub base_url: String,
    pub origin: EndpointOrigin,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthResolverDeclaration {
    ApiKey { env_vars: Vec<String> },
    OAuth,
    Ambient { name: String },
    Extension { resolver_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretDeclaration {
    pub name: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub purpose: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthProbeDeclaration {
    pub kind: HealthProbeKind,
    #[serde(default)]
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthProbeKind {
    None,
    Endpoint,
    Extension,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtensionModelDeclaration {
    pub id: String,
    pub slug: String,
    #[serde(default)]
    pub display_name: String,
    pub context_window_tokens: u64,
    #[serde(default)]
    pub reasoning: u8,
    #[serde(default)]
    pub vision: bool,
    #[serde(default)]
    pub tools: bool,
    #[serde(default)]
    pub streaming: bool,
    #[serde(default)]
    pub prompt_caching: bool,
    #[serde(default)]
    pub usage_reporting: bool,
    #[serde(default)]
    pub tool_schema_class: String,
    #[serde(default)]
    pub prompt_per_m: f64,
    #[serde(default)]
    pub completion_per_m: f64,
    #[serde(default = "default_priority")]
    pub priority: u8,
    #[serde(default)]
    pub tier: String,
}

fn default_priority() -> u8 {
    100
}

/// Provider-side copy of the plugin wire contract. Keeping this DTO plain
/// and serde-compatible permits the integration owner to pass the plugin
/// manifest over the existing loader boundary without a crate dependency.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderExtensionDocument {
    pub schema_version: u16,
    pub id: String,
    pub display_name: String,
    pub version: String,
    pub protocol: ProviderProtocol,
    pub endpoint: EndpointDeclaration,
    pub auth: AuthResolverDeclaration,
    #[serde(default)]
    pub secrets: Vec<SecretDeclaration>,
    #[serde(default)]
    pub models: Vec<ExtensionModelDeclaration>,
    pub health_probe: HealthProbeDeclaration,
    #[serde(default)]
    pub requested_subscription_eligibility: Option<String>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

impl ProviderExtensionDocument {
    pub fn from_json(json: &str) -> Result<Self, ExtensionRegistrationError> {
        let document: Self = serde_json::from_str(json)
            .map_err(|error| ExtensionRegistrationError::Invalid(error.to_string()))?;
        document.validate()?;
        Ok(document)
    }

    pub fn validate(&self) -> Result<(), ExtensionRegistrationError> {
        if self.schema_version != PROVIDER_EXTENSION_SCHEMA_VERSION {
            return Err(ExtensionRegistrationError::Invalid(format!(
                "unsupported schema version {}",
                self.schema_version
            )));
        }
        if !valid_id(&self.id) {
            return Err(ExtensionRegistrationError::Invalid(
                "id must be a non-empty provider-safe identifier".into(),
            ));
        }
        if self.display_name.trim().is_empty() || self.version.trim().is_empty() {
            return Err(ExtensionRegistrationError::Invalid(
                "display_name and version are required".into(),
            ));
        }
        validate_endpoint(&self.endpoint)?;
        if self.models.is_empty() {
            return Err(ExtensionRegistrationError::Invalid(
                "at least one model is required".into(),
            ));
        }
        let mut ids = std::collections::HashSet::new();
        for model in &self.models {
            if model.id.trim().is_empty()
                || model.slug
                    != format!(
                        "{}/{}",
                        self.id,
                        model.slug.rsplit('/').next().unwrap_or_default()
                    )
                || model.context_window_tokens == 0
                || model.reasoning > 3
                || !model.prompt_per_m.is_finite()
                || !model.completion_per_m.is_finite()
                || model.prompt_per_m < 0.0
                || model.completion_per_m < 0.0
            {
                return Err(ExtensionRegistrationError::Invalid(format!(
                    "invalid model declaration `{}`",
                    model.id
                )));
            }
            if !ids.insert(&model.id) {
                return Err(ExtensionRegistrationError::Invalid(format!(
                    "duplicate model id `{}`",
                    model.id
                )));
            }
        }
        for secret in &self.secrets {
            if secret.name.trim().is_empty() {
                return Err(ExtensionRegistrationError::Invalid(
                    "secret declarations require a name".into(),
                ));
            }
        }
        Ok(())
    }
}

fn valid_id(value: &str) -> bool {
    !value.trim().is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

fn validate_endpoint(endpoint: &EndpointDeclaration) -> Result<(), ExtensionRegistrationError> {
    let url = endpoint.base_url.to_ascii_lowercase();
    if !(url.starts_with("https://") || url.starts_with("http://"))
        || endpoint.base_url.chars().any(char::is_whitespace)
    {
        return Err(ExtensionRegistrationError::Invalid(
            "endpoint base_url must be an absolute http(s) URL".into(),
        ));
    }
    for name in endpoint.headers.keys() {
        let lower = name.to_ascii_lowercase();
        if lower == "authorization" || lower.contains("api-key") || lower.contains("token") {
            return Err(ExtensionRegistrationError::Invalid(format!(
                "secret-bearing header `{name}` must be resolved at request time"
            )));
        }
    }
    Ok(())
}

/// Endpoint resolution retains both provenance records. A user override is
/// never rewritten as an extension endpoint, and the extension default is
/// never mistaken for a built-in route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionEndpoint {
    pub extension_default: EndpointDeclaration,
    pub user_override: Option<EndpointDeclaration>,
}

impl ExtensionEndpoint {
    pub fn effective(&self) -> &EndpointDeclaration {
        self.user_override
            .as_ref()
            .unwrap_or(&self.extension_default)
    }

    pub fn with_user_override(
        mut self,
        endpoint: EndpointDeclaration,
    ) -> Result<Self, ExtensionRegistrationError> {
        if !matches!(endpoint.origin, EndpointOrigin::UserConfigured { .. }) {
            return Err(ExtensionRegistrationError::Invalid(
                "an endpoint override must have UserConfigured provenance".into(),
            ));
        }
        validate_endpoint(&endpoint)?;
        self.user_override = Some(endpoint);
        Ok(self)
    }
}

/// Runtime auth resolver supplied by the extension host. It returns a
/// one-call `ModelAuth`; the bridge never persists or serializes that value.
pub trait ExtensionAuthResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        provider_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<ModelAuth>, AuthError>>;
}

/// Adapter for PL-002. The broker sees only its existing narrow trait, while
/// the extension registry owns resolver identity and lifecycle.
#[derive(Clone, Default)]
pub struct ExtensionCredentialBridge {
    resolvers: Arc<RwLock<BTreeMap<String, Arc<dyn ExtensionAuthResolver>>>>,
}

impl ExtensionCredentialBridge {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &self,
        provider_id: impl Into<String>,
        resolver: Arc<dyn ExtensionAuthResolver>,
    ) {
        if let Ok(mut resolvers) = self.resolvers.write() {
            resolvers.insert(provider_id.into(), resolver);
        }
    }

    pub fn resolver_count(&self) -> usize {
        self.resolvers.read().map(|r| r.len()).unwrap_or(0)
    }
}

impl ExtensionCredentialResolver for ExtensionCredentialBridge {
    fn resolve<'a>(
        &'a self,
        provider_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<ModelAuth>, AuthError>> {
        let resolver = self
            .resolvers
            .read()
            .ok()
            .and_then(|resolvers| resolvers.get(provider_id).cloned());
        Box::pin(async move {
            match resolver {
                Some(resolver) => resolver.resolve(provider_id).await,
                None => Ok(None),
            }
        })
    }
}

pub trait ExtensionTransport: Send + Sync {
    fn stream<'a>(
        &'a self,
        request: ExtensionRequest,
        auth: ModelAuth,
    ) -> BoxFuture<'a, Result<ProviderResponseStream, ProviderError>>;
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExtensionRequest {
    pub provider_id: String,
    pub model_id: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionHealth {
    Healthy,
    Unavailable { detail: String },
}

pub trait ExtensionHealthProbe: Send + Sync {
    fn probe<'a>(
        &'a self,
        provider_id: &'a str,
        endpoint: &'a EndpointDeclaration,
    ) -> BoxFuture<'a, Result<ExtensionHealth, ProviderError>>;
}

pub struct ExtensionProviderRegistration {
    pub id: String,
    pub display_name: String,
    pub version: String,
    pub protocol: ProviderProtocol,
    pub endpoint: ExtensionEndpoint,
    pub auth: AuthResolverDeclaration,
    pub secrets: Vec<SecretDeclaration>,
    pub models: Vec<ExtensionModelDeclaration>,
    pub health_probe: HealthProbeDeclaration,
    pub requested_subscription_eligibility: SubscriptionEligibility,
    pub auth_resolver: Option<Arc<dyn ExtensionAuthResolver>>,
    pub transport: Option<Arc<dyn ExtensionTransport>>,
    pub health: Option<Arc<dyn ExtensionHealthProbe>>,
}

impl ExtensionProviderRegistration {
    pub fn from_document(
        document: ProviderExtensionDocument,
    ) -> Result<Self, ExtensionRegistrationError> {
        document.validate()?;
        let requested_subscription_eligibility =
            match document.requested_subscription_eligibility.as_deref() {
                None | Some("unknown") => SubscriptionEligibility::Unknown,
                Some("supported") => SubscriptionEligibility::Supported,
                Some("unsupported") => SubscriptionEligibility::Unsupported,
                Some(other) => {
                    return Err(ExtensionRegistrationError::Invalid(format!(
                        "unknown subscription eligibility `{other}`"
                    )))
                }
            };
        Ok(Self {
            id: document.id,
            display_name: document.display_name,
            version: document.version,
            protocol: document.protocol,
            endpoint: ExtensionEndpoint {
                extension_default: document.endpoint,
                user_override: None,
            },
            auth: document.auth,
            secrets: document.secrets,
            models: document.models,
            health_probe: document.health_probe,
            requested_subscription_eligibility,
            auth_resolver: None,
            transport: None,
            health: None,
        })
    }

    pub fn from_json(json: &str) -> Result<Self, ExtensionRegistrationError> {
        Self::from_document(ProviderExtensionDocument::from_json(json)?)
    }

    pub fn with_auth_resolver(mut self, resolver: Arc<dyn ExtensionAuthResolver>) -> Self {
        self.auth_resolver = Some(resolver);
        self
    }

    pub fn with_transport(mut self, transport: Arc<dyn ExtensionTransport>) -> Self {
        self.transport = Some(transport);
        self
    }

    pub fn with_health_probe(mut self, health: Arc<dyn ExtensionHealthProbe>) -> Self {
        self.health = Some(health);
        self
    }

    pub fn with_user_endpoint(
        mut self,
        endpoint: EndpointDeclaration,
    ) -> Result<Self, ExtensionRegistrationError> {
        self.endpoint = self.endpoint.clone().with_user_override(endpoint)?;
        Ok(self)
    }

    /// Invoke a custom transport without giving the transport ownership of
    /// the registration or of any persisted credential. Built-in protocol
    /// adapters continue to use the existing clients at the host call site.
    pub fn stream(
        &self,
        request: ExtensionRequest,
        auth: ModelAuth,
    ) -> BoxFuture<'_, Result<ProviderResponseStream, ProviderError>> {
        match &self.transport {
            Some(transport) => transport.stream(request, auth),
            None => Box::pin(async {
                Err(ProviderError::Provider(
                    "provider extension has no custom transport".into(),
                ))
            }),
        }
    }

    pub fn probe(&self) -> BoxFuture<'_, Result<ExtensionHealth, ProviderError>> {
        let endpoint = self.endpoint.effective().clone();
        let provider_id = self.id.clone();
        match &self.health {
            Some(health) => {
                let health = health.clone();
                Box::pin(async move { health.probe(&provider_id, &endpoint).await })
            }
            None => Box::pin(async {
                Err(ProviderError::Provider(
                    "provider extension has no health probe".into(),
                ))
            }),
        }
    }

    fn validate_runtime(&self) -> Result<(), ExtensionRegistrationError> {
        if self.endpoint.extension_default.origin
            != (EndpointOrigin::Extension {
                extension_id: self.id.clone(),
            })
        {
            return Err(ExtensionRegistrationError::Invalid(
                "extension default endpoint must retain Extension provenance".into(),
            ));
        }
        if matches!(self.protocol, ProviderProtocol::Custom { .. }) && self.transport.is_none() {
            return Err(ExtensionRegistrationError::Invalid(
                "custom protocol requires an ExtensionTransport".into(),
            ));
        }
        if matches!(self.health_probe.kind, HealthProbeKind::Extension) && self.health.is_none() {
            return Err(ExtensionRegistrationError::Invalid(
                "extension health probe requires an ExtensionHealthProbe".into(),
            ));
        }
        Ok(())
    }

    fn capability(
        &self,
        model: &ExtensionModelDeclaration,
    ) -> Result<ModelCapability, ExtensionRegistrationError> {
        let tier = match model.tier.as_str() {
            "cheap" => ModelTier::Cheap,
            "capable" => ModelTier::Capable,
            other => {
                return Err(ExtensionRegistrationError::InvalidTier {
                    provider: self.id.clone(),
                    tier: other.to_string(),
                })
            }
        };
        Ok(ModelCapability {
            slug: model.slug.clone(),
            prompt_per_m: model.prompt_per_m,
            completion_per_m: model.completion_per_m,
            context_window_tokens: model.context_window_tokens,
            compaction_trigger_percent: 70,
            compaction_reserve_tokens: 32_768,
            compaction_max_output_tokens: 20_000,
            tier,
            reasoning: model.reasoning,
            supported_efforts: None,
            tool_schema_class: model.tool_schema_class.clone(),
            vision: model.vision,
        })
    }

    fn registry_auth_mode(&self) -> AuthMode {
        match &self.auth {
            AuthResolverDeclaration::OAuth => AuthMode::OAuth,
            // Extension credentials are request-time API credentials from the
            // registry's perspective. The resolver itself remains distinct in
            // PL-002; this avoids pretending AuthMode has an extension variant.
            AuthResolverDeclaration::ApiKey { .. }
            | AuthResolverDeclaration::Ambient { .. }
            | AuthResolverDeclaration::Extension { .. } => AuthMode::ApiKey,
        }
    }

    /// Register this extension's routes and composed metadata. Existing
    /// built-in/configured routes are not overwritten. `policy` is where the
    /// host supplies explicit trust and catalog provenance.
    pub fn register_into(
        &self,
        routes: &mut ModelRegistry,
        metadata: &mut RegistryMeta,
        policy: ExtensionRegistrationPolicy,
    ) -> Result<(), ExtensionRegistrationError> {
        self.validate_runtime()?;
        if self.requested_subscription_eligibility == SubscriptionEligibility::Supported
            && policy.trust != ExtensionTrust::ExplicitlyGranted
        {
            return Err(ExtensionRegistrationError::TrustRequired {
                provider: self.id.clone(),
            });
        }
        for model in &self.models {
            if routes.get(&model.slug).is_some() || metadata.get(&model.slug).is_some() {
                return Err(ExtensionRegistrationError::RouteCollision(
                    model.slug.clone(),
                ));
            }
        }
        // Build every row before mutating either registry. A malformed later
        // model must not leave a partially registered extension behind.
        let endpoint = self.endpoint.effective();
        let mut rows = Vec::with_capacity(self.models.len());
        for model in &self.models {
            let route = ModelRoute::new(
                model.slug.clone(),
                self.id.clone(),
                endpoint.base_url.clone(),
                model.id.clone(),
                model.priority,
            );
            let capability = self.capability(model)?;
            let entry = from_route_and_capability(
                &route,
                &capability,
                vec![self.registry_auth_mode()],
                self.requested_subscription_eligibility,
                policy.availability.clone(),
                model.tools,
                model.streaming,
                model.prompt_caching,
                model.usage_reporting,
                policy.catalog_source,
                policy.freshness.clone(),
            );
            rows.push((route, entry));
        }
        for (route, entry) in rows {
            routes.register(route);
            metadata.register(entry);
        }
        Ok(())
    }
}

/// Registry-owned extension set. It is separate from built-ins and carries a
/// credential bridge ready to be passed to `ProviderCredentialConfig::with_extension`.
#[derive(Default)]
pub struct ExtensionProviderRegistry {
    providers: BTreeMap<String, Arc<ExtensionProviderRegistration>>,
    credential_bridge: ExtensionCredentialBridge,
}

impl ExtensionProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &mut self,
        provider: ExtensionProviderRegistration,
        routes: &mut ModelRegistry,
        metadata: &mut RegistryMeta,
        policy: ExtensionRegistrationPolicy,
    ) -> Result<(), ExtensionRegistrationError> {
        if self.providers.contains_key(&provider.id) {
            return Err(ExtensionRegistrationError::DuplicateProvider(provider.id));
        }
        provider.register_into(routes, metadata, policy)?;
        if let Some(resolver) = &provider.auth_resolver {
            self.credential_bridge
                .register(&provider.id, resolver.clone());
        }
        let id = provider.id.clone();
        self.providers.insert(id, Arc::new(provider));
        Ok(())
    }

    /// Parse and register one plugin-owned provider document through the
    /// same collision, trust, catalog, and credential-bridge path as an
    /// in-process registration.
    pub fn register_json(
        &mut self,
        json: &str,
        routes: &mut ModelRegistry,
        metadata: &mut RegistryMeta,
        policy: ExtensionRegistrationPolicy,
    ) -> Result<(), ExtensionRegistrationError> {
        self.register(
            ExtensionProviderRegistration::from_json(json)?,
            routes,
            metadata,
            policy,
        )
    }

    pub fn get(&self, provider_id: &str) -> Option<&ExtensionProviderRegistration> {
        self.providers.get(provider_id).map(Arc::as_ref)
    }

    pub fn iter(&self) -> impl Iterator<Item = &ExtensionProviderRegistration> {
        self.providers.values().map(Arc::as_ref)
    }

    pub fn credential_resolver(&self) -> Arc<dyn ExtensionCredentialResolver> {
        Arc::new(self.credential_bridge.clone())
    }
}
