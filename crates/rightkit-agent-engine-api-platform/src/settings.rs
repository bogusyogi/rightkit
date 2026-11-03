use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderKeyStatus {
    pub provider: String,
    pub configured: bool,
    pub path: String,
    pub source: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigFileStatus {
    pub scope: String,
    pub path: String,
    pub exists: bool,
    pub modified_ms: Option<u128>,
    pub bytes: Option<u64>,
    pub sha256: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigStatus {
    pub workspace_root: String,
    pub fingerprint: String,
    pub loaded_at: String,
    pub files: Vec<ConfigFileStatus>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsetSettingsProfile {
    pub id: String,
    pub name: String,
    pub description: String,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub read_only: bool,
    pub default_behavioral_skills: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveToolsetConfig {
    pub name: Option<String>,
    pub description: Option<String>,
    pub allowed_tools: Option<Vec<String>>,
    pub disallowed_tools: Option<Vec<String>>,
    pub read_only: Option<bool>,
    pub default_behavioral_skills: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveProviderKey {
    #[serde(alias = "api_key")]
    pub api_key: String,
}

#[derive(Deserialize)]
pub struct SaveDirectInferenceSecret {
    pub secret: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct SaveSettings {
    pub workspace_root: Option<String>,
    pub primary_orchestrator_model_id: Option<String>,
    pub role_model_mode: Option<String>,
    pub model_overrides: Option<std::collections::HashMap<String, String>>,
    pub budget: Option<serde_json::Value>,
    pub benchmarks: Option<serde_json::Value>,
    pub remote: Option<SaveRemoteSettings>,
    pub routing: Option<SaveRoutingSettings>,
    #[serde(default)]
    pub keep_daemon_resident: Option<bool>,
    pub orchestration: Option<SaveOrchestrationSettings>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct SaveRoutingSettings {
    pub capability_router: bool,
    pub aggressiveness: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct SaveRemoteSettings {
    pub telegram_token: Option<String>,
    pub clear_telegram_token: Option<bool>,
    pub telegram_allowed_users: Option<Vec<u64>>,
    pub web_enabled: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct SaveOrchestrationSettings {
    pub conductor_routing: bool,
}
