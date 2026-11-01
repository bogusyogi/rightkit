#[test]
fn default_db_path_nonempty() {
    assert!(!default_db_path().is_empty());
}

#[test]
fn cli_provider_route_preserves_configured_fallback_models() {
    let route = cli_provider_route(
        "key".into(),
        "primary-model".into(),
        "https://openrouter.ai/api/v1",
        vec!["fallback-a".into(), "fallback-b".into()],
    );

    assert_eq!(route.model_id, "primary-model");
    assert_eq!(
        route.fallback_models,
        vec!["fallback-a".to_string(), "fallback-b".to_string()]
    );
    assert_eq!(route.effort, None);
}

#[test]
fn cli_subagent_timeout_uses_turn_budget_with_bounds() {
    let short = rightkit_agent_tools::coding_tools::SubagentSpawnOptions {
        max_iterations: Some(1),
        ..Default::default()
    };
    let long = rightkit_agent_tools::coding_tools::SubagentSpawnOptions {
        max_iterations: Some(999),
        ..Default::default()
    };

    assert_eq!(cli_subagent_timeout_seconds(&short, None), 60);
    assert_eq!(cli_subagent_timeout_seconds(&long, None), 900);
    assert_eq!(
        cli_subagent_timeout_seconds(&Default::default(), None),
        DEFAULT_CLI_SUBAGENT_TIMEOUT_SECONDS
    );
}

#[test]
fn cli_subagent_timeout_env_override_is_bounded() {
    assert_eq!(
        cli_subagent_timeout_seconds(&Default::default(), Some("1")),
        MIN_CLI_SUBAGENT_TIMEOUT_SECONDS
    );
    assert_eq!(
        cli_subagent_timeout_seconds(&Default::default(), Some("9999")),
        MAX_CLI_SUBAGENT_TIMEOUT_SECONDS
    );
    assert_eq!(
        cli_subagent_timeout_seconds(&Default::default(), Some("180")),
        180
    );
}

#[test]
fn tail_chars_keeps_diagnostic_suffix() {
    assert_eq!(tail_chars("short", 10), "short");
    assert_eq!(tail_chars("abcdef", 3), "...def");
}

#[test]
fn cli_toolset_profile_caps_explicit_allowed_tools() {
    let (active_toolset, allowed, disallowed) = resolve_cli_tool_filters(
        Some("dev-safe"),
        vec!["read_file".to_string(), "shell".to_string()],
        vec!["write_file".to_string()],
    )
    .expect("expected CLI tool filters");

    assert_eq!(active_toolset.as_deref(), Some("dev-safe"));
    assert_eq!(allowed, vec!["read_file"]);
    assert!(disallowed.contains(&"shell".to_string()));
    assert!(disallowed.contains(&"write_file".to_string()));
}

#[test]
fn cli_toolset_profile_rejects_unknown_profile() {
    let error =
        resolve_cli_tool_filters(Some("missing-toolset"), Vec::new(), Vec::new()).unwrap_err();

    assert!(error.contains("Unknown toolset 'missing-toolset'"));
}

#[test]
fn recipe_cli_params_require_key_value_pairs() {
    let params =
        parse_recipe_cli_params(&["feature=recipes".to_string(), "dryRun=true".to_string()])
            .expect("expected params");

    assert_eq!(params.get("feature").map(String::as_str), Some("recipes"));
    assert_eq!(params.get("dryRun").map(String::as_str), Some("true"));
    assert!(parse_recipe_cli_params(&["missing-equals".to_string()]).is_err());
    assert!(parse_recipe_cli_params(&["=empty".to_string()]).is_err());
}

#[test]
fn recipe_cli_resolves_yaml_file_target() {
    let path = std::env::temp_dir().join(format!(
        "coderight_recipe_cli_{}_fixture.yaml",
        std::process::id()
    ));
    std::fs::write(
        &path,
        r#"
id: cli-fixture
version: "1"
title: CLI fixture
description: Exercise recipe CLI target loading.
prompt: "Ship {{feature}}"
parameters:
  - key: feature
    inputType: string
    requirement: required
    description: Feature name
    default: recipes
    value: null
"#,
    )
    .unwrap();

    let recipe = resolve_cli_recipe_target(path.to_str().unwrap(), &RecipeStore::new())
        .expect("expected recipe");

    assert_eq!(recipe.id.as_deref(), Some("cli-fixture"));
    assert_eq!(recipe.title, "CLI fixture");
    let _ = std::fs::remove_file(path);
}

#[test]
fn recipe_cli_resolves_installed_id_and_deeplink() {
    let recipe = Recipe {
        id: Some("installed-cli".to_string()),
        version: "1".to_string(),
        title: "Installed CLI".to_string(),
        description: "Installed recipe".to_string(),
        instructions: None,
        prompt: Some("Run {{feature}}".to_string()),
        activities: Vec::new(),
        parameters: Vec::new(),
        required_toolsets: Vec::new(),
        required_mcp_servers: Vec::new(),
        required_plugins: Vec::new(),
        skills: Vec::new(),
        settings: Default::default(),
        response_schema: None,
        retry: None,
        sub_recipes: Vec::new(),
    };
    let mut store = RecipeStore::new();
    store.install(InstalledRecipe {
        id: "installed-cli".to_string(),
        source: rightkit_agent_recipes::RecipeSource::Inline,
        source_uri: None,
        title: recipe.title.clone(),
        version: recipe.version.clone(),
        checksum: rightkit_agent_recipes::recipe_checksum(&recipe),
        trusted: true,
        recipe,
        installed_at: now_iso(),
        latest_validation: None,
    });

    let loaded = resolve_cli_recipe_target("installed-cli", &store).expect("expected recipe");
    let params = parse_recipe_cli_params(&["feature=recipes".to_string()]).unwrap();
    let link = build_recipe_deeplink(loaded.id.as_deref().unwrap(), "render", &params).unwrap();

    assert_eq!(loaded.title, "Installed CLI");
    assert!(link.url.starts_with("coderight://recipes/installed-cli"));
    assert_eq!(link.action, "render");
}

#[test]
fn merged_config_reads_workspace_config() {
    let root = std::env::temp_dir().join(format!(
        "coderight_cli_config_workspace_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".coderight")).unwrap();
    std::fs::write(
        root.join(".coderight").join("config.toml"),
        r#"
[model]
default-model = "workspace-model"

[remote]
telegram-token = "workspace-token"
"#,
    )
    .unwrap();

    let env = std::collections::HashMap::new();
    let cfg = load_merged_config_for_workspace(&root, &env);
    assert_eq!(cfg.model.default_model, "workspace-model");
    assert_eq!(
        cfg.remote.telegram_token.as_deref(),
        Some("workspace-token")
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn resolve_engine_config_uses_keyless_configured_local_route() {
    let mut cfg = rightkit_app_settings::agent_config::RightKitConfig::default();
    cfg.model.default_model = "qwen2.5-coder:7b".to_string();
    cfg.provider_routes.insert(
        "ollama-local".to_string(),
        rightkit_app_settings::agent_config::ProviderRouteConfig {
            display_name: "Local: qwen2.5-coder:7b".to_string(),
            base_url: "http://127.0.0.1:11434/v1".to_string(),
            model: "qwen2.5-coder:7b".to_string(),
            api_key_required: false,
            api_key_env: None,
            approved_private: false,
        },
    );

    let engine = resolve_engine_config(&cfg, None, None).expect("engine config");
    assert_eq!(engine.model_id, "qwen2.5-coder:7b");
    assert_eq!(engine.base_url, "http://127.0.0.1:11434/v1");
    assert_eq!(engine.api_key, "");
}

#[test]
fn configured_route_falls_back_to_saved_key_when_env_is_absent() {
    let route = rightkit_app_settings::agent_config::ProviderRouteConfig {
        display_name: "Saved route".to_string(),
        base_url: "https://example.invalid/v1".to_string(),
        model: "saved-model".to_string(),
        api_key_required: true,
        api_key_env: Some("RIGHTKIT_TEST_UNSET_ROUTE_KEY".to_string()),
        approved_private: false,
    };
    let key = configured_route_api_key(
        "minimax",
        &route,
        |_| None,
        |provider| (provider == "minimax").then(|| "saved-route-key".to_string()),
    );
    assert_eq!(key.as_deref(), Some("saved-route-key"));
}

#[test]
fn default_hosted_provider_uses_openrouter_only() {
    let only_non_openrouter = select_keyed_catalog_provider(
        DEFAULT_HOSTED_PROVIDER_PRIORITY.iter().copied(),
        |provider| (provider != "openrouter").then(|| format!("{provider}-key")),
    );
    assert!(only_non_openrouter.is_none());

    let openrouter = select_keyed_catalog_provider(
        DEFAULT_HOSTED_PROVIDER_PRIORITY.iter().copied(),
        |provider| Some(format!("{provider}-key")),
    )
    .expect("preferred provider");

    assert_eq!(openrouter.0.id, "openrouter");
    assert_eq!(openrouter.1, "openrouter-key");
}

#[test]
fn merged_config_applies_env_overrides() {
    let root =
        std::env::temp_dir().join(format!("coderight_cli_config_env_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".coderight")).unwrap();
    std::fs::write(
        root.join(".coderight").join("config.toml"),
        r#"
[model]
default-model = "workspace-model"

[permissions]
mode = "ask"
"#,
    )
    .unwrap();

    let mut env = std::collections::HashMap::new();
    env.insert("RIGHTKIT_MODEL".to_string(), "env-model".to_string());
    env.insert(
        "RIGHTKIT_PERMISSION_MODE".to_string(),
        "accept-edits".to_string(),
    );

    let cfg = load_merged_config_for_workspace(&root, &env);
    assert_eq!(cfg.model.default_model, "env-model");
    assert_eq!(cfg.permissions.mode, "accept-edits");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn remote_setup_updates_only_requested_config_fields() {
    let mut cfg = rightkit_app_settings::agent_config::RightKitConfig::default();
    cfg.remote.telegram_token = Some("old-token".into());
    cfg.remote.telegram_allowed_users = vec![1, 2];
    cfg.remote.web_enabled = Some(false);

    let updated = apply_remote_setup_to_config(
        cfg.clone(),
        Some(" new-token "),
        &[3, 4],
        false,
        true,
        false,
    );
    assert_eq!(updated.remote.telegram_token.as_deref(), Some("new-token"));
    assert_eq!(updated.remote.telegram_allowed_users, vec![3, 4]);
    assert_eq!(updated.remote.web_enabled, Some(true));

    let preserved = apply_remote_setup_to_config(cfg.clone(), None, &[], false, false, false);
    assert_eq!(preserved.remote, cfg.remote);

    let cleared = apply_remote_setup_to_config(cfg, Some("ignored"), &[], true, false, true);
    assert!(cleared.remote.telegram_token.is_none());
    assert_eq!(cleared.remote.web_enabled, Some(false));
}

#[test]
fn remote_setup_next_steps_guide_user_discovery_when_allowlist_missing() {
    let mut cfg = rightkit_app_settings::agent_config::RightKitConfig::default();
    cfg.remote.telegram_token = Some("token".into());

    let steps = remote_setup_next_steps(&cfg);
    assert!(
        steps
            .iter()
            .any(|step| step.contains("coderight remote users")),
        "{steps:?}"
    );
    assert!(
        steps
            .iter()
            .any(|step| step.contains("coderight remote setup --allowed-user")),
        "{steps:?}"
    );
    assert!(
        !steps
            .iter()
            .any(|step| step.contains("remote smoke --timeout-seconds")),
        "{steps:?}"
    );
}

#[test]
fn remote_setup_next_steps_include_final_smoke_when_allowlist_ready() {
    let mut cfg = rightkit_app_settings::agent_config::RightKitConfig::default();
    cfg.remote.telegram_token = Some("token".into());
    cfg.remote.telegram_allowed_users = vec![123];

    let steps = remote_setup_next_steps(&cfg);
    assert!(
        steps
            .iter()
            .any(|step| step.contains("coderight remote test")),
        "{steps:?}"
    );
    assert!(
        steps
            .iter()
            .any(|step| step.contains("coderight remote smoke --timeout-seconds 120")),
        "{steps:?}"
    );
}
