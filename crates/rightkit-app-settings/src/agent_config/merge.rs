//! Layered merge logic for [`HostConfig`].

use crate::agent_config::types::{
    BudgetConfig, FeatureConfig, HooksConfig, HostConfig, ModelConfig, OrchestrationConfig,
    PermissionsConfig, PromptConfig, RemoteConfig, SessionConfig, ToolsConfig, TuiConfig,
    TurnConfig,
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
/// [`crate::agent_config::ToolsConfig::lazy_tool_loading`].
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
        tui: merge_tui(base.tui, overlay.tui),
        hooks: merge_hooks(base.hooks, overlay.hooks),
        turn: TurnConfig {
            max_wall_clock_minutes: option_scalar(
                base.turn.max_wall_clock_minutes,
                overlay.turn.max_wall_clock_minutes,
            ),
        },
    }
}

/// Hook lists per event extend the base list in order: the user layer's hooks run first, then the
/// overlay's (rule 58: one ordered chain, any deny wins).
fn merge_hooks(mut base: HooksConfig, overlay: HooksConfig) -> HooksConfig {
    for (event, value) in overlay.0 {
        match (base.0.get_mut(&event), value) {
            (Some(serde_json::Value::Array(existing)), serde_json::Value::Array(more)) => {
                existing.extend(more);
            }
            (_, value) => {
                base.0.insert(event, value);
            }
        }
    }
    base
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
        routing: crate::agent_config::types::RoutingConfig {
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
    pub provenance: HashMap<String, crate::agent_config::types::Provenance>,
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
    config: &crate::agent_config::types::HostConfig,
) -> Result<(), Vec<crate::agent_config::types::ValidationError>> {
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
        let provenance = crate::agent_config::types::Provenance::new(
            if overlay_val.is_some() {
                crate::agent_config::types::ConfigSource::Workspace
            } else if base_val.is_some() {
                crate::agent_config::types::ConfigSource::User
            } else {
                crate::agent_config::types::ConfigSource::Default
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_config::types::{AgentOverride, PreGateCheckConfig};

    fn check(id: &str, command: &str) -> PreGateCheckConfig {
        PreGateCheckConfig {
            id: id.to_string(),
            command: command.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn lazy_tool_loading_defaults_on() {
        // It shipped defaulting to false, so every live request carried all 68
        // tool schemas — the exact token tax the feature exists to remove.
        assert!(crate::agent_config::ToolsConfig::default().lazy_tool_loading());
        assert!(crate::agent_config::HostConfig::default()
            .tools
            .lazy_tool_loading());
    }

    #[test]
    fn merge_tools_overlay_enables_lazy_tool_loading() {
        let base = HostConfig::default();
        let mut overlay = HostConfig::default();
        overlay.tools.lazy_tool_loading = Some(true);

        let out = merge(base, overlay);

        assert!(out.tools.lazy_tool_loading());
    }

    #[test]
    fn merge_tools_base_lazy_tool_loading_survives_default_overlay() {
        let mut base = HostConfig::default();
        base.tools.lazy_tool_loading = Some(true);
        let overlay = HostConfig::default();

        let out = merge(base, overlay);

        assert!(out.tools.lazy_tool_loading());
    }

    #[test]
    fn model_interface_defaults_to_legacy_promoted() {
        assert_eq!(
            crate::agent_config::ToolsConfig::default().model_interface(),
            crate::agent_config::ModelToolInterfaceConfig::LegacyPromoted
        );
        assert_eq!(
            crate::agent_config::HostConfig::default()
                .tools
                .model_interface(),
            crate::agent_config::ModelToolInterfaceConfig::LegacyPromoted
        );
    }

    #[test]
    fn model_interface_fixed_v1_parses_and_merges() {
        let loader = crate::agent_config::ConfigLoader::new();
        let user = loader
            .load_str("[tools]\nmodel-interface = \"fixed-v1\"\n")
            .expect("valid toml");

        let out = loader.load_layered(vec![user]);

        assert_eq!(
            out.tools.model_interface(),
            crate::agent_config::ModelToolInterfaceConfig::FixedV1
        );
        assert!(out.tools.lazy_tool_loading());
    }

    #[test]
    fn model_interface_explicit_legacy_survives_empty_overlay() {
        let mut base = HostConfig::default();
        base.tools.model_interface = Some(crate::agent_config::ModelToolInterfaceConfig::FixedV1);
        let mut overlay = HostConfig::default();
        overlay.tools.model_interface =
            Some(crate::agent_config::ModelToolInterfaceConfig::LegacyPromoted);

        let out = merge(base.clone(), overlay);
        assert_eq!(
            out.tools.model_interface(),
            crate::agent_config::ModelToolInterfaceConfig::LegacyPromoted
        );

        let out = merge(base, HostConfig::default());
        assert_eq!(
            out.tools.model_interface(),
            crate::agent_config::ModelToolInterfaceConfig::FixedV1
        );
    }

    /// Every default-on flag documents "set `false` to disable". `scalar()`
    /// could not express that — `false` is `bool::default()`, so an explicit
    /// `false` was indistinguishable from "absent" and silently discarded.
    #[test]
    fn overlay_false_turns_default_on_flags_off() {
        let base = HostConfig::default();
        let mut overlay = HostConfig::default();
        overlay.tools.lazy_tool_loading = Some(false);
        overlay.orchestration.eval_hack_advisory = Some(false);
        overlay.prompt.brief_default = Some(false);

        let out = merge(base, overlay);

        assert!(!out.tools.lazy_tool_loading());
        assert!(!out.orchestration.eval_hack_advisory());
        assert!(!out.prompt.brief_default());
    }

    /// The same, through the real TOML path a user's `config.toml` takes.
    #[test]
    fn toml_false_survives_the_layered_load() {
        let loader = crate::agent_config::ConfigLoader::new();
        let user = loader
            .load_str("[tools]\nlazy-tool-loading = false\n\n[prompt]\nbrief-default = false\n")
            .expect("valid toml");

        let out = loader.load_layered(vec![user]);

        assert!(!out.tools.lazy_tool_loading());
        assert!(!out.prompt.brief_default());
    }

    /// An absent key must not pin the default that happened to be current when
    /// the file was written — this is what froze `lazy-tool-loading` off.
    #[test]
    fn absent_keys_keep_the_current_default() {
        let loader = crate::agent_config::ConfigLoader::new();
        let user = loader.load_str("[tools]\n").expect("valid toml");

        let out = loader.load_layered(vec![user]);

        assert!(out.tools.lazy_tool_loading());
        assert!(out.orchestration.eval_hack_advisory());
        assert!(out.prompt.brief_default());
    }

    /// Saved configs must not write flags nobody set, or every save re-freezes
    /// today's defaults into the file forever.
    #[test]
    fn unset_flags_are_not_serialized() {
        let rendered = toml::to_string_pretty(&HostConfig::default()).expect("serializes");

        assert!(!rendered.contains("lazy-tool-loading"));
        assert!(!rendered.contains("eval-hack-advisory"));
        assert!(!rendered.contains("brief-default"));
        assert!(!rendered.contains("conductor-routing"));
        assert!(!rendered.contains("capability-router"));
    }

    #[test]
    fn explicit_false_wins_across_all_boolean_layers() {
        let mut defaults = HostConfig::default();
        defaults.session.disable_thinking = Some(true);
        defaults.remote.web_enabled = Some(true);

        let user = HostConfig::default();
        let workspace = HostConfig::default();
        let mut session = HostConfig::default();
        session.session.disable_thinking = Some(false);
        let mut runtime = HostConfig::default();
        runtime.remote.web_enabled = Some(false);

        let out = [defaults, user, workspace, session, runtime]
            .into_iter()
            .fold(HostConfig::default(), merge);

        assert_eq!(out.session.disable_thinking, Some(false));
        assert!(!out.session.disable_thinking());
        assert_eq!(out.remote.web_enabled, Some(false));
        assert!(!out.remote.web_enabled());
    }

    #[test]
    fn explicit_boolean_values_round_trip_without_materializing_unset() {
        let loader = crate::agent_config::ConfigLoader::new();
        let config = loader
            .load_str("[session]\ndisable-thinking = false\n\n[remote]\nweb-enabled = true\n")
            .expect("valid config");

        let rendered = toml::to_string_pretty(&config).expect("serializes");
        let round_trip = loader.load_str(&rendered).expect("round trips");

        assert_eq!(round_trip.session.disable_thinking, Some(false));
        assert_eq!(round_trip.remote.web_enabled, Some(true));
        assert!(!rendered.contains("lazy-tool-loading"));
    }

    #[test]
    fn merge_orchestration_keeps_base_budget_when_overlay_is_default() {
        let mut base = HostConfig::default();
        base.orchestration.budget.per_task_max_usd = 1.25;
        base.orchestration.budget.max_workers = 8;
        let overlay = HostConfig::default();

        let out = merge(base, overlay);

        assert_eq!(out.orchestration.budget.per_task_max_usd, 1.25);
        assert_eq!(out.orchestration.budget.max_workers, 8);
    }

    #[test]
    fn merge_orchestration_replaces_budget_when_overlay_is_non_default() {
        let mut base = HostConfig::default();
        base.orchestration.budget.per_task_max_usd = 1.25;
        let mut overlay = HostConfig::default();
        overlay.orchestration.budget.per_task_max_usd = 2.5;

        let out = merge(base, overlay);

        assert_eq!(out.orchestration.budget.per_task_max_usd, 2.5);
        assert_eq!(
            out.orchestration.budget.max_workers,
            BudgetConfig::default().max_workers
        );
    }

    #[test]
    fn merge_orchestration_keeps_base_checks_when_overlay_empty() {
        let mut base = HostConfig::default();
        base.orchestration.pre_gate_checks = vec![check("fmt", "cargo fmt --check")];
        let overlay = HostConfig::default();

        let out = merge(base, overlay);

        assert_eq!(out.orchestration.pre_gate_checks.len(), 1);
        assert_eq!(out.orchestration.pre_gate_checks[0].id, "fmt");
    }

    #[test]
    fn merge_orchestration_replaces_checks_when_overlay_declares_any() {
        let mut base = HostConfig::default();
        base.orchestration.pre_gate_checks = vec![check("fmt", "cargo fmt --check")];
        let mut overlay = HostConfig::default();
        overlay.orchestration.pre_gate_checks = vec![check("test", "cargo test")];

        let out = merge(base, overlay);

        assert_eq!(out.orchestration.pre_gate_checks.len(), 1);
        assert_eq!(out.orchestration.pre_gate_checks[0].id, "test");
    }

    #[test]
    fn merge_orchestration_overlay_agents_replace_by_name() {
        let mut base = HostConfig::default();
        base.orchestration.agents.insert(
            "coder".to_string(),
            AgentOverride {
                model_id: "base-model".to_string(),
                provider_id: "base-provider".to_string(),
            },
        );
        let mut overlay = HostConfig::default();
        overlay.orchestration.agents.insert(
            "coder".to_string(),
            AgentOverride {
                model_id: "overlay-model".to_string(),
                provider_id: "overlay-provider".to_string(),
            },
        );

        let out = merge(base, overlay);
        let agent = out.orchestration.agents.get("coder").unwrap();

        assert_eq!(agent.model_id, "overlay-model");
        assert_eq!(agent.provider_id, "overlay-provider");
    }

    #[test]
    fn merge_orchestration_conductor_routing_explicit_false_wins() {
        // Base=ON, overlay explicit false → OFF (explicit false wins).
        let mut base = HostConfig::default();
        base.orchestration.conductor_routing = Some(true);
        let mut overlay = HostConfig::default();
        overlay.orchestration.conductor_routing = Some(false);
        let out = merge(base, overlay);
        assert!(
            !out.orchestration.conductor_routing(),
            "explicit false must win over true base"
        );
        assert_eq!(out.orchestration.conductor_routing, Some(false));

        // Base absent, overlay true → true.
        let base = HostConfig::default();
        let mut overlay = HostConfig::default();
        overlay.orchestration.conductor_routing = Some(true);
        let out = merge(base, overlay);
        assert!(out.orchestration.conductor_routing());
        assert_eq!(out.orchestration.conductor_routing, Some(true));

        // Base true, overlay absent → stays true.
        let mut base = HostConfig::default();
        base.orchestration.conductor_routing = Some(true);
        let overlay = HostConfig::default();
        let out = merge(base, overlay);
        assert!(out.orchestration.conductor_routing());
    }

    #[test]
    fn merge_orchestration_routing_capability_router_explicit_false_wins() {
        // Base=ON, overlay explicit false → OFF.
        let mut base = HostConfig::default();
        base.orchestration.routing.capability_router = Some(true);
        let mut overlay = HostConfig::default();
        overlay.orchestration.routing.capability_router = Some(false);
        let out = merge(base, overlay);
        assert!(
            !out.orchestration.routing.capability_router(),
            "explicit false must win over true base"
        );
        assert_eq!(out.orchestration.routing.capability_router, Some(false));

        // Base absent, overlay true → true.
        let base = HostConfig::default();
        let mut overlay = HostConfig::default();
        overlay.orchestration.routing.capability_router = Some(true);
        let out = merge(base, overlay);
        assert!(out.orchestration.routing.capability_router());

        // Base ON, overlay absent → stays ON.
        let mut base = HostConfig::default();
        base.orchestration.routing.capability_router = Some(true);
        let overlay = HostConfig::default();
        let out = merge(base, overlay);
        assert!(
            out.orchestration.routing.capability_router(),
            "base-only capability_router must survive a default overlay"
        );

        // Default stays OFF.
        let base = HostConfig::default();
        let overlay = HostConfig::default();
        let out = merge(base, overlay);
        assert!(!out.orchestration.routing.capability_router());
    }

    #[test]
    fn merge_orchestration_routing_aggressiveness_overlay_wins_when_set() {
        let base = HostConfig::default();
        let mut overlay = HostConfig::default();
        overlay.orchestration.routing.aggressiveness = "aggressive".to_string();
        let out = merge(base, overlay);
        assert_eq!(out.orchestration.routing.aggressiveness, "aggressive");

        // Empty overlay string preserves base.
        let mut base = HostConfig::default();
        base.orchestration.routing.aggressiveness = "balanced".to_string();
        let overlay = HostConfig::default();
        let out = merge(base, overlay);
        assert_eq!(out.orchestration.routing.aggressiveness, "balanced");
    }

    #[test]
    fn merge_orchestration_role_model_mode_allows_explicit_single_to_replace_split() {
        let mut base = HostConfig::default();
        base.orchestration.role_model_mode = "split".to_string();
        let mut overlay = HostConfig::default();
        overlay.orchestration.role_model_mode = "single".to_string();

        let out = merge(base, overlay);

        assert_eq!(out.orchestration.role_model_mode, "single");
    }
}
