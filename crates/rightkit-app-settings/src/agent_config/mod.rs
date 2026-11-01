//! host_config — layered TOML configuration for the host app.
//!
//! Merge precedence (later overrides earlier for scalars; lists extend):
//! 1. built-in defaults
//! 2. user config (`<state root>/config.toml`, the rightkit-daemon AppRoot, see [`state_root`])
//! 3. workspace config (`<workspace>/<state-dir>/config.toml`)
//! 4. env var overrides (`RIGHTKIT_*`)
//! 5. session overrides (a passed-in map)

pub mod accounts;
pub mod agent_config;
pub mod instructions;
pub mod licence_env;
mod loader;
mod merge;
pub mod okf;
pub mod presets;
pub mod session_secrets;
pub mod state_root;
#[cfg(any(test, feature = "agent_config-test-mode"))]
pub mod test_isolation;
mod types;
pub mod workspace_trust;

pub use agent_config::{find_agents_dir, AgentMcpConfig, AgentMcpServer};
pub use instructions::{Frontmatter, InstructionFile, InstructionLoader, InstructionScope};
pub use loader::{ConfigError, ConfigLoader};
pub use merge::{
    extend_dedup, merge, merge_with_report, validate_config, MergeConflict, MergeReport, TypedMerge,
};
pub use okf::{
    compress_prose, emit_bundle as emit_okf_bundle, parse_bundle as parse_okf_bundle, OkfBundle,
    OkfBundleStats, OkfConcept, OkfEmitOptions, OkfError, OkfLink, OKF_VERSION,
};
pub use state_root::{
    app_name, os_home_dir, set_app_name, state_root, state_root_is_isolated, APP_NAME,
};
pub use types::{
    AgentOverride, BooleanSetting, BudgetConfig, ChannelProfileConfig, ConfigSource, FeatureConfig,
    HostConfig, MemoryConfig, ModelConfig, ModelToolInterfaceConfig, OrchestrationConfig,
    PermissionsConfig, PreGateCheckConfig, PromptConfig, Provenance, ProviderRouteConfig,
    RemoteConfig, RoutingConfig, SessionConfig, ToolsConfig, ToolsetConfig, TuiConfig,
    ValidationError, WorkflowImportTrustedPublicKey,
};
pub use workspace_trust::{
    is_workspace_trusted, is_workspace_trusted_in, workspace_trust_key, workspace_trust_store_path,
};

#[cfg(test)]
pub(crate) fn env_var_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn loader() -> ConfigLoader {
        ConfigLoader::new()
    }

    #[test]
    fn default_config_is_empty_ish() {
        let c = HostConfig::default();
        assert_eq!(c.model.default_model, "");
        assert!(c.permissions.deny_read_globs.is_empty());
        assert!(c.permissions.deny_write_globs.is_empty());
        assert!(c.model.fallback_models.is_empty());
        assert_eq!(c.model.max_tokens, 0);
        assert!(c.tools.web_search_provider.is_none());
        assert_eq!(c.orchestration.budget.per_task_max_usd, 0.1);
        assert_eq!(c.orchestration.budget.per_agent_call_max_usd, 0.1);
    }

    #[test]
    fn load_str_parses_model() {
        let toml = r#"
            [model]
            default-model = "claude-opus"
            max-tokens = 4096
            temperature = 0.7
            fallback-models = ["a", "b"]
        "#;
        let c = loader().load_str(toml).unwrap();
        assert_eq!(c.model.default_model, "claude-opus");
        assert_eq!(c.model.max_tokens, 4096);
        assert_eq!(c.model.temperature, 0.7);
        assert_eq!(c.model.fallback_models, vec!["a", "b"]);
    }

    #[test]
    fn load_str_parses_session_defaults() {
        let toml = r#"
            [session]
            allowed-tools = ["read_file", "search"]
            disallowed-tools = ["shell"]
            effort = "high"
            thinking-budget-tokens = 4096
            disable-thinking = true
        "#;
        let c = loader().load_str(toml).unwrap();
        assert_eq!(c.session.allowed_tools, vec!["read_file", "search"]);
        assert_eq!(c.session.disallowed_tools, vec!["shell"]);
        assert_eq!(c.session.effort, "high");
        assert_eq!(c.session.thinking_budget_tokens, Some(4096));
        assert!(c.session.disable_thinking());
    }

    #[test]
    fn load_str_parses_toolsets_and_channel_profiles() {
        let toml = r#"
            [toolsets.remote-safe]
            name = "Remote safe"
            description = "Remote profile"
            allowed-tools = ["read_file", "search"]
            disallowed-tools = ["shell"]
            read-only = true
            default-behavioral-skills = ["cr-baseline-behavior"]

            [channel-profiles.telegram]
            toolset = "remote-safe"
            delivery-policy = "approval-required"
            trust = "remoteAgentUntrusted"
        "#;
        let c = loader().load_str(toml).unwrap();
        let profile = c.toolsets.get("remote-safe").expect("toolset profile");
        assert_eq!(profile.name, "Remote safe");
        assert_eq!(profile.description, "Remote profile");
        assert_eq!(profile.allowed_tools, vec!["read_file", "search"]);
        assert_eq!(profile.disallowed_tools, vec!["shell"]);
        assert!(profile.read_only);
        assert_eq!(
            profile.default_behavioral_skills,
            vec!["cr-baseline-behavior"]
        );

        let channel = c.channel_profiles.get("telegram").expect("channel profile");
        assert_eq!(channel.toolset, "remote-safe");
        assert_eq!(channel.delivery_policy, "approval-required");
        assert_eq!(channel.trust, "remoteAgentUntrusted");
    }

    #[test]
    fn load_str_parses_provider_routes() {
        let toml = r#"
            [provider-routes.ollama-local]
            display-name = "Local: qwen2.5-coder:7b"
            base-url = "http://127.0.0.1:11434/v1"
            model = "qwen2.5-coder:7b"
            api-key-required = false
        "#;
        let c = loader().load_str(toml).unwrap();
        let route = c
            .provider_routes
            .get("ollama-local")
            .expect("provider route");
        assert_eq!(route.display_name, "Local: qwen2.5-coder:7b");
        assert_eq!(route.base_url, "http://127.0.0.1:11434/v1");
        assert_eq!(route.model, "qwen2.5-coder:7b");
        assert!(!route.api_key_required);
    }

    #[test]
    fn load_str_invalid_toml_errors() {
        let toml = "this is = = not valid toml [[[";
        let err = loader().load_str(toml).unwrap_err();
        match err {
            ConfigError::Toml(_) => {}
            other => panic!("expected Toml error, got {other:?}"),
        }
    }

    #[test]
    fn merge_scalar_overlay_wins() {
        let mut base = HostConfig::default();
        base.model.default_model = "base-model".into();
        let mut overlay = HostConfig::default();
        overlay.model.default_model = "overlay-model".into();
        let out = merge(base, overlay);
        assert_eq!(out.model.default_model, "overlay-model");
    }

    #[test]
    fn merge_scalar_overlay_default_keeps_base() {
        let mut base = HostConfig::default();
        base.model.default_model = "base-model".into();
        let overlay = HostConfig::default(); // empty model
        let out = merge(base, overlay);
        assert_eq!(out.model.default_model, "base-model");
    }

    #[test]
    fn merge_option_some_wins() {
        let mut base = HostConfig::default();
        base.tools.web_search_provider = Some("base".into());
        let mut overlay = HostConfig::default();
        overlay.tools.web_search_provider = Some("overlay".into());
        let out = merge(base, overlay);
        assert_eq!(out.tools.web_search_provider.as_deref(), Some("overlay"));
    }

    #[test]
    fn merge_option_none_keeps_base() {
        let mut base = HostConfig::default();
        base.tools.web_search_provider = Some("base".into());
        let overlay = HostConfig::default(); // None
        let out = merge(base, overlay);
        assert_eq!(out.tools.web_search_provider.as_deref(), Some("base"));
    }

    #[test]
    fn merge_lists_extend() {
        let mut base = HostConfig::default();
        base.model.fallback_models = vec!["a".into(), "b".into()];
        let mut overlay = HostConfig::default();
        overlay.model.fallback_models = vec!["c".into()];
        let out = merge(base, overlay);
        assert_eq!(out.model.fallback_models, vec!["a", "b", "c"]);
    }

    #[test]
    fn merge_lists_dedup() {
        let mut base = HostConfig::default();
        base.model.fallback_models = vec!["a".into(), "b".into()];
        let mut overlay = HostConfig::default();
        overlay.model.fallback_models = vec!["b".into(), "c".into()];
        let out = merge(base, overlay);
        assert_eq!(out.model.fallback_models, vec!["a", "b", "c"]);
    }

    #[test]
    fn load_layered_three_layers() {
        // Layer 1: defaults (model + temperature + one fallback + one deny glob)
        let mut defaults = HostConfig::default();
        defaults.model.default_model = "default-model".into();
        defaults.model.temperature = 0.5;
        defaults.model.fallback_models = vec!["d1".into()];
        defaults.permissions.mode = "ask".into();
        defaults.permissions.deny_read_globs = vec!["**/.git/**".into()];

        // Layer 2: user — overrides model, adds a fallback, adds a deny glob
        let mut user = HostConfig::default();
        user.model.default_model = "user-model".into();
        user.model.fallback_models = vec!["u1".into()];
        user.permissions.deny_read_globs = vec!["**/.env".into()];

        // Layer 3: workspace — overrides model again, adds fallback + token
        let mut workspace = HostConfig::default();
        workspace.model.default_model = "workspace-model".into();
        workspace.model.fallback_models = vec!["w1".into()];
        workspace.session.allowed_tools = vec!["read_file".into()];
        workspace.session.effort = "high".into();
        workspace.session.thinking_budget_tokens = Some(8192);
        workspace.session.disable_thinking = Some(true);
        workspace.remote.telegram_token = Some("tok".into());

        let out = loader().load_layered(vec![defaults, user, workspace]);

        // Scalar comes from the topmost layer that set it.
        assert_eq!(out.model.default_model, "workspace-model");
        // Temperature only set by defaults -> kept.
        assert_eq!(out.model.temperature, 0.5);
        // Mode only set by defaults -> kept.
        assert_eq!(out.permissions.mode, "ask");
        // Lists accumulate across all three layers.
        assert_eq!(out.model.fallback_models, vec!["d1", "u1", "w1"]);
        assert_eq!(
            out.permissions.deny_read_globs,
            vec!["**/.git/**", "**/.env"]
        );
        assert_eq!(out.session.allowed_tools, vec!["read_file"]);
        assert_eq!(out.session.effort, "high");
        assert_eq!(out.session.thinking_budget_tokens, Some(8192));
        assert!(out.session.disable_thinking());
        // Option set only by workspace.
        assert_eq!(out.remote.telegram_token.as_deref(), Some("tok"));
    }

    #[test]
    fn load_files_for_workspace_reads_explicit_workspace_root() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path().join(".rightkit");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("config.toml"),
            r#"
[model]
default-model = "workspace-model"

[session]
allowed-tools = ["read_file"]
"#,
        )
        .unwrap();

        let out = loader().load_files_for_workspace(dir.path());
        assert_eq!(out.model.default_model, "workspace-model");
        assert_eq!(out.session.allowed_tools, vec!["read_file"]);
    }

    #[test]
    fn suite_root_override_moves_user_config_load_and_save_path() {
        let _guard = crate::agent_config::env_var_test_lock();
        let prior = std::env::var_os(rightkit_daemon::ENV_SUITE_ROOT);
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var(rightkit_daemon::ENV_SUITE_ROOT, home.path());
        }

        let mut cfg = HostConfig::default();
        cfg.model.default_model = "home-model".into();
        cfg.remote.telegram_token = Some("home-token".into());
        loader().save_user(&cfg).unwrap();

        let saved_path = home.path().join("apps").join("Agent").join("config.toml");
        let saved = std::fs::read_to_string(&saved_path).unwrap();
        assert!(saved.contains("home-token"));

        let loaded = loader().load_files_for_workspace(workspace.path());
        assert_eq!(loaded.model.default_model, "home-model");
        assert_eq!(loaded.remote.telegram_token.as_deref(), Some("home-token"));

        unsafe {
            match prior {
                Some(value) => std::env::set_var(rightkit_daemon::ENV_SUITE_ROOT, value),
                None => std::env::remove_var(rightkit_daemon::ENV_SUITE_ROOT),
            }
        }
    }

    #[test]
    fn apply_env_model_override() {
        let config = HostConfig::default();
        let mut env = HashMap::new();
        env.insert("RIGHTKIT_MODEL".to_string(), "env-model".to_string());
        let out = loader().apply_env(config, &env);
        assert_eq!(out.model.default_model, "env-model");
    }

    #[test]
    fn apply_env_permission_mode() {
        let config = HostConfig::default();
        let mut env = HashMap::new();
        env.insert("RIGHTKIT_PERMISSION_MODE".to_string(), "auto".to_string());
        let out = loader().apply_env(config, &env);
        assert_eq!(out.permissions.mode, "auto");
    }

    #[test]
    fn apply_env_telegram_token() {
        let config = HostConfig::default();
        let mut env = HashMap::new();
        env.insert("RIGHTKIT_TELEGRAM_TOKEN".to_string(), "secret".to_string());
        let out = loader().apply_env(config, &env);
        assert_eq!(out.remote.telegram_token.as_deref(), Some("secret"));
    }

    #[test]
    fn apply_env_web_search_config() {
        let config = HostConfig::default();
        let mut env = HashMap::new();
        env.insert(
            "RIGHTKIT_WEB_SEARCH_PROVIDER".to_string(),
            "brave".to_string(),
        );
        env.insert(
            "RIGHTKIT_WEB_SEARCH_API_KEY".to_string(),
            "search-secret".to_string(),
        );
        let out = loader().apply_env(config, &env);
        assert_eq!(out.tools.web_search_provider.as_deref(), Some("brave"));
        assert_eq!(
            out.tools.web_search_api_key.as_deref(),
            Some("search-secret")
        );
    }

    #[test]
    fn load_str_parses_feature_flags() {
        let toml = r#"
            [features]
            cli-acquisition-v1 = true
        "#;
        let c = loader().load_str(toml).unwrap();
        assert_eq!(c.features.cli_acquisition_v1, Some(true));
    }

    #[test]
    fn checkpoint_features_are_opt_in_and_merge_like_other_flags() {
        let off = FeatureConfig::default();
        assert!(!off.turn_checkpoints_enabled());
        assert!(!off.auto_commit_on_exit_enabled());

        let toml = r#"
            [features]
            turn-checkpoints = true
            auto-commit-on-exit = true
        "#;
        let c = loader().load_str(toml).unwrap();
        assert!(c.features.turn_checkpoints_enabled());
        assert!(c.features.auto_commit_on_exit_enabled());

        let mut base = HostConfig::default();
        base.features.turn_checkpoints = Some(true);
        let mut overlay = HostConfig::default();
        overlay.features.turn_checkpoints = Some(false);
        overlay.features.auto_commit_on_exit = Some(true);
        let out = merge(base, overlay);
        assert!(!out.features.turn_checkpoints_enabled());
        assert!(out.features.auto_commit_on_exit_enabled());
    }

    #[test]
    fn merge_feature_flag_false_overrides_true() {
        let mut base = HostConfig::default();
        base.features.cli_acquisition_v1 = Some(true);
        let mut overlay = HostConfig::default();
        overlay.features.cli_acquisition_v1 = Some(false);
        let out = merge(base, overlay);
        assert_eq!(out.features.cli_acquisition_v1, Some(false));
    }

    #[test]
    fn apply_env_cli_acquisition_v1() {
        let config = HostConfig::default();
        let mut env = HashMap::new();
        env.insert(
            "RIGHTKIT_CLI_ACQUISITION_V1".to_string(),
            "false".to_string(),
        );
        let out = loader().apply_env(config, &env);
        assert_eq!(out.features.cli_acquisition_v1, Some(false));
    }

    #[test]
    fn apply_env_empty_keeps_config() {
        let mut config = HostConfig::default();
        config.model.default_model = "kept".into();
        let env = HashMap::new();
        let out = loader().apply_env(config, &env);
        assert_eq!(out.model.default_model, "kept");
    }

    #[test]
    fn roundtrip_serialize_deserialize() {
        let mut c = HostConfig::default();
        c.model.default_model = "m".into();
        c.model.fallback_models = vec!["a".into(), "b".into()];
        c.model.max_tokens = 1234;
        c.model.temperature = 0.9;
        c.permissions.mode = "ask".into();
        c.permissions.deny_read_globs = vec!["**/.env".into()];
        c.tools.web_search_provider = Some("brave".into());
        c.remote.telegram_token = Some("tok".into());
        c.remote.telegram_allowed_users = vec![1, 2, 3];
        c.remote.web_enabled = Some(true);

        let json = serde_json::to_string(&c).unwrap();
        let back: HostConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);

        let toml_str = toml::to_string(&c).unwrap();
        let back_toml: HostConfig = loader().load_str(&toml_str).unwrap();
        assert_eq!(c, back_toml);
    }

    #[test]
    fn extend_dedup_preserves_order() {
        let base = vec!["a", "b", "c"];
        let overlay = vec!["c", "d", "a", "e"];
        let out = extend_dedup(base, overlay);
        assert_eq!(out, vec!["a", "b", "c", "d", "e"]);
    }

    #[test]
    fn telegram_allowed_users_extend() {
        let mut base = HostConfig::default();
        base.remote.telegram_allowed_users = vec![1, 2];
        let mut overlay = HostConfig::default();
        overlay.remote.telegram_allowed_users = vec![2, 3];
        let out = merge(base, overlay);
        assert_eq!(out.remote.telegram_allowed_users, vec![1, 2, 3]);
    }

    #[test]
    fn toolsets_and_channel_profiles_overlay_by_id() {
        let mut base = HostConfig::default();
        base.toolsets.insert(
            "remote-safe".into(),
            ToolsetConfig {
                name: "Base".into(),
                allowed_tools: vec!["read_file".into()],
                ..Default::default()
            },
        );
        base.channel_profiles.insert(
            "telegram".into(),
            ChannelProfileConfig {
                toolset: "remote-safe".into(),
                delivery_policy: "approval-required".into(),
                trust: "remoteAgentUntrusted".into(),
            },
        );
        let mut overlay = HostConfig::default();
        overlay.toolsets.insert(
            "remote-safe".into(),
            ToolsetConfig {
                name: "Overlay".into(),
                allowed_tools: vec!["read_file".into(), "search".into()],
                disallowed_tools: vec!["shell".into()],
                ..Default::default()
            },
        );
        overlay.channel_profiles.insert(
            "telegram".into(),
            ChannelProfileConfig {
                toolset: "telegram-remote".into(),
                delivery_policy: "target-validated".into(),
                trust: "toolUntrusted".into(),
            },
        );

        let out = merge(base, overlay);
        let profile = out.toolsets.get("remote-safe").unwrap();
        assert_eq!(profile.name, "Overlay");
        assert_eq!(profile.allowed_tools, vec!["read_file", "search"]);
        assert_eq!(profile.disallowed_tools, vec!["shell"]);
        let channel = out.channel_profiles.get("telegram").unwrap();
        assert_eq!(channel.toolset, "telegram-remote");
        assert_eq!(channel.delivery_policy, "target-validated");
        assert_eq!(channel.trust, "toolUntrusted");
    }

    #[test]
    fn provider_routes_overlay_by_id() {
        let mut base = HostConfig::default();
        base.provider_routes.insert(
            "local".into(),
            ProviderRouteConfig {
                base_url: "http://127.0.0.1:11434/v1".into(),
                model: "old-model".into(),
                ..Default::default()
            },
        );
        let mut overlay = HostConfig::default();
        overlay.provider_routes.insert(
            "local".into(),
            ProviderRouteConfig {
                base_url: "http://127.0.0.1:1234/v1".into(),
                model: "new-model".into(),
                ..Default::default()
            },
        );

        let out = merge(base, overlay);
        let route = out.provider_routes.get("local").unwrap();
        assert_eq!(route.base_url, "http://127.0.0.1:1234/v1");
        assert_eq!(route.model, "new-model");
    }

    #[test]
    fn web_search_config_overlay() {
        let mut base = HostConfig::default();
        base.tools.web_search_provider = Some("base-provider".into());
        base.tools.web_search_api_key = Some("base-key".into());
        let mut overlay = HostConfig::default();
        overlay.tools.web_search_provider = Some("overlay-provider".into());
        // overlay leaves api_key None -> base key preserved
        let out = merge(base, overlay);
        assert_eq!(
            out.tools.web_search_provider.as_deref(),
            Some("overlay-provider")
        );
        assert_eq!(out.tools.web_search_api_key.as_deref(), Some("base-key"));
    }
}
