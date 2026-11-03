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

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthResponse {
    pub ok: bool,
    pub service: String,
    pub port: u16,
    pub database_path: String,
    pub utc_now: String,
    pub storage_read_pool_idle: Option<usize>,
    /// Non-secret provider credential-store posture. Plaintext fallback is
    /// never implicit; when enabled or used this object makes the degraded
    /// state visible to every daemon client.
    pub provider_key_storage: ProviderKeyStorageStatus,
}
