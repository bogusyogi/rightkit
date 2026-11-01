#[test]
fn parses_hooks_list() {
    assert!(matches!(
        parse(&["coderight", "hooks", "list"]).command,
        Command::Hooks {
            command: HooksCommand::List
        }
    ));
    assert!(matches!(
        parse(&["coderight", "hook", "list"]).command,
        Command::Hooks {
            command: HooksCommand::List
        }
    ));
}

#[test]
fn parses_hooks_test_with_optional_matcher() {
    let cli = parse(&["coderight", "hooks", "test", "PreToolUse"]);
    match &cli.command {
        Command::Hooks {
            command: HooksCommand::Test { event, matcher },
        } => {
            assert_eq!(event, "PreToolUse");
            assert!(matcher.is_none());
        }
        other => panic!("expected HooksCommand::Test, got {other:?}"),
    }
    let cli = parse(&[
        "coderight",
        "hooks",
        "test",
        "Notification",
        "--matcher",
        "NOTI",
    ]);
    match &cli.command {
        Command::Hooks {
            command: HooksCommand::Test { event, matcher },
        } => {
            assert_eq!(event, "Notification");
            assert_eq!(matcher.as_deref(), Some("NOTI"));
        }
        other => panic!("expected HooksCommand::Test with matcher, got {other:?}"),
    }
}

#[test]
fn parses_hooks_revoke_with_optional_restore() {
    let cli = parse(&[
        "coderight",
        "hooks",
        "revoke",
        "RIGHTKIT_HOOK_PRE_TOOL_USE",
    ]);
    match &cli.command {
        Command::Hooks {
            command: HooksCommand::Revoke { id, restore },
        } => {
            assert_eq!(id, "RIGHTKIT_HOOK_PRE_TOOL_USE");
            assert!(!restore);
        }
        other => panic!("expected HooksCommand::Revoke, got {other:?}"),
    }
    let cli = parse(&[
        "coderight",
        "hooks",
        "revoke",
        "RIGHTKIT_HOOK_PRE_TOOL_USE",
        "--restore",
    ]);
    match &cli.command {
        Command::Hooks {
            command: HooksCommand::Revoke { id, restore },
        } => {
            assert_eq!(id, "RIGHTKIT_HOOK_PRE_TOOL_USE");
            assert!(*restore);
        }
        other => panic!("expected HooksCommand::Revoke with restore, got {other:?}"),
    }
}

#[test]
fn parses_run_safe_mode() {
    let cli = parse(&[
        "coderight",
        "run",
        "audit workspace",
        "--safe-mode",
        "--max-budget-usd",
        "0.25",
    ]);
    match &cli.command {
        Command::Run { ref args, .. } => {
            assert!(args.safe_mode, "--safe-mode must map to run safe_mode=true");
            assert!((args.max_budget_usd.unwrap() - 0.25).abs() < 1e-9);
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn parses_run_bare() {
    let cli = parse(&["coderight", "run", "do it", "--bare"]);
    match &cli.command {
        Command::Run { ref args, .. } => assert!(args.bare),
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn parses_run_safe_mode_default_false() {
    let cli = parse(&["coderight", "run", "do it"]);
    match &cli.command {
        Command::Run { ref args, .. } => assert!(!args.safe_mode),
        other => panic!("expected Run, got {other:?}"),
    }
}
