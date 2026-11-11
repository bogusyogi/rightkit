use super::*;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSession {
    pub id: Option<String>,
    pub role: Option<String>,
    #[serde(default)]
    pub workspace_root: String,
    #[serde(default = "default_title")]
    pub title: String,
    pub model: Option<String>,
    /// Explicit provider route pin. Mutually exclusive with
    /// `inferenceSelection` (which carries its own connection authority);
    /// absent means the daemon resolves configured defaults/routes.
    pub provider_id: Option<String>,
    pub inference_selection: Option<crate::inference_runtime::InferenceSelection>,
    pub parent_session_id: Option<String>,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub disallowed_tools: Vec<String>,
    #[serde(default)]
    pub active_skills: Vec<String>,
    pub toolset: Option<String>,
    pub execution_backend: Option<String>,
    pub execution_workspace_id: Option<String>,
    pub driver_kind: Option<String>,
    pub mcp_channel: Option<String>,
    pub spec_id: Option<String>,
    pub effort: Option<String>,
    pub thinking_budget_tokens: Option<u32>,
    pub disable_thinking: Option<bool>,
    #[serde(default)]
    pub safe_mode: bool,
    /// Fable max-effort mode: force high effort + inject the constraint-rich directive.
    #[serde(default)]
    pub fable_mode: Option<bool>,
    /// Override for the brief-default system prompt directive.
    #[serde(default)]
    pub brief_default: Option<bool>,
    /// Permission mode for this session, overriding the workspace config.
    ///
    /// Needed for unattended callers. The mode otherwise comes only from a
    /// config file on disk, and the default (`ask`) fails closed when nothing
    /// is subscribed to answer — so a headless run could not perform a single
    /// gated action without the caller first editing the user's config.
    /// Accepts the same spellings as the config key: `plan`, `ask`,
    /// `accept-edits`, `auto-safe`, `bypass`.
    pub permission_mode: Option<String>,
    /// CLI run limits and prompt/runtime overrides.
    #[serde(default)]
    pub max_iterations: Option<u32>,
    #[serde(default)]
    pub budget_ceiling_usd: Option<f64>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub append_system_prompt: Option<String>,
    #[serde(default)]
    pub bare: bool,
    #[serde(default)]
    pub no_mcp: bool,
    #[serde(default)]
    pub add_dir: Vec<String>,
    #[serde(default)]
    pub mcp_config: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BindSessionSpecRequest {
    pub spec_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AwaySummaryQuery {
    pub since_index: Option<usize>,
    pub limit: Option<usize>,
}

fn default_title() -> String {
    "New session".into()
}

pub(crate) fn normalize_mcp_channel(channel: Option<String>) -> String {
    let normalized = channel
        .as_deref()
        .unwrap_or("desktop")
        .trim()
        .to_ascii_lowercase();
    if normalized.is_empty() {
        "desktop".to_string()
    } else {
        normalized
    }
}

pub(crate) fn normalize_driver_kind(kind: Option<String>) -> Result<String, String> {
    let normalized = kind
        .as_deref()
        .unwrap_or("native")
        .trim()
        .to_ascii_lowercase();
    match normalized.as_str() {
        "native" | "codex" | "claude" | "grok" | "cursor" | "cline" | "commandcode" => {
            Ok(normalized)
        }
        _ => Err("driverKind must name native or a registered CLI provider".to_string()),
    }
}

pub(crate) fn normalize_effort(effort: Option<String>) -> Result<Option<String>, String> {
    let Some(raw) = effort else {
        return Ok(None);
    };
    let normalized = raw.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Ok(None);
    }
    match normalized.as_str() {
        "low" | "medium" | "high" => Ok(Some(normalized)),
        _ => Err(format!(
            "Invalid effort '{raw}'. Expected one of: low, medium, high."
        )),
    }
}

pub(crate) fn normalize_thinking_budget(tokens: Option<u32>) -> Result<Option<u32>, String> {
    match tokens {
        Some(0) => Err("thinkingBudgetTokens must be greater than zero".to_string()),
        Some(tokens) => Ok(Some(tokens)),
        None => Ok(None),
    }
}

/// Canonical separator form for paths crossing the JSON/API boundary.
/// Filesystem operations keep the host-native path; public payloads do not.
pub(crate) fn api_workspace_root(workspace_root: &str) -> String {
    workspace_root.trim().replace('\\', "/")
}

pub(crate) fn workspace_trust_key(workspace_root: &str) -> String {
    api_workspace_root(workspace_root)
}

pub(crate) fn workspace_trust_label(trust: rightkit_agent_policy::WorkspaceTrust) -> &'static str {
    match trust {
        rightkit_agent_policy::WorkspaceTrust::Untrusted => "untrusted",
        rightkit_agent_policy::WorkspaceTrust::Trusted => "trusted",
        rightkit_agent_policy::WorkspaceTrust::Sandboxed => "sandboxed",
        rightkit_agent_policy::WorkspaceTrust::Locked => "locked",
    }
}

pub(crate) fn parse_workspace_trust_label(
    raw: &str,
) -> Option<rightkit_agent_policy::WorkspaceTrust> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "untrusted" => Some(rightkit_agent_policy::WorkspaceTrust::Untrusted),
        "trusted" => Some(rightkit_agent_policy::WorkspaceTrust::Trusted),
        "sandboxed" => Some(rightkit_agent_policy::WorkspaceTrust::Sandboxed),
        "locked" => Some(rightkit_agent_policy::WorkspaceTrust::Locked),
        _ => None,
    }
}

pub(crate) fn merge_active_skill_ids(
    defaults: Option<&[String]>,
    explicit: &[String],
) -> Vec<String> {
    let mut merged = Vec::new();
    for skill in defaults.unwrap_or(&[]).iter().chain(explicit.iter()) {
        let skill = skill.trim();
        if !skill.is_empty() && !merged.iter().any(|existing| existing == skill) {
            merged.push(skill.to_string());
        }
    }
    merged
}

pub(crate) fn validate_execution_backend(
    registry: &rightkit_sandbox::execution::ExecutionBackendRegistry,
    requested: Option<&str>,
) -> Result<Option<String>, StatusCode> {
    let Some(backend) = requested.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if registry.get(backend).is_none() {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(Some(backend.to_string()))
}

pub(crate) fn effective_tool_filter(
    role_allowed_tools: Option<&Vec<String>>,
    requested_allowed_tools: &[String],
    requested_disallowed_tools: &[String],
) -> Option<Vec<String>> {
    let mut selected = if let Some(role_allowed) = role_allowed_tools {
        role_allowed
            .iter()
            .filter(|tool| {
                requested_allowed_tools.is_empty()
                    || requested_allowed_tools
                        .iter()
                        .any(|allowed| allowed == *tool)
            })
            .cloned()
            .collect::<Vec<_>>()
    } else if !requested_allowed_tools.is_empty() {
        requested_allowed_tools.to_vec()
    } else {
        return None;
    };

    if !requested_disallowed_tools.is_empty() {
        selected.retain(|tool| {
            !requested_disallowed_tools
                .iter()
                .any(|denied| denied == tool)
        });
    }
    selected.sort();
    selected.dedup();
    Some(selected)
}

pub(crate) fn build_session_tool_registry_for_plugins(
    request: crate::session_tool_catalog::SessionToolRegistryRequest<'_>,
) -> rightkit_agent_tools::coding_tools::ToolRegistry {
    crate::session_tool_catalog::build_session_tool_registry_for_plugins(request)
}

pub(crate) fn load_workspace_config_with_env(
    workspace: &FsPath,
) -> rightkit_agent_config::HostConfig {
    let loader = rightkit_agent_config::ConfigLoader::new();
    let base = loader.load_files_for_workspace(workspace);
    let env_vars: HashMap<String, String> = std::env::vars().collect();
    loader.apply_env(base, &env_vars)
}

pub(crate) fn web_search_config_from_config(
    config: &rightkit_agent_config::HostConfig,
) -> Option<rightkit_agent_tools::coding_tools::WebSearchConfig> {
    let provider = config
        .tools
        .web_search_provider
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let tinyfish_available = rightkit_llm::agent_providers::load_api_key("tinyfish")
                .is_some()
                || std::env::var("TINYFISH_API_KEY").is_ok();
            tinyfish_available.then(|| "tinyfish".to_string())
        })?;
    let api_key = config
        .tools
        .web_search_api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    if provider != "tinyfish" && api_key.is_empty() {
        return None;
    }
    Some(rightkit_agent_tools::coding_tools::WebSearchConfig {
        provider,
        api_key: api_key.to_string(),
    })
}
