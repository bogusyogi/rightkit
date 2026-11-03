//! Pi custom-provider/model extension boundary, ported to Rust.
//!
//! Donor lineage: Pi `packages/ai/src/models.ts` (`Provider`/`Model`) and
//! `packages/ai/src/providers/*` (provider protocol/auth declarations). This
//! module is the plugin-side, serializable contract only. The provider crate
//! has the runtime registration adapter because keeping the plugin crate
//! independent of provider transport and credential code is intentional.
//!
//! A provider extension can describe identity, protocol, endpoint, auth,
//! models, health and secret declarations without placing a secret in the
//! manifest. It does not receive core trust merely by being installed or
//! enabled. The daemon must apply its own policy before registration.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const PROVIDER_EXTENSION_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderExtensionError {
    #[error("provider extension manifest is invalid: {0}")]
    Invalid(String),
    #[error("provider extension schema version {0} is unsupported")]
    UnsupportedSchema(u16),
}

/// Wire protocol family understood by the native provider adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProtocol {
    OpenAiCompatible,
    Anthropic,
    /// The plugin owns request/response translation through its runtime
    /// transport. The string is a stable diagnostic identifier, not code.
    Custom {
        name: String,
    },
}

/// Provenance is retained instead of collapsing built-in, extension and
/// operator-configured endpoints into one URL string.
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

/// Auth is a declaration, never a credential. Actual bytes resolve through
/// PL-002's `ExtensionCredentialResolver` seam in the provider crate.
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
    /// A label for policy/UI; it is not a value or a lookup result.
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
pub struct ProviderModelDeclaration {
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

/// Serializable provider extension descriptor. This is deliberately separate
/// from `PluginManifest`: a plugin may expose tools without being a provider,
/// and provider registration must not infer network or secret authority from
/// tool metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderExtensionManifest {
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
    pub models: Vec<ProviderModelDeclaration>,
    pub health_probe: HealthProbeDeclaration,
    /// Extensions start untrusted. This field is descriptive and cannot grant
    /// trust; the host supplies an explicit policy/grant at registration.
    #[serde(default)]
    pub requested_subscription_eligibility: Option<String>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

impl ProviderExtensionManifest {
    pub fn from_json(json: &str) -> Result<Self, ProviderExtensionError> {
        let manifest: Self = serde_json::from_str(json)
            .map_err(|error| ProviderExtensionError::Invalid(error.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn to_json(&self) -> Result<String, ProviderExtensionError> {
        self.validate()?;
        serde_json::to_string_pretty(self)
            .map_err(|error| ProviderExtensionError::Invalid(error.to_string()))
    }

    pub fn validate(&self) -> Result<(), ProviderExtensionError> {
        if self.schema_version != PROVIDER_EXTENSION_SCHEMA_VERSION {
            return Err(ProviderExtensionError::UnsupportedSchema(
                self.schema_version,
            ));
        }
        if !valid_id(&self.id) {
            return Err(ProviderExtensionError::Invalid(
                "id must be a non-empty provider-safe identifier".into(),
            ));
        }
        if self.display_name.trim().is_empty() || self.version.trim().is_empty() {
            return Err(ProviderExtensionError::Invalid(
                "display_name and version are required".into(),
            ));
        }
        if !valid_url(&self.endpoint.base_url) {
            return Err(ProviderExtensionError::Invalid(
                "endpoint base_url must be an absolute http(s) URL".into(),
            ));
        }
        if self.endpoint.origin
            != (EndpointOrigin::Extension {
                extension_id: self.id.clone(),
            })
        {
            return Err(ProviderExtensionError::Invalid(
                "provider manifest endpoint must retain Extension provenance".into(),
            ));
        }
        for name in self.endpoint.headers.keys() {
            let lower = name.to_ascii_lowercase();
            if lower == "authorization" || lower.contains("api-key") || lower.contains("token") {
                return Err(ProviderExtensionError::Invalid(format!(
                    "secret-bearing header `{name}` must be resolved at request time"
                )));
            }
        }
        if self.models.is_empty() {
            return Err(ProviderExtensionError::Invalid(
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
                || model.tool_schema_class.trim().is_empty()
            {
                return Err(ProviderExtensionError::Invalid(format!(
                    "invalid model declaration `{}`",
                    model.id
                )));
            }
            if !ids.insert(&model.id) {
                return Err(ProviderExtensionError::Invalid(format!(
                    "duplicate model id `{}`",
                    model.id
                )));
            }
            if model.context_window_tokens == 0
                || !model.prompt_per_m.is_finite()
                || !model.completion_per_m.is_finite()
                || model.prompt_per_m < 0.0
                || model.completion_per_m < 0.0
            {
                return Err(ProviderExtensionError::Invalid(format!(
                    "invalid capability metadata for model `{}`",
                    model.id
                )));
            }
        }
        for secret in &self.secrets {
            if secret.name.trim().is_empty() {
                return Err(ProviderExtensionError::Invalid(
                    "secret declarations require a name".into(),
                ));
            }
        }
        Ok(())
    }
}

fn default_priority() -> u8 {
    100
}

fn valid_id(value: &str) -> bool {
    !value.trim().is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

fn valid_url(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://"))
        && !value.chars().any(char::is_whitespace)
}
