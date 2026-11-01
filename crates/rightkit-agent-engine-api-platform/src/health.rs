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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_response_serialises_camel_case() {
        let response = HealthResponse {
            ok: true,
            service: "engine".into(),
            port: 8080,
            database_path: "/tmp/db".into(),
            utc_now: "2026-01-01T00:00:00Z".into(),
            storage_read_pool_idle: None,
            provider_key_storage: ProviderKeyStorageStatus {
                keychain_failure_observed: false,
            },
        };
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["databasePath"], "/tmp/db");
        assert!(value["storageReadPoolIdle"].is_null());
        assert_eq!(
            value["providerKeyStorage"]["keychainFailureObserved"],
            false
        );
        let back: HealthResponse = serde_json::from_value(value).unwrap();
        assert_eq!(back.port, 8080);
    }
}
