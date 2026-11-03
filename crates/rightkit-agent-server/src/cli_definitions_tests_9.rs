#[test]
fn parses_mcp_catalog_command() {
    assert!(matches!(
        parse(&["coderight", "mcp", "catalog", "--json"]).command,
        Command::Mcp {
            command: McpCommand::Catalog { json: true }
        }
    ));

    assert!(matches!(
        parse(&["coderight", "mcp", "catalog"]).command,
        Command::Mcp {
            command: McpCommand::Catalog { json: false }
        }
    ));
}

#[test]
fn parses_mcp_install_command() {
    let cli = parse(&[
        "coderight",
        "mcp",
        "install",
        "context7",
        "--scope",
        "workspace",
        "--workspace-root",
        "D:/ws",
    ]);
    match &cli.command {
        Command::Mcp {
            command:
                McpCommand::Install {
                    name,
                    scope,
                    workspace_root,
                },
        } => {
            assert_eq!(name, "context7");
            assert_eq!(scope, "workspace");
            assert_eq!(workspace_root.as_deref(), Some("D:/ws"));
        }
        other => panic!("expected mcp install, got {other:?}"),
    }

    let cli = parse(&["coderight", "mcp", "install", "memory"]);
    match &cli.command {
        Command::Mcp {
            command:
                McpCommand::Install {
                    name,
                    scope,
                    workspace_root,
                },
        } => {
            assert_eq!(name, "memory");
            assert_eq!(scope, "user");
            assert!(workspace_root.is_none());
        }
        other => panic!("expected mcp install default scope, got {other:?}"),
    }
}
