use rightkit_agent_orchestration::coding::registry::AgentRegistry;
use rightkit_app_settings::agent_config::HostConfig;

pub(crate) const ROLE_MODEL_MODE_SINGLE: &str = "single";
pub(crate) const ROLE_MODEL_MODE_SPLIT: &str = "split";
pub(crate) const ROLE_MODEL_SPLIT_FEATURE: &str = "role_model_split";
pub(crate) const PRO_FEATURE: &str = "pro";

pub(crate) fn split_entitled<F>(mut has_feature: F) -> bool
where
    F: FnMut(&str) -> bool,
{
    has_feature(ROLE_MODEL_SPLIT_FEATURE) || has_feature(PRO_FEATURE)
}

/// Re-exported so the daemon and the agent registry cannot disagree about who
/// serves a model. They used to: the registry hardcoded `openrouter` for every
/// agent while this function derived the real provider, so routing worked and
/// every UI reading `provider_id` lied.
pub(crate) fn provider_id_for_model_slug(slug: &str) -> &'static str {
    rightkit_agent_orchestration::coding::registry::provider_id_for_model_slug(slug)
}

pub(crate) fn role_model_mode(config: &HostConfig, split_entitled: bool) -> &'static str {
    let requested = config.orchestration.role_model_mode.trim();
    if requested.eq_ignore_ascii_case(ROLE_MODEL_MODE_SPLIT) && split_entitled {
        ROLE_MODEL_MODE_SPLIT
    } else {
        ROLE_MODEL_MODE_SINGLE
    }
}

pub(crate) fn uses_single_role_model(config: &HostConfig, split_entitled: bool) -> bool {
    role_model_mode(config, split_entitled) == ROLE_MODEL_MODE_SINGLE
}

pub(crate) fn resolve_configured_model_alias(configured: &str, registry: &AgentRegistry) -> String {
    let trimmed = configured.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        registry
            .get(trimmed)
            .map(|agent| agent.model_id.clone())
            .unwrap_or_else(|| trimmed.to_string())
    }
}

pub(crate) fn single_role_model_for_config(
    config: &HostConfig,
    registry: &AgentRegistry,
    engine_default_model: Option<&str>,
) -> Option<String> {
    let raw = config
        .orchestration
        .boss_model
        .trim()
        .to_string()
        .if_empty_then(|| config.model.default_model.trim().to_string())
        .if_empty_then(|| engine_default_model.unwrap_or_default().trim().to_string());
    let resolved = resolve_configured_model_alias(&raw, registry);
    (!resolved.trim().is_empty()).then(|| resolved.trim().to_string())
}

pub(crate) fn apply_effective_role_model_policy(
    registry: &mut AgentRegistry,
    config: &HostConfig,
    split_entitled: bool,
    engine_default_model: Option<&str>,
) {
    apply_effective_role_model_policy_with_override(
        registry,
        config,
        split_entitled,
        engine_default_model,
        None,
    );
}

pub(crate) fn apply_effective_role_model_policy_with_override(
    registry: &mut AgentRegistry,
    config: &HostConfig,
    split_entitled: bool,
    engine_default_model: Option<&str>,
    session_model_override: Option<&str>,
) {
    if !uses_single_role_model(config, split_entitled) {
        return;
    }
    let model_id = session_model_override
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
        .or_else(|| single_role_model_for_config(config, registry, engine_default_model));
    let Some(model_id) = model_id else {
        return;
    };
    let provider_id = provider_id_for_model_slug(&model_id);
    let descriptors = registry.all().to_vec();
    for mut descriptor in descriptors {
        descriptor.model_id = model_id.clone();
        descriptor.provider_id = provider_id.to_string();
        registry.register(descriptor);
    }
}

trait EmptyStringExt {
    fn if_empty_then<F>(self, fallback: F) -> String
    where
        F: FnOnce() -> String;
}

impl EmptyStringExt for String {
    fn if_empty_then<F>(self, fallback: F) -> String
    where
        F: FnOnce() -> String,
    {
        if self.trim().is_empty() {
            fallback()
        } else {
            self
        }
    }
}
