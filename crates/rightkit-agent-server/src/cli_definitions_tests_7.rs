#[test]
fn parses_permission_management_commands() {
    let cli = parse(&["coderight", "permission", "mode", "sess-1", "accept-edits"]);
    match &cli.command {
        Command::Permission {
            command: PermissionCommand::Mode { session_id, mode },
        } => {
            assert_eq!(session_id, "sess-1");
            assert_eq!(mode, "accept-edits");
        }
        other => panic!("expected permission mode, got {other:?}"),
    }

    assert!(matches!(
        parse(&["coderight", "permissions", "rules", "sess-1"]).command,
        Command::Permission {
            command: PermissionCommand::Rules { .. }
        }
    ));

    let cli = parse(&[
        "coderight",
        "permission",
        "rule-add",
        "sess-1",
        "deny",
        "shell",
    ]);
    match &cli.command {
        Command::Permission {
            command:
                PermissionCommand::RuleAdd {
                    session_id,
                    effect,
                    pattern,
                },
        } => {
            assert_eq!(session_id, "sess-1");
            assert_eq!(effect, "deny");
            assert_eq!(pattern, "shell");
        }
        other => panic!("expected permission rule-add, got {other:?}"),
    }

    assert!(matches!(
        parse(&["coderight", "permission", "rule-remove", "sess-1", "0"]).command,
        Command::Permission {
            command: PermissionCommand::RuleRemove { index: 0, .. }
        }
    ));
}

#[test]
fn parses_provider_status() {
    let cli = parse(&["coderight", "provider", "status", "openrouter"]);
    match &cli.command {
        Command::Provider {
            command: ProviderCommand::Status { provider },
        } => assert_eq!(provider, "openrouter"),
        other => panic!("expected provider status, got {other:?}"),
    }
    assert!(Cli::try_parse_from(["coderight", "provider", "status", "bogus"]).is_err());
}

// S:H13: connect/rotate read secrets from stdin; argv never carries keys.
#[test]
fn parses_provider_secret_verbs_without_argv_secrets() {
    let cli = parse(&["coderight", "provider", "rotate", "openrouter"]);
    match &cli.command {
        Command::Provider {
            command: ProviderCommand::Rotate { provider },
        } => assert_eq!(provider, "openrouter"),
        other => panic!("expected provider rotate, got {other:?}"),
    }
    assert!(matches!(
        parse(&["coderight", "provider", "connect", "openrouter"]).command,
        Command::Provider {
            command: ProviderCommand::Connect { .. }
        }
    ));
    assert!(
        Cli::try_parse_from([
            "coderight",
            "provider",
            "rotate",
            "openrouter",
            "--api-key",
            "sk-x"
        ])
        .is_err(),
        "argv secret flag must be rejected"
    );
}

#[test]
fn parses_provider_test_verb() {
    assert!(matches!(
        parse(&["coderight", "provider", "test", "openrouter"]).command,
        Command::Provider {
            command: ProviderCommand::Test { .. }
        }
    ));
}

#[test]
fn parses_provider_disconnect() {
    let cli = parse(&["coderight", "provider", "disconnect", "openrouter"]);
    match &cli.command {
        Command::Provider {
            command: ProviderCommand::Disconnect { provider },
        } => assert_eq!(provider, "openrouter"),
        other => panic!("expected provider disconnect, got {other:?}"),
    }
    assert!(Cli::try_parse_from(["coderight", "provider", "disconnect", "bogus"]).is_err());
}

#[test]
fn parses_every_provider_verb_for_the_screen_providers() {
    for provider in [
        "openai",
        "claude-code",
        "antigravity",
        "opencode-go",
        "commandcode",
        "openrouter",
    ] {
        for verb in [
            "status",
            "login",
            "logout",
            "connect",
            "rotate",
            "disconnect",
            "test",
            "models",
        ] {
            assert!(
                Cli::try_parse_from(["coderight", "provider", verb, provider]).is_ok(),
                "provider {verb} {provider} must parse"
            );
        }
    }
    match &parse(&[
        "coderight",
        "provider",
        "set-visibility",
        "claude-code",
        "opus",
        "--visible=false",
    ])
    .command
    {
        Command::Provider {
            command:
                ProviderCommand::SetVisibility {
                    provider,
                    model,
                    visible,
                },
        } => {
            assert_eq!(provider, "claude-code");
            assert_eq!(model, "opus");
            assert!(!visible);
        }
        other => panic!("expected provider set-visibility, got {other:?}"),
    }
}

#[test]
fn parses_local_model_diagnostics() {
    assert!(matches!(
        parse(&["coderight", "local-model", "hardware"]).command,
        Command::LocalModel {
            command: LocalModelCommand::Hardware
        }
    ));
    assert!(matches!(
        parse(&["coderight", "local-models", "catalog"]).command,
        Command::LocalModel {
            command: LocalModelCommand::Catalog
        }
    ));
}

#[test]
fn parses_hooks_status() {
    assert!(matches!(
        parse(&["coderight", "hooks", "status"]).command,
        Command::Hooks {
            command: HooksCommand::Status
        }
    ));
    assert!(matches!(
        parse(&["coderight", "hook", "status"]).command,
        Command::Hooks {
            command: HooksCommand::Status
        }
    ));
}

#[test]
fn parses_toolset_diagnostics() {
    assert!(matches!(
        parse(&["coderight", "toolset", "list", "--json"]).command,
        Command::Toolset {
            command: ToolsetCommand::List { json: true }
        }
    ));
    assert!(matches!(
        parse(&["coderight", "toolsets", "gateways"]).command,
        Command::Toolset {
            command: ToolsetCommand::Gateways { json: false }
        }
    ));
}

#[test]
fn parses_execution_diagnostics() {
    assert!(matches!(
        parse(&["coderight", "execution", "backends", "--json"]).command,
        Command::Execution {
            command: ExecutionCommand::Backends {
                workspace_root: None,
                json: true,
            }
        }
    ));

    let cli = parse(&[
        "coderight",
        "execution",
        "doctor",
        "custom-backend",
        "--workspace-root",
        "D:/Claude/coderight",
    ]);
    match cli.command {
        Command::Execution {
            command:
                ExecutionCommand::Doctor {
                    backend_id,
                    workspace_root: Some(root),
                    json: false,
                },
        } => {
            assert_eq!(backend_id, "custom-backend");
            assert_eq!(root, "D:/Claude/coderight");
        }
        other => panic!("expected execution doctor, got {other:?}"),
    }
}

// ── Task 2: Run headless flags ────────────────────────────────────────────

#[test]
fn parses_run_allowed_tools_comma_separated() {
    let cli = parse(&[
        "coderight",
        "run",
        "do it",
        "--allowed-tools",
        "read_file,shell,web",
    ]);
    match &cli.command {
        Command::Run { ref args, .. } => {
            assert_eq!(args.allowed_tools, &["read_file", "shell", "web"]);
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn parses_run_disallowed_tools() {
    let cli = parse(&[
        "coderight",
        "run",
        "do it",
        "--disallowed-tools",
        "shell,web_search",
    ]);
    match &cli.command {
        Command::Run { ref args, .. } => {
            assert_eq!(args.disallowed_tools, &["shell", "web_search"]);
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn parses_run_toolset() {
    let cli = parse(&["coderight", "run", "do it", "--toolset", "dev-safe"]);
    match &cli.command {
        Command::Run { ref args, .. } => {
            assert_eq!(args.toolset.as_deref(), Some("dev-safe"));
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn parses_run_max_budget_usd() {
    let cli = parse(&["coderight", "run", "do it", "--max-budget-usd", "1.50"]);
    match &cli.command {
        Command::Run { ref args, .. } => {
            assert!((args.max_budget_usd.unwrap() - 1.50).abs() < 1e-9);
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn parses_run_system_prompt() {
    let cli = parse(&[
        "coderight",
        "run",
        "do it",
        "--system-prompt",
        "You are helpful.",
    ]);
    match &cli.command {
        Command::Run { ref args, .. } => {
            assert_eq!(args.system_prompt.as_deref(), Some("You are helpful."));
            assert!(args.system_prompt_file.is_none());
            assert!(args.append_system_prompt.is_none());
            assert!(args.append_system_prompt_file.is_none());
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn parses_run_system_prompt_file_and_append_flags() {
    let cli = parse(&[
        "coderight",
        "run",
        "do it",
        "--system-prompt-file",
        "system.md",
        "--append-system-prompt",
        "Extra guidance.",
    ]);
    match &cli.command {
        Command::Run { ref args, .. } => {
            assert!(args.system_prompt.is_none());
            assert_eq!(args.system_prompt_file.as_deref(), Some("system.md"));
            assert_eq!(
                args.append_system_prompt.as_deref(),
                Some("Extra guidance.")
            );
            assert!(args.append_system_prompt_file.is_none());
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn parses_run_mcp_config() {
    let cli = parse(&["coderight", "run", "do it", "--mcp-config", "/tmp/mcp.json"]);
    match &cli.command {
        Command::Run { ref args, .. } => {
            assert_eq!(args.mcp_config.as_deref(), Some("/tmp/mcp.json"));
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn parses_run_session_id_without_prompt() {
    let cli = parse(&["coderight", "run", "--session-id", "existing-session"]);
    match &cli.command {
        Command::Run { ref args, .. } => {
            assert_eq!(args.session_id.as_deref(), Some("existing-session"));
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn parses_run_add_dir_multiple() {
    let cli = parse(&[
        "coderight",
        "run",
        "do it",
        "--add-dir",
        "/home/user/docs",
        "--add-dir",
        "/home/user/extra",
    ]);
    match &cli.command {
        Command::Run { ref args, .. } => {
            assert_eq!(args.add_dir, &["/home/user/docs", "/home/user/extra"]);
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn parses_run_all_new_flags_together() {
    let cli = parse(&[
        "coderight",
        "run",
        "fix bug",
        "--allowed-tools",
        "read_file,shell",
        "--disallowed-tools",
        "web_search",
        "--toolset",
        "dev-safe",
        "--max-budget-usd",
        "2.0",
        "--exit-on-error",
        "--bare",
        "--bypass-approvals",
        "--system-prompt",
        "Be concise.",
        "--mcp-config",
        "/etc/mcp.json",
        "--session-id",
        "sess-1",
        "--add-dir",
        "/extra",
    ]);
    match &cli.command {
        Command::Run { ref args, .. } => {
            assert_eq!(args.prompt.as_deref(), Some("fix bug"));
            assert_eq!(args.allowed_tools, &["read_file", "shell"]);
            assert_eq!(args.disallowed_tools, &["web_search"]);
            assert_eq!(args.toolset.as_deref(), Some("dev-safe"));
            assert!((args.max_budget_usd.unwrap() - 2.0).abs() < 1e-9);
            assert!(args.exit_on_error);
            assert!(args.bare);
            assert!(args.bypass_approvals);
            assert_eq!(args.system_prompt.as_deref(), Some("Be concise."));
            assert!(args.system_prompt_file.is_none());
            assert!(args.append_system_prompt.is_none());
            assert!(args.append_system_prompt_file.is_none());
            assert_eq!(args.mcp_config.as_deref(), Some("/etc/mcp.json"));
            assert_eq!(args.session_id.as_deref(), Some("sess-1"));
            assert_eq!(args.add_dir, &["/extra"]);
        }
        other => panic!("expected Run, got {other:?}"),
    }
}
