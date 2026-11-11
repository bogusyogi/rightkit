//! Layered merge logic for [`CodeRightConfig`].

use crate::types::{
    BudgetConfig, FeatureConfig, HostConfig, MemoryConfig, ModelConfig, OrchestrationConfig,
    PermissionsConfig, PromptConfig, RemoteConfig, SessionConfig, ToolsConfig, TuiConfig,
};
use std::collections::HashMap;

/// Extend `base` with the items from `overlay`, deduplicating while
/// preserving the first-seen order.
pub fn extend_dedup<T: PartialEq + Clone>(base: Vec<T>, overlay: Vec<T>) -> Vec<T> {
    let mut out: Vec<T> = Vec::with_capacity(base.len() + overlay.len());
    for item in base.into_iter().chain(overlay) {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

/// Pick the overlay scalar if it differs from the type default, otherwise
/// keep the base value.
///
/// This treats "equals the type default" as "unset", which is a safe proxy for
/// strings and numbers but NOT for `bool`: `false` is both a legitimate value
/// and `bool::default()`, so an overlay can turn a flag on but never off. Use
/// `Option<bool>` plus [`option_scalar`] for flags instead — see
/// [`crate::ToolsConfig::lazy_tool_loading`].
fn scalar<T: PartialEq + Default>(base: T, overlay: T) -> T {
    if overlay != T::default() {
        overlay
    } else {
        base
    }
}

/// For `Option` scalars: `Some` from overlay wins, `None` keeps base.
fn option_scalar<T>(base: Option<T>, overlay: Option<T>) -> Option<T> {
    match overlay {
        Some(v) => Some(v),
        None => base,
    }
}

/// Merge two configs, with `overlay` taking precedence over `base`.
///
/// Scalar fields: a non-default overlay value replaces the base value.
/// `Option` fields: `Some` in overlay wins; `None` keeps base.
/// List fields: base list extended with overlay items, deduplicated.
pub fn merge(base: HostConfig, overlay: HostConfig) -> HostConfig {
    HostConfig {
        model: merge_model(base.model, overlay.model),
        provider_routes: merge_map_replace(base.provider_routes, overlay.provider_routes),
        session: merge_session(base.session, overlay.session),
        permissions: merge_permissions(base.permissions, overlay.permissions),
        tools: merge_tools(base.tools, overlay.tools),
        features: merge_features(base.features, overlay.features),
        toolsets: merge_map_replace(base.toolsets, overlay.toolsets),
        channel_profiles: merge_map_replace(base.channel_profiles, overlay.channel_profiles),
        remote: merge_remote(base.remote, overlay.remote),
        orchestration: merge_orchestration(base.orchestration, overlay.orchestration),
        prompt: merge_prompt(base.prompt, overlay.prompt),
        memory: merge_memory(base.memory, overlay.memory),
        tui: merge_tui(base.tui, overlay.tui),
    }
}

fn merge_tui(base: TuiConfig, overlay: TuiConfig) -> TuiConfig {
    let mut keymap = base.keymap;
    keymap.extend(overlay.keymap);
    TuiConfig {
        theme: scalar(base.theme, overlay.theme),
        notify: scalar(base.notify, overlay.notify),
        vim: option_scalar(base.vim, overlay.vim),
        fullscreen: option_scalar(base.fullscreen, overlay.fullscreen),
        copy_on_select: option_scalar(base.copy_on_select, overlay.copy_on_select),
        always_copy_full_response: option_scalar(
            base.always_copy_full_response,
            overlay.always_copy_full_response,
        ),
        agents_view_default: option_scalar(base.agents_view_default, overlay.agents_view_default),
        statusline: if overlay.statusline.is_empty() {
            base.statusline
        } else {
            overlay.statusline
        },
        keymap,
    }
}

fn merge_map_replace<T>(
    mut base: HashMap<String, T>,
    overlay: HashMap<String, T>,
) -> HashMap<String, T> {
    for (key, value) in overlay {
        base.insert(key, value);
    }
    base
}

fn merge_model(base: ModelConfig, overlay: ModelConfig) -> ModelConfig {
    ModelConfig {
        default_model: scalar(base.default_model, overlay.default_model),
        fallback_models: extend_dedup(base.fallback_models, overlay.fallback_models),
        max_tokens: scalar(base.max_tokens, overlay.max_tokens),
        temperature: scalar(base.temperature, overlay.temperature),
    }
}

fn merge_session(base: SessionConfig, overlay: SessionConfig) -> SessionConfig {
    SessionConfig {
        allowed_tools: extend_dedup(base.allowed_tools, overlay.allowed_tools),
        disallowed_tools: extend_dedup(base.disallowed_tools, overlay.disallowed_tools),
        effort: scalar(base.effort, overlay.effort),
        thinking_budget_tokens: option_scalar(
            base.thinking_budget_tokens,
            overlay.thinking_budget_tokens,
        ),
        disable_thinking: option_scalar(base.disable_thinking, overlay.disable_thinking),
    }
}

fn merge_permissions(base: PermissionsConfig, overlay: PermissionsConfig) -> PermissionsConfig {
    PermissionsConfig {
        mode: scalar(base.mode, overlay.mode),
        deny_read_globs: extend_dedup(base.deny_read_globs, overlay.deny_read_globs),
        deny_write_globs: extend_dedup(base.deny_write_globs, overlay.deny_write_globs),
        abac_yaml: option_scalar(base.abac_yaml, overlay.abac_yaml),
    }
}

fn merge_tools(base: ToolsConfig, overlay: ToolsConfig) -> ToolsConfig {
    ToolsConfig {
        web_search_provider: option_scalar(base.web_search_provider, overlay.web_search_provider),
        web_search_api_key: option_scalar(base.web_search_api_key, overlay.web_search_api_key),
        lazy_tool_loading: option_scalar(base.lazy_tool_loading, overlay.lazy_tool_loading),
        model_interface: option_scalar(base.model_interface, overlay.model_interface),
    }
}

fn merge_features(base: FeatureConfig, overlay: FeatureConfig) -> FeatureConfig {
    FeatureConfig {
        cli_acquisition_v1: option_scalar(base.cli_acquisition_v1, overlay.cli_acquisition_v1),
        workflows_enabled: option_scalar(base.workflows_enabled, overlay.workflows_enabled),
        turn_checkpoints: option_scalar(base.turn_checkpoints, overlay.turn_checkpoints),
        auto_commit_on_exit: option_scalar(base.auto_commit_on_exit, overlay.auto_commit_on_exit),
        workflow_import_trusted_public_keys: extend_dedup(
            base.workflow_import_trusted_public_keys,
            overlay.workflow_import_trusted_public_keys,
        ),
    }
}

fn merge_remote(base: RemoteConfig, overlay: RemoteConfig) -> RemoteConfig {
    RemoteConfig {
        telegram_token: option_scalar(base.telegram_token, overlay.telegram_token),
        telegram_allowed_users: extend_dedup(
            base.telegram_allowed_users,
            overlay.telegram_allowed_users,
        ),
        web_enabled: option_scalar(base.web_enabled, overlay.web_enabled),
    }
}

fn merge_orchestration(
    base: OrchestrationConfig,
    overlay: OrchestrationConfig,
) -> OrchestrationConfig {
    let mut agents = base.agents;
    // Overlay entries replace base entries for the same agent name.
    for (name, ov) in overlay.agents {
        agents.insert(name, ov);
    }
    // The overlay (env/user layer) wins for budget only when it was explicitly
    // set to something other than the defaults; otherwise keep the base.
    let budget = if overlay.budget == BudgetConfig::default() {
        base.budget
    } else {
        overlay.budget
    };
    // The overlay's checks replace the base's when it declares any.
    let pre_gate_checks = if overlay.pre_gate_checks.is_empty() {
        base.pre_gate_checks
    } else {
        overlay.pre_gate_checks
    };
    OrchestrationConfig {
        boss_model: scalar(base.boss_model, overlay.boss_model),
        role_model_mode: scalar(base.role_model_mode, overlay.role_model_mode),
        agents,
        budget,
        pre_gate_checks,
        conductor_routing: option_scalar(base.conductor_routing, overlay.conductor_routing),
        eval_hack_advisory: option_scalar(base.eval_hack_advisory, overlay.eval_hack_advisory),
        routing: crate::types::RoutingConfig {
            capability_router: option_scalar(
                base.routing.capability_router,
                overlay.routing.capability_router,
            ),
            aggressiveness: if overlay.routing.aggressiveness.trim().is_empty() {
                base.routing.aggressiveness.clone()
            } else {
                overlay.routing.aggressiveness.clone()
            },
        },
    }
}

fn merge_prompt(base: PromptConfig, overlay: PromptConfig) -> PromptConfig {
    PromptConfig {
        brief_default: option_scalar(base.brief_default, overlay.brief_default),
    }
}

fn merge_memory(base: MemoryConfig, overlay: MemoryConfig) -> MemoryConfig {
    MemoryConfig {
        retrieval_tier: scalar(base.retrieval_tier, overlay.retrieval_tier),
        bandit_enabled: option_scalar(base.bandit_enabled, overlay.bandit_enabled),
        bandit_epsilon: option_scalar(base.bandit_epsilon, overlay.bandit_epsilon),
        bandit_alpha: option_scalar(base.bandit_alpha, overlay.bandit_alpha),
    }
}

/// Provenance for a merge conflict — which layers contributed which value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeConflict {
    pub field: String,
    pub base_value: String,
    pub overlay_value: String,
    pub resolved_value: String,
    pub base_source: String,
    pub overlay_source: String,
}

/// Report produced by a provenance-aware merge.
#[derive(Debug, Default, Clone)]
pub struct MergeReport {
    pub conflicts: Vec<MergeConflict>,
    pub provenance: HashMap<String, crate::types::Provenance>,
}

impl MergeReport {
    pub fn has_conflicts(&self) -> bool {
        !self.conflicts.is_empty()
    }

    pub fn conflict_for(&self, field: &str) -> Option<&MergeConflict> {
        self.conflicts.iter().find(|c| c.field == field)
    }
}

/// Typed merge trait — every config section implements governed merging.
pub trait TypedMerge: Sized {
    fn merge_typed(base: Self, overlay: Self) -> Self;
    fn merge_with_report(
        base: Self,
        overlay: Self,
        base_source: &str,
        overlay_source: &str,
    ) -> (Self, Vec<MergeConflict>);
}

/// Validate a config after merging — fail-closed on any bounded-domain violation.
pub fn validate_config(
    config: &crate::types::HostConfig,
) -> Result<(), Vec<crate::types::ValidationError>> {
    config.validate()
}

/// Provenance-aware merge that tracks which layer won for every field.
pub fn merge_with_report(
    base: HostConfig,
    overlay: HostConfig,
    base_source: &str,
    overlay_source: &str,
) -> (HostConfig, MergeReport) {
    let mut report = MergeReport::default();

    // Track boolean fields where both layers set a value — that's a conflict
    // even though overlay wins (provenance matters for auditing).
    let bool_fields = [
        (
            "session.disable_thinking",
            &base.session.disable_thinking,
            &overlay.session.disable_thinking,
        ),
        (
            "remote.web_enabled",
            &base.remote.web_enabled,
            &overlay.remote.web_enabled,
        ),
        (
            "orchestration.conductor_routing",
            &base.orchestration.conductor_routing,
            &overlay.orchestration.conductor_routing,
        ),
        (
            "orchestration.routing.capability_router",
            &base.orchestration.routing.capability_router,
            &overlay.orchestration.routing.capability_router,
        ),
        (
            "tools.lazy_tool_loading",
            &base.tools.lazy_tool_loading,
            &overlay.tools.lazy_tool_loading,
        ),
        (
            "features.workflows_enabled",
            &base.features.workflows_enabled,
            &overlay.features.workflows_enabled,
        ),
        (
            "prompt.brief_default",
            &base.prompt.brief_default,
            &overlay.prompt.brief_default,
        ),
        (
            "memory.bandit_enabled",
            &base.memory.bandit_enabled,
            &overlay.memory.bandit_enabled,
        ),
    ];
    for (field, base_val, overlay_val) in bool_fields {
        if base_val.is_some() && overlay_val.is_some() && base_val != overlay_val {
            report.conflicts.push(MergeConflict {
                field: field.to_string(),
                base_value: format!("{base_val:?}"),
                overlay_value: format!("{overlay_val:?}"),
                resolved_value: format!("{overlay_val:?}"),
                base_source: base_source.to_string(),
                overlay_source: overlay_source.to_string(),
            });
        }
        let provenance = crate::types::Provenance::new(
            if overlay_val.is_some() {
                crate::types::ConfigSource::Workspace
            } else if base_val.is_some() {
                crate::types::ConfigSource::User
            } else {
                crate::types::ConfigSource::Default
            },
            field,
        );
        report.provenance.insert(field.to_string(), provenance);
    }

    // Track scalar conflicts where both non-default and differ.
    if base.model.default_model != overlay.model.default_model
        && !overlay.model.default_model.is_empty()
        && !base.model.default_model.is_empty()
    {
        report.conflicts.push(MergeConflict {
            field: "model.default_model".into(),
            base_value: base.model.default_model.clone(),
            overlay_value: overlay.model.default_model.clone(),
            resolved_value: overlay.model.default_model.clone(),
            base_source: base_source.into(),
            overlay_source: overlay_source.into(),
        });
    }

    let merged = merge(base, overlay);
    (merged, report)
}
