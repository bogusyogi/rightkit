use clap::CommandFactory;

fn parse(args: &[&str]) -> Cli {
    Cli::try_parse_from(args).unwrap()
}

#[test]
fn parses_serve_default_port() {
    let cli = parse(&["coderight", "serve"]);
    match &cli.command {
        Command::Serve {
            port,
            db,
            model,
            provider,
            context_mode,
            resident,
        } => {
            assert_eq!(*port, 0);
            assert!(db.is_none());
            assert!(model.is_none());
            assert!(provider.is_none());
            assert_eq!(*context_mode, ContextMode::Membrane);
            assert!(!resident);
        }
        other => panic!("expected Serve, got {other:?}"),
    }
}

#[test]
fn parses_explicit_direct_context_mode() {
    let cli = parse(&["coderight", "run", "hello", "--context-mode", "direct"]);
    match &cli.command {
        Command::Run { args } => assert_eq!(args.context_mode, ContextMode::Direct),
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn parses_serve_with_port() {
    let cli = parse(&["coderight", "serve", "--port", "8080"]);
    match &cli.command {
        Command::Serve { port, .. } => assert_eq!(*port, 8080),
        other => panic!("expected Serve, got {other:?}"),
    }
}

#[test]
fn parses_remote_subcommands() {
    let cli = parse(&["coderight", "remote", "status"]);
    match &cli.command {
        Command::Remote {
            command: RemoteCommand::Status,
        } => {}
        other => panic!("expected remote status, got {other:?}"),
    }

    let cli = parse(&["coderight", "remote", "test"]);
    match &cli.command {
        Command::Remote {
            command: RemoteCommand::Test,
        } => {}
        other => panic!("expected remote test, got {other:?}"),
    }

    let cli = parse(&[
        "coderight",
        "remote",
        "setup",
        "--telegram-token",
        "123:abc",
        "--allowed-user",
        "123,456",
        "--allowed-user",
        "789",
        "--enable-web",
    ]);
    match &cli.command {
        Command::Remote {
            command:
                RemoteCommand::Setup {
                    telegram_token,
                    allowed_users,
                    clear_token,
                    enable_web,
                    disable_web,
                },
        } => {
            assert_eq!(telegram_token.as_deref(), Some("123:abc"));
            assert_eq!(allowed_users, &vec![123, 456, 789]);
            assert!(!clear_token);
            assert!(*enable_web);
            assert!(!disable_web);
        }
        other => panic!("expected remote setup, got {other:?}"),
    }

    let cli = parse(&[
        "coderight",
        "remote",
        "validate",
        "--telegram-token",
        "123:abc",
        "--api-base",
        "http://127.0.0.1:3000",
    ]);
    match &cli.command {
        Command::Remote {
            command:
                RemoteCommand::Validate {
                    telegram_token,
                    api_base,
                },
        } => {
            assert_eq!(telegram_token.as_deref(), Some("123:abc"));
            assert_eq!(api_base, "http://127.0.0.1:3000");
        }
        other => panic!("expected remote validate, got {other:?}"),
    }

    let cli = parse(&[
        "coderight",
        "remote",
        "users",
        "--telegram-token",
        "123:abc",
        "--api-base",
        "http://127.0.0.1:3000",
        "--limit",
        "5",
    ]);
    match &cli.command {
        Command::Remote {
            command:
                RemoteCommand::Users {
                    telegram_token,
                    api_base,
                    limit,
                },
        } => {
            assert_eq!(telegram_token.as_deref(), Some("123:abc"));
            assert_eq!(api_base, "http://127.0.0.1:3000");
            assert_eq!(*limit, 5);
        }
        other => panic!("expected remote users, got {other:?}"),
    }

    let cli = parse(&[
        "coderight",
        "remote",
        "smoke",
        "--timeout-seconds",
        "30",
        "--poll-interval-seconds",
        "2",
        "--no-require-chat",
    ]);
    match &cli.command {
        Command::Remote {
            command:
                RemoteCommand::Smoke {
                    timeout_seconds,
                    poll_interval_seconds,
                    no_require_chat,
                },
        } => {
            assert_eq!(*timeout_seconds, 30);
            assert_eq!(*poll_interval_seconds, 2);
            assert!(*no_require_chat);
        }
        other => panic!("expected remote smoke, got {other:?}"),
    }
}

#[test]
fn parses_recipe_subcommands() {
    let cli = parse(&["coderight", "recipe", "list", "--json"]);
    match &cli.command {
        Command::Recipe {
            command: RecipeCommand::List { json, store },
        } => {
            assert!(*json);
            assert!(store.is_none());
        }
        other => panic!("expected recipe list, got {other:?}"),
    }

    let cli = parse(&["coderight", "recipe", "validate", "ship-feature.yaml"]);
    match &cli.command {
        Command::Recipe {
            command: RecipeCommand::Validate { target, .. },
        } => assert_eq!(target, "ship-feature.yaml"),
        other => panic!("expected recipe validate, got {other:?}"),
    }

    let cli = parse(&[
        "coderight",
        "recipe",
        "render",
        "ship-feature",
        "-p",
        "feature=recipes",
        "--json",
    ]);
    match &cli.command {
        Command::Recipe {
            command:
                RecipeCommand::Render {
                    target,
                    params,
                    json,
                    ..
                },
        } => {
            assert_eq!(target, "ship-feature");
            assert_eq!(params, &vec!["feature=recipes".to_string()]);
            assert!(*json);
        }
        other => panic!("expected recipe render, got {other:?}"),
    }

    let cli = parse(&[
        "coderight",
        "recipe",
        "run",
        "ship-feature",
        "-p",
        "feature=recipes",
        "--model",
        "openai/gpt-5",
        "--yes",
        "--toolset",
        "coding-local",
    ]);
    match &cli.command {
        Command::Recipe {
            command:
                RecipeCommand::Run {
                    target,
                    params,
                    model,
                    auto_approve,
                    toolset,
                    ..
                },
        } => {
            assert_eq!(target, "ship-feature");
            assert_eq!(params, &vec!["feature=recipes".to_string()]);
            assert_eq!(model.as_deref(), Some("openai/gpt-5"));
            assert!(*auto_approve);
            assert_eq!(toolset.as_deref(), Some("coding-local"));
        }
        other => panic!("expected recipe run, got {other:?}"),
    }

    let cli = parse(&[
        "coderight",
        "recipe",
        "deeplink",
        "ship-feature",
        "--action",
        "render",
        "-p",
        "feature=recipes",
    ]);
    match &cli.command {
        Command::Recipe {
            command:
                RecipeCommand::Deeplink {
                    target,
                    action,
                    params,
                    ..
                },
        } => {
            assert_eq!(target, "ship-feature");
            assert_eq!(action, "render");
            assert_eq!(params, &vec!["feature=recipes".to_string()]);
        }
        other => panic!("expected recipe deeplink, got {other:?}"),
    }
}

#[test]
fn parses_schedule_management_commands() {
    assert!(matches!(
        parse(&["coderight", "schedule", "list", "--json"]).command,
        Command::Schedule {
            command: ScheduleCommand::List { json: true, .. }
        }
    ));
    assert!(matches!(
        parse(&["coderight", "schedules", "audit"]).command,
        Command::Schedule {
            command: ScheduleCommand::Audit { .. }
        }
    ));

    let cli = parse(&[
        "coderight",
        "schedule",
        "pause",
        "daily-review",
        "--workspace-root",
        "D:/work",
    ]);
    match &cli.command {
        Command::Schedule {
            command: ScheduleCommand::Pause { id, workspace_root },
        } => {
            assert_eq!(id, "daily-review");
            assert_eq!(workspace_root.as_deref(), Some("D:/work"));
        }
        other => panic!("expected schedule pause, got {other:?}"),
    }

    assert!(matches!(
        parse(&["coderight", "schedule", "resume", "daily-review"]).command,
        Command::Schedule {
            command: ScheduleCommand::Resume { .. }
        }
    ));
    assert!(matches!(
        parse(&["coderight", "schedule", "run", "daily-review"]).command,
        Command::Schedule {
            command: ScheduleCommand::Run { .. }
        }
    ));
    assert!(matches!(
        parse(&["coderight", "schedule", "runs", "daily-review"]).command,
        Command::Schedule {
            command: ScheduleCommand::Runs { .. }
        }
    ));
}
