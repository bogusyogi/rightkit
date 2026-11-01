#[test]
fn parses_diff() {
    let cli = parse(&["coderight", "diff", "--staged", "D:/repo"]);
    match &cli.command {
        Command::Diff { workspace, staged } => {
            assert_eq!(workspace.as_deref(), Some("D:/repo"));
            assert!(*staged);
        }
        other => panic!("expected Diff, got {other:?}"),
    }

    let cli = parse(&["coderight", "diff"]);
    match &cli.command {
        Command::Diff { workspace, staged } => {
            assert!(workspace.is_none());
            assert!(!*staged);
        }
        other => panic!("expected Diff, got {other:?}"),
    }
}

#[test]
fn parses_review() {
    let cli = parse(&[
        "coderight",
        "review",
        "--model",
        "openrouter/test-model",
        "--provider",
        "openrouter",
        "--workspace",
        "D:/repo",
        "--base",
        "origin/main",
        "--dir",
        "engine",
        "--safe-mode",
        "--max-turns",
        "3",
        "--max-budget-usd",
        "0.25",
        "focus",
        "on",
        "security",
    ]);
    match &cli.command {
        Command::Review {
            focus,
            model,
            provider,
            workspace,
            base,
            dirs,
            safe_mode,
            max_turns,
            max_budget_usd,
            ..
        } => {
            assert_eq!(focus, &vec!["focus", "on", "security"]);
            assert_eq!(model.as_deref(), Some("openrouter/test-model"));
            assert_eq!(provider.as_deref(), Some("openrouter"));
            assert_eq!(workspace.as_deref(), Some("D:/repo"));
            assert_eq!(base.as_deref(), Some("origin/main"));
            assert_eq!(dirs, &vec!["engine".to_string()]);
            assert!(*safe_mode);
            assert_eq!(*max_turns, Some(3));
            assert!((max_budget_usd.unwrap() - 0.25).abs() < 1e-9);
        }
        other => panic!("expected Review, got {other:?}"),
    }
}

#[test]
fn parses_review_findings() {
    let cli = parse(&["coderight", "review", "findings", "sess-review", "--json"]);
    match &cli.command {
        Command::Review {
            focus,
            session,
            json,
            ..
        } => {
            assert_eq!(focus, &vec!["findings", "sess-review"]);
            assert!(session.is_none());
            assert!(*json);
        }
        other => panic!("expected Review findings, got {other:?}"),
    }

    let cli = parse(&[
        "coderight",
        "review",
        "findings",
        "--session",
        "sess-review",
    ]);
    match &cli.command {
        Command::Review { focus, session, .. } => {
            assert_eq!(focus, &vec!["findings"]);
            assert_eq!(session.as_deref(), Some("sess-review"));
        }
        other => panic!("expected Review findings, got {other:?}"),
    }
}

#[test]
fn parses_mcp_server() {
    let cli = parse(&[
        "coderight",
        "mcp-server",
        "--workspace",
        "D:/repo",
        "--allow-mutate",
    ]);
    match &cli.command {
        Command::McpServer {
            workspace,
            allow_mutate,
            session_id,
        } => {
            assert_eq!(workspace.as_deref(), Some("D:/repo"));
            assert!(*allow_mutate);
            assert_eq!(session_id, &None);
        }
        other => panic!("expected McpServer, got {other:?}"),
    }
}

#[test]
fn parses_session_bound_mcp_server_without_secret_arguments() {
    let cli = parse(&[
        "coderight",
        "mcp-server",
        "--workspace",
        "D:/repo",
        "--session-id",
        "sess-driver",
    ]);
    match &cli.command {
        Command::McpServer {
            session_id,
            allow_mutate,
            ..
        } => {
            assert_eq!(session_id.as_deref(), Some("sess-driver"));
            assert!(!allow_mutate);
        }
        other => panic!("expected McpServer, got {other:?}"),
    }
}

#[test]
fn parses_mcp_management_commands() {
    assert!(matches!(
        parse(&["coderight", "mcp", "list", "--json"]).command,
        Command::Mcp {
            command: McpCommand::List { json: true, .. }
        }
    ));

    let cli = parse(&[
        "coderight",
        "mcp",
        "get",
        "context7",
        "--workspace-root",
        "D:/ws",
    ]);
    match &cli.command {
        Command::Mcp {
            command:
                McpCommand::Get {
                    name,
                    workspace_root,
                },
        } => {
            assert_eq!(name, "context7");
            assert_eq!(workspace_root.as_deref(), Some("D:/ws"));
        }
        other => panic!("expected mcp get, got {other:?}"),
    }

    let cli = parse(&["coderight", "mcp", "add-json", "mcp.json"]);
    match &cli.command {
        Command::Mcp {
            command: McpCommand::AddJson { path },
        } => assert_eq!(path, "mcp.json"),
        other => panic!("expected mcp add-json, got {other:?}"),
    }

    let cli = parse(&["coderight", "mcp", "add", "mcp.json"]);
    match &cli.command {
        Command::Mcp {
            command: McpCommand::AddJson { path },
        } => assert_eq!(path, "mcp.json"),
        other => panic!("expected mcp add alias, got {other:?}"),
    }

    let cli = parse(&[
        "coderight",
        "mcp",
        "remove",
        "context7",
        "--scope",
        "workspace",
        "--workspace-root",
        "D:/ws",
    ]);
    match &cli.command {
        Command::Mcp {
            command:
                McpCommand::Remove {
                    name,
                    scope,
                    workspace_root,
                },
        } => {
            assert_eq!(name, "context7");
            assert_eq!(scope, "workspace");
            assert_eq!(workspace_root.as_deref(), Some("D:/ws"));
        }
        other => panic!("expected mcp remove, got {other:?}"),
    }

    assert!(matches!(
        parse(&["coderight", "mcp", "probe", "context7"]).command,
        Command::Mcp {
            command: McpCommand::Probe { .. }
        }
    ));
    assert!(matches!(
        parse(&["coderight", "mcp", "status"]).command,
        Command::Mcp {
            command: McpCommand::Status
        }
    ));
}

#[test]
fn parses_plugin_management_commands() {
    assert!(matches!(
        parse(&["coderight", "plugin", "list", "--json"]).command,
        Command::Plugin {
            command: PluginCommand::List { json: true }
        }
    ));
    assert!(matches!(
        parse(&["coderight", "plugin", "marketplace"]).command,
        Command::Plugin {
            command: PluginCommand::Marketplace
        }
    ));
    assert!(matches!(
        parse(&["coderight", "plugin", "reload"]).command,
        Command::Plugin {
            command: PluginCommand::Reload
        }
    ));

    let cli = parse(&["coderight", "plugin", "install", "D:/plugin", "--overwrite"]);
    match &cli.command {
        Command::Plugin {
            command:
                PluginCommand::Install {
                    source_path,
                    overwrite,
                },
        } => {
            assert_eq!(source_path, "D:/plugin");
            assert!(*overwrite);
        }
        other => panic!("expected plugin install, got {other:?}"),
    }

    let cli = parse(&[
        "coderight",
        "plugin",
        "install-marketplace",
        "tools.demo",
        "--expected-sha256",
        "abc",
        "--overwrite",
        "--allow-file-urls",
    ]);
    match &cli.command {
        Command::Plugin {
            command:
                PluginCommand::InstallMarketplace {
                    id,
                    expected_sha256,
                    overwrite,
                    allow_file_urls,
                },
        } => {
            assert_eq!(id, "tools.demo");
            assert_eq!(expected_sha256.as_deref(), Some("abc"));
            assert!(*overwrite);
            assert!(*allow_file_urls);
        }
        other => panic!("expected plugin install-marketplace, got {other:?}"),
    }

    let cli = parse(&[
        "coderight",
        "plugin",
        "marketplace-sync",
        "https://plugins.example/catalog.json",
        "--expected-sha256",
        "def",
        "--allow-unsigned",
        "--allow-file-urls",
    ]);
    match &cli.command {
        Command::Plugin {
            command:
                PluginCommand::MarketplaceSync {
                    catalog_url,
                    expected_sha256,
                    allow_unsigned,
                    allow_file_urls,
                },
        } => {
            assert_eq!(catalog_url, "https://plugins.example/catalog.json");
            assert_eq!(expected_sha256, "def");
            assert!(*allow_unsigned);
            assert!(*allow_file_urls);
        }
        other => panic!("expected plugin marketplace-sync, got {other:?}"),
    }

    assert!(matches!(
        parse(&["coderight", "plugin", "enable", "tools.demo"]).command,
        Command::Plugin {
            command: PluginCommand::Enable { .. }
        }
    ));
    assert!(matches!(
        parse(&["coderight", "plugin", "disable", "tools.demo"]).command,
        Command::Plugin {
            command: PluginCommand::Disable { .. }
        }
    ));
}

// --- MCP test/login parse tests (moved from cli_definitions_tests_5.rs) --------

#[test]
fn parses_mcp_test() {
    let args = Cli::parse_from(["coderight", "mcp", "test", "my-server"]);
    assert_eq!(
        args.command,
        Command::Mcp {
            command: McpCommand::Test {
                name: "my-server".to_string(),
                scope: "user".to_string(),
                workspace_root: None,
                json: false,
            },
        }
    );
}

#[test]
fn parses_mcp_test_json() {
    let args = Cli::parse_from(["coderight", "mcp", "test", "my-server", "--json"]);
    assert!(matches!(
        args.command,
        Command::Mcp {
            command: McpCommand::Test { json: true, .. },
        }
    ));
}

#[test]
fn parses_mcp_test_with_scope() {
    let args = Cli::parse_from([
        "coderight",
        "mcp",
        "test",
        "my-server",
        "--scope",
        "workspace",
        "--workspace-root",
        "/tmp",
    ]);
    assert!(matches!(
        args.command,
        Command::Mcp {
            command: McpCommand::Test { scope, .. },
        } if scope == "workspace"
    ));
}

#[test]
fn parses_mcp_login() {
    let args = Cli::parse_from(["coderight", "mcp", "login", "my-server"]);
    assert_eq!(
        args.command,
        Command::Mcp {
            command: McpCommand::Login {
                name: "my-server".to_string(),
                scope: "user".to_string(),
                workspace_root: None,
            },
        }
    );
}

#[test]
fn parses_mcp_login_with_scope() {
    let args = Cli::parse_from([
        "coderight",
        "mcp",
        "login",
        "my-server",
        "--scope",
        "workspace",
    ]);
    assert!(matches!(
        args.command,
        Command::Mcp {
            command: McpCommand::Login { scope, .. },
        } if scope == "workspace"
    ));
}
