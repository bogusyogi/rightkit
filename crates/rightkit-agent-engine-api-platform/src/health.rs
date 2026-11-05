use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceQuery {
    pub workspace_root: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveSandboxSettingsRequest {
    pub tier: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxSettingsRecord {
    pub tier: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderKeyStorageStatus {
    pub keychain_failure_observed: bool,
}

/// The client-API contract version the daemon serves. A client accepts the daemon only when this
/// and [`PROTOCOL_VERSION`] sit inside its compiled range; the app `version` is displayed, never compared.
pub const CONTRACT_VERSION: u32 = 1;
/// The daemon discovery-record protocol version (`daemon.port` `ProtocolVersion`).
pub const PROTOCOL_VERSION: u16 = 1;

/// The unauthenticated `GET /health` body: exactly these five fields (client API R37). Everything
/// else (`port`, `databasePath`, storage pool, provider-key posture) is behind the bearer on
/// `/machine/info` and `/machine/status`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HealthResponse {
    pub ok: bool,
    pub service: String,
    pub version: String,
    pub contract_version: u32,
    pub protocol_version: u16,
}
