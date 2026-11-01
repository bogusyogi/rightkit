#[test]
fn parses_learning_approval_commands() {
    match parse(&["coderight", "learning", "approve", "proposal-1"]).command {
        Command::Learning {
            command: LearningCommand::Approve { proposal_id },
        } => assert_eq!(proposal_id, "proposal-1"),
        other => panic!("expected learning approve, got {other:?}"),
    }

    match parse(&["coderight", "learning", "reject", "proposal-2"]).command {
        Command::Learning {
            command: LearningCommand::Reject { proposal_id },
        } => assert_eq!(proposal_id, "proposal-2"),
        other => panic!("expected learning reject, got {other:?}"),
    }
}

#[test]
fn parses_run_prompt() {
    let cli = parse(&["coderight", "run", "fix the bug"]);
    match &cli.command {
        Command::Run { ref args, .. } => {
            assert_eq!(args.prompt.as_deref(), Some("fix the bug"));
            assert!(args.model.is_none());
            assert_eq!(args.provider, None);
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn parses_exec_alias() {
    let cli = parse(&["coderight", "exec", "fix the bug"]);
    match &cli.command {
        Command::Run { ref args, .. } => assert_eq!(args.prompt.as_deref(), Some("fix the bug")),
        other => panic!("expected Run through exec alias, got {other:?}"),
    }
}

#[test]
fn top_level_help_shows_exec_alias() {
    let mut cmd = Cli::command();
    let mut help = Vec::new();
    cmd.write_help(&mut help).unwrap();
    let help = String::from_utf8(help).unwrap();

    assert!(help.contains("run"), "{help}");
    assert!(help.contains("exec"), "{help}");
}

#[test]
fn parses_run_with_model_and_provider() {
    let cli = parse(&[
        "coderight",
        "run",
        "hello",
        "--model",
        "deepseek/deepseek-v4-pro",
        "--provider",
        "openrouter",
    ]);
    match &cli.command {
        Command::Run { ref args, .. } => {
            assert_eq!(args.prompt.as_deref(), Some("hello"));
            assert_eq!(args.model.as_deref(), Some("deepseek/deepseek-v4-pro"));
            assert_eq!(args.provider.as_deref(), Some("openrouter"));
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn parses_run_prompt_file_flags_and_rejects_conflicting_forms_at_runtime() {
    let cli = parse(&[
        "coderight",
        "run",
        "go",
        "--system-prompt-file",
        "sys.md",
        "--append-system-prompt-file",
        "app.md",
        "--max-turns",
        "4",
        "--max-budget-usd",
        "0.5",
        "--mcp-config",
        "m.json",
        "--add-dir",
        "a",
        "--bare",
    ]);
    match &cli.command {
        Command::Run { ref args, .. } => {
            assert_eq!(args.system_prompt_file.as_deref(), Some("sys.md"));
            assert_eq!(args.append_system_prompt_file.as_deref(), Some("app.md"));
            assert_eq!(args.max_turns, Some(4));
            assert_eq!(args.mcp_config.as_deref(), Some("m.json"));
            assert_eq!(args.add_dir, &["a"]);
            assert!(args.bare);
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn parses_run_with_max_turns() {
    let cli = parse(&["coderight", "run", "do the thing", "--max-turns", "10"]);
    match &cli.command {
        Command::Run { ref args, .. } => {
            assert_eq!(args.prompt.as_deref(), Some("do the thing"));
            assert_eq!(args.max_turns, Some(10));
            assert!(!args.pipe);
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn parses_run_with_pipe_flag() {
    let cli = parse(&["coderight", "run", "--pipe"]);
    match &cli.command {
        Command::Run { ref args, .. } => {
            assert!(args.prompt.is_none());
            assert!(args.pipe);
            assert!(args.max_turns.is_none());
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn parses_run_print_alias_without_prompt() {
    let cli = parse(&["coderight", "run", "-p"]);
    match &cli.command {
        Command::Run { ref args, .. } => {
            assert!(args.print);
            assert!(args.prompt.is_none());
        }
        other => panic!("expected Run, got {other:?}"),
    }

    let cli = parse(&["coderight", "run", "--print", "hello"]);
    match &cli.command {
        Command::Run { ref args, .. } => {
            assert!(args.print);
            assert_eq!(args.prompt.as_deref(), Some("hello"));
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn parses_run_exit_on_error_and_bare() {
    let cli = parse(&["coderight", "run", "do it", "--exit-on-error", "--bare"]);
    match &cli.command {
        Command::Run { ref args, .. } => {
            assert!(args.exit_on_error);
            assert!(args.bare);
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn run_requires_prompt_or_pipe() {
    assert!(Cli::try_parse_from(["coderight", "run"]).is_err());
}

#[test]
fn bare_service_verbs_are_rejected() {
    assert!(Cli::try_parse_from(["coderight", "install-service"]).is_err());
    assert!(Cli::try_parse_from(["coderight", "uninstall-service"]).is_err());
    assert!(Cli::try_parse_from(["coderight", "install"]).is_err());
    assert!(Cli::try_parse_from(["coderight", "uninstall"]).is_err());
}

#[test]
fn parses_status_and_config() {
    assert!(matches!(
        parse(&["coderight", "status"]).command,
        Command::Status { json: false }
    ));
    assert!(matches!(
        parse(&["coderight", "status", "--json"]).command,
        Command::Status { json: true }
    ));
    assert!(matches!(
        parse(&["coderight", "info", "--json"]).command,
        Command::Info { json: true }
    ));
    assert!(matches!(
        parse(&["coderight", "config"]).command,
        Command::Config
    ));
}

#[test]
fn parses_caps() {
    assert!(matches!(
        parse(&["coderight", "caps"]).command,
        Command::Caps { json: false }
    ));
    assert!(matches!(
        parse(&["coderight", "caps", "--json"]).command,
        Command::Caps { json: true }
    ));
}

#[test]
fn parses_diagnostics_bundle_zip_output() {
    let cli = parse(&[
        "coderight",
        "diagnostics",
        "bundle",
        "--output",
        "bundle.zip",
    ]);
    match &cli.command {
        Command::Diagnostics {
            command: DiagnosticsCommand::Bundle { output },
        } => assert_eq!(output, "bundle.zip"),
        other => panic!("expected diagnostics bundle, got {other:?}"),
    }
}

#[test]
fn unknown_subcommand_errors() {
    assert!(Cli::try_parse_from(["coderight", "frobnicate"]).is_err());
}

// ── Task 1: background-session subcommands ────────────────────────────────

#[test]
fn parses_ps() {
    assert!(matches!(
        parse(&["coderight", "ps"]).command,
        Command::Ps { json: false }
    ));
    assert!(matches!(
        parse(&["coderight", "ps", "--json"]).command,
        Command::Ps { json: true }
    ));
}

#[test]
fn parses_sessions_alias() {
    assert!(matches!(
        parse(&["coderight", "sessions"]).command,
        Command::Sessions { json: false }
    ));
    assert!(matches!(
        parse(&["coderight", "sessions", "--json"]).command,
        Command::Sessions { json: true }
    ));
}

#[test]
fn parses_agents() {
    let cli = parse(&["coderight", "agents", "--json", "--all", "--cwd"]);
    match &cli.command {
        Command::Agents {
            session_id,
            json,
            all,
            cwd,
        } => {
            assert!(session_id.is_none());
            assert!(*json);
            assert!(*all);
            assert!(*cwd);
        }
        other => panic!("expected agents list, got {other:?}"),
    }

    let cli = parse(&["coderight", "agents", "sess-1"]);
    match &cli.command {
        Command::Agents {
            session_id,
            json,
            all,
            cwd,
        } => {
            assert_eq!(session_id.as_deref(), Some("sess-1"));
            assert!(!json);
            assert!(!all);
            assert!(!cwd);
        }
        other => panic!("expected agents session, got {other:?}"),
    }
}
