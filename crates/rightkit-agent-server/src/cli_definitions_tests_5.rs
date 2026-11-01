#[test]
fn parses_session_subcommands() {
    let cli = parse(&[
        "coderight",
        "session",
        "list",
        "--status",
        "dormant",
        "--json",
    ]);
    match &cli.command {
        Command::Session {
            command: SessionCommand::List { status, tag, json },
        } => {
            assert_eq!(status.as_deref(), Some("dormant"));
            assert!(tag.is_none());
            assert!(*json);
        }
        other => panic!("expected session list, got {other:?}"),
    }

    let cli = parse(&["coderight", "session", "search", "billing", "--limit", "7"]);
    match &cli.command {
        Command::Session {
            command: SessionCommand::Search { query, limit },
        } => {
            assert_eq!(query, "billing");
            assert_eq!(*limit, 7);
        }
        other => panic!("expected session search, got {other:?}"),
    }

    let cli = parse(&["coderight", "session", "delete", "s1"]);
    match &cli.command {
        Command::Session {
            command: SessionCommand::Delete { session_id },
        } => assert_eq!(session_id, "s1"),
        other => panic!("expected session delete, got {other:?}"),
    }

    let cli = parse(&["coderight", "session", "archive", "s1"]);
    match &cli.command {
        Command::Session {
            command: SessionCommand::Archive { session_id },
        } => assert_eq!(session_id, "s1"),
        other => panic!("expected session archive, got {other:?}"),
    }

    let cli = parse(&["coderight", "session", "restore", "s1"]);
    match &cli.command {
        Command::Session {
            command: SessionCommand::Restore { session_id },
        } => assert_eq!(session_id, "s1"),
        other => panic!("expected session restore, got {other:?}"),
    }

    let cli = parse(&[
        "coderight",
        "session",
        "set",
        "s1",
        "--title",
        "Renamed",
        "--model",
        "coder",
        "--tag",
        "parity",
        "--tag",
        "cli",
    ]);
    match &cli.command {
        Command::Session {
            command:
                SessionCommand::Set {
                    session_id,
                    title,
                    model,
                    tags,
                    clear_tags,
                },
        } => {
            assert_eq!(session_id, "s1");
            assert_eq!(title.as_deref(), Some("Renamed"));
            assert_eq!(model.as_deref(), Some("coder"));
            assert_eq!(tags, &vec!["parity".to_string(), "cli".to_string()]);
            assert!(!clear_tags);
        }
        other => panic!("expected session set, got {other:?}"),
    }

    let cli = parse(&["coderight", "session", "export", "s1", "--json"]);
    match &cli.command {
        Command::Session {
            command: SessionCommand::Export { session_id, json },
        } => {
            assert_eq!(session_id, "s1");
            assert!(*json);
        }
        other => panic!("expected session export, got {other:?}"),
    }
}

#[test]
fn parses_logs() {
    let cli = parse(&["coderight", "logs", "abc123"]);
    match &cli.command {
        Command::Logs { session_id } => assert_eq!(session_id, "abc123"),
        other => panic!("expected Logs, got {other:?}"),
    }
}

#[test]
fn parses_attach() {
    let cli = parse(&["coderight", "attach", "sess-42"]);
    match &cli.command {
        Command::Attach { session_id } => assert_eq!(session_id, "sess-42"),
        other => panic!("expected Attach, got {other:?}"),
    }
}

#[test]
fn parses_resume() {
    let cli = parse(&["coderight", "resume", "sess-42"]);
    match &cli.command {
        Command::Resume {
            session_id, last, ..
        } => {
            assert_eq!(session_id.as_deref(), Some("sess-42"));
            assert!(!*last);
        }
        other => panic!("expected Resume, got {other:?}"),
    }

    let cli = parse(&["coderight", "resume", "--last", "--all", "--all-workspaces"]);
    match &cli.command {
        Command::Resume {
            session_id,
            last,
            all,
            all_workspaces,
        } => {
            assert!(session_id.is_none());
            assert!(*last);
            assert!(*all);
            assert!(*all_workspaces);
        }
        other => panic!("expected Resume, got {other:?}"),
    }
}

#[test]
fn parses_fork() {
    let cli = parse(&["coderight", "fork", "sess-42", "--json"]);
    match &cli.command {
        Command::Fork {
            session_id,
            last,
            json,
            ..
        } => {
            assert_eq!(session_id.as_deref(), Some("sess-42"));
            assert!(!*last);
            assert!(*json);
        }
        other => panic!("expected Fork, got {other:?}"),
    }

    let cli = parse(&[
        "coderight",
        "fork",
        "--last",
        "--all-workspaces",
        "--title",
        "Try B",
    ]);
    match &cli.command {
        Command::Fork {
            session_id,
            last,
            all_workspaces,
            title,
            ..
        } => {
            assert!(session_id.is_none());
            assert!(*last);
            assert!(*all_workspaces);
            assert_eq!(title.as_deref(), Some("Try B"));
        }
        other => panic!("expected Fork, got {other:?}"),
    }
}

#[test]
fn parses_conduct() {
    let cli = parse(&[
        "coderight",
        "conduct",
        "ship the feature",
        "--dry-run",
        "--workspace",
        "/tmp/ws",
    ]);
    match &cli.command {
        Command::Conduct {
            goal,
            dry_run,
            safe_mode,
            workspace,
        } => {
            assert_eq!(goal, "ship the feature");
            assert!(dry_run);
            assert!(!safe_mode);
            assert_eq!(workspace.as_deref(), Some("/tmp/ws"));
        }
        other => panic!("expected Conduct, got {other:?}"),
    }
}

#[test]
fn parses_completion() {
    let cli = parse(&["coderight", "completion", "bash"]);
    match &cli.command {
        Command::Completion { shell } => assert_eq!(*shell, CompletionShell::Bash),
        other => panic!("expected Completion, got {other:?}"),
    }
}

#[test]
fn parses_conduct_minimal() {
    let cli = parse(&["coderight", "conduct", "fix the bug"]);
    match &cli.command {
        Command::Conduct {
            goal,
            dry_run,
            safe_mode,
            workspace,
        } => {
            assert_eq!(goal, "fix the bug");
            assert!(!dry_run);
            assert!(!safe_mode);
            assert_eq!(workspace.as_deref(), None);
        }
        other => panic!("expected Conduct, got {other:?}"),
    }
}

#[test]
fn parses_conduct_safe_mode_alias() {
    let cli = parse(&["coderight", "conduct", "ship it", "--safe"]);
    match &cli.command {
        Command::Conduct { safe_mode, .. } => assert!(*safe_mode),
        other => panic!("expected Conduct, got {other:?}"),
    }
}

#[test]
fn parses_kill() {
    let cli = parse(&["coderight", "kill", "sess-99"]);
    match &cli.command {
        Command::Kill { session_id } => assert_eq!(session_id, "sess-99"),
        other => panic!("expected Kill, got {other:?}"),
    }
}

#[test]
fn parses_handoff_and_aliases() {
    for subcommand in ["handoff", "bridge", "ide", "desktop", "mobile", "teleport"] {
        let cli = parse(&["coderight", subcommand, "sess-bridge"]);
        match &cli.command {
            Command::Handoff { session_id } => assert_eq!(session_id, "sess-bridge"),
            other => panic!("expected Handoff for {subcommand}, got {other:?}"),
        }
    }
}

#[test]
fn parses_app_handoff_helpers() {
    let cli = parse(&["coderight", "app", "handoff", "sess-app"]);
    match &cli.command {
        Command::App {
            command: AppCommand::Handoff { session_id },
        } => assert_eq!(session_id, "sess-app"),
        other => panic!("expected app handoff, got {other:?}"),
    }

    let cli = parse(&["coderight", "app", "open", "sess-app"]);
    match &cli.command {
        Command::App {
            command: AppCommand::Open { session_id },
        } => assert_eq!(session_id, "sess-app"),
        other => panic!("expected app open, got {other:?}"),
    }
}

#[test]
fn logs_requires_session_id() {
    assert!(Cli::try_parse_from(["coderight", "logs"]).is_err());
}

#[test]
fn attach_requires_session_id() {
    assert!(Cli::try_parse_from(["coderight", "attach"]).is_err());
}

#[test]
fn kill_requires_session_id() {
    assert!(Cli::try_parse_from(["coderight", "kill"]).is_err());
}

#[test]
fn handoff_requires_session_id() {
    assert!(Cli::try_parse_from(["coderight", "handoff"]).is_err());
}

#[test]
fn parses_export_and_transcript_alias() {
    let cli = parse(&["coderight", "export", "sess-export", "--json"]);
    match &cli.command {
        Command::Export { session_id, json } => {
            assert_eq!(session_id, "sess-export");
            assert!(*json);
        }
        other => panic!("expected Export, got {other:?}"),
    }

    let cli = parse(&["coderight", "transcript", "sess-export"]);
    match &cli.command {
        Command::Export { session_id, json } => {
            assert_eq!(session_id, "sess-export");
            assert!(!*json);
        }
        other => panic!("expected Export alias, got {other:?}"),
    }
}

#[test]
fn export_requires_session_id() {
    assert!(Cli::try_parse_from(["coderight", "export"]).is_err());
}
