pub(crate) use rightkit_agent_engine_api_platform::settings::{
    ConfigFileStatus, ConfigStatus, ProviderKeyStatus, SaveProviderKey, SaveSettings,
    SaveToolsetConfig, ToolsetSettingsProfile,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveDirectInferenceConnection {
    name: String,
    protocol: crate::inference_runtime::InferenceProtocol,
    normalized_base_url: String,
    auth_kind: crate::inference_runtime::DirectAuthKind,
    auth_header_name: Option<String>,
    #[serde(default)]
    public_headers: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    manual_model_ids: Vec<String>,
    #[serde(default)]
    trust_private_network: bool,
}

pub(crate) use rightkit_agent_engine_api_platform::settings::SaveDirectInferenceSecret;
