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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_response_serialises_camel_case() {
        let response = HealthResponse {
            ok: true,
            service: "engine".into(),
            version: "1.2.3".into(),
            contract_version: CONTRACT_VERSION,
            protocol_version: PROTOCOL_VERSION,
        };
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["contractVersion"], CONTRACT_VERSION);
        assert_eq!(value["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(value.as_object().unwrap().len(), 5);
        let back: HealthResponse = serde_json::from_value(value).unwrap();
        assert_eq!(back.version, "1.2.3");
    }

    #[test]
    fn health_response_refuses_unknown_fields() {
        let body = serde_json::json!({
            "ok": true,
            "service": "engine",
            "version": "1.2.3",
            "contractVersion": 1,
            "protocolVersion": 1,
            "databasePath": "/tmp/db"
        });
        assert!(serde_json::from_value::<HealthResponse>(body).is_err());
    }
}
