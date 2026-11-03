use serde::{Deserialize, Serialize};

pub const INFERENCE_SELECTION_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceAuthority {
    Direct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceProtocol {
    OpenaiCompatible,
    AnthropicCompatible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InferenceSelection {
    pub schema_version: u8,
    pub authority: InferenceAuthority,
    pub connection_id: String,
    pub protocol: InferenceProtocol,
    pub model_id: String,
    pub route_policy_id: Option<String>,
}

impl InferenceSelection {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != INFERENCE_SELECTION_SCHEMA_VERSION {
            return Err("invalid_selection_schema");
        }
        if self.connection_id.trim().is_empty() || self.model_id.trim().is_empty() {
            return Err("invalid_selection");
        }
        match self.authority {
            InferenceAuthority::Direct => {
                if self.route_policy_id.is_some() {
                    return Err("route_policy_invalid");
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogModel {
    pub id: String,
    pub object: Option<String>,
    pub created: Option<i64>,
    pub owned_by: Option<String>,
    pub root: Option<String>,
    pub parent: Option<String>,
    pub permission: serde_json::Value,
    pub capabilities: Vec<String>,
    pub context_length: Option<u64>,
    pub max_input_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub selectable: bool,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogGroup {
    pub authority: InferenceAuthority,
    pub connection_id: String,
    pub name: String,
    pub protocol: InferenceProtocol,
    pub state: CatalogState,
    pub models: Vec<CatalogModel>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogState {
    Ready,
    NeedsSecret,
    Unreachable,
    Unqualified,
    Invalid,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadSnapshot {
    pub queued_agents: usize,
    pub active_agents: usize,
    pub active_limit: usize,
    pub per_connection_active: std::collections::BTreeMap<String, usize>,
    pub per_route_active: std::collections::BTreeMap<String, usize>,
    pub buffered_events: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectAuthKind {
    None,
    Bearer,
    XApiKey,
    ApiKeyHeader,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectConnection {
    pub id: String,
    pub name: String,
    pub protocol: InferenceProtocol,
    pub normalized_base_url: String,
    pub auth_kind: DirectAuthKind,
    pub auth_header_name: Option<String>,
    pub public_headers: std::collections::BTreeMap<String, String>,
    pub manual_model_ids: Vec<String>,
    pub trust_private_network: bool,
    pub credential_configured: bool,
    pub credential_revision: u64,
    pub configuration_revision: u64,
    pub created_at: String,
    pub updated_at: String,
}

/// Qualified runtime route. This is intentionally non-serializable: endpoint
/// authentication is process memory only and its Debug output is redacted.
#[derive(Clone)]
pub struct ResolvedInferenceRoute {
    pub selection: InferenceSelection,
    pub fingerprint: String,
    pub endpoint: String,
    pub auth_header_name: Option<String>,
    pub auth_secret: Option<String>,
    pub public_headers: std::collections::BTreeMap<String, String>,
    pub direct_connection: Option<DirectConnection>,
    pub attestation_id: String,
}

impl std::fmt::Debug for ResolvedInferenceRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedInferenceRoute")
            .field("selection", &self.selection)
            .field("fingerprint", &self.fingerprint)
            .field("endpoint", &self.endpoint)
            .field("auth_header_name", &self.auth_header_name)
            .field(
                "auth_secret",
                &self.auth_secret.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "public_header_names",
                &self.public_headers.keys().collect::<Vec<_>>(),
            )
            .field("direct_connection", &self.direct_connection.is_some())
            .field("attestation_id", &self.attestation_id)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InferenceRouteError {
    InvalidSelection,
    ConnectionNotFound,
    ModelUnavailable,
    SecretRequired,
    AuthorityUnavailable,
    AttestationRequired,
}

impl InferenceRouteError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidSelection => "invalid_connection",
            Self::ConnectionNotFound => "connection_not_found",
            Self::ModelUnavailable => "model_unavailable",
            Self::SecretRequired => "secret_required",
            Self::AuthorityUnavailable => "authority_unavailable",
            Self::AttestationRequired => "attestation_required",
        }
    }
}

impl std::fmt::Display for InferenceRouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for InferenceRouteError {}
