#[test]
fn parses_spec_subcommands() {
    let cli = parse(&[
        "coderight",
        "spec",
        "list",
        "--workspace",
        "D:/work",
        "--json",
    ]);
    match &cli.command {
        Command::Spec {
            command: SpecCommand::List {
                workspace, json, ..
            },
        } => {
            assert_eq!(workspace.as_deref(), Some("D:/work"));
            assert!(*json);
        }
        other => panic!("expected spec list, got {other:?}"),
    }

    let cli = parse(&[
        "coderight",
        "spec",
        "new",
        "--workspace",
        "D:/work",
        "--description",
        "Build the spec command surface",
        "--title",
        "Spec commands",
    ]);
    match &cli.command {
        Command::Spec {
            command:
                SpecCommand::New {
                    workspace,
                    description,
                    title,
                    ..
                },
        } => {
            assert_eq!(workspace.as_deref(), Some("D:/work"));
            assert_eq!(description, "Build the spec command surface");
            assert_eq!(title.as_deref(), Some("Spec commands"));
        }
        other => panic!("expected spec new, got {other:?}"),
    }

    assert!(matches!(
        parse(&["coderight", "spec", "analyze", "spec-1", "--json"]).command,
        Command::Spec {
            command: SpecCommand::Analyze { .. }
        }
    ));
    assert!(matches!(
        parse(&["coderight", "spec", "trace", "spec-1"]).command,
        Command::Spec {
            command: SpecCommand::Trace { .. }
        }
    ));
    assert!(matches!(
        parse(&["coderight", "spec", "status", "spec-1"]).command,
        Command::Spec {
            command: SpecCommand::Status { .. }
        }
    ));
    assert!(matches!(
        parse(&["coderight", "spec", "clarify", "spec-1"]).command,
        Command::Spec {
            command: SpecCommand::Clarify { .. }
        }
    ));
    match parse(&[
        "coderight",
        "spec",
        "plan",
        "spec-1",
        "--constraints",
        "parallel lanes",
        "--no-verification",
    ])
    .command
    {
        Command::Spec {
            command:
                SpecCommand::Plan {
                    spec_id,
                    constraints,
                    no_verification,
                },
        } => {
            assert_eq!(spec_id, "spec-1");
            assert_eq!(constraints.as_deref(), Some("parallel lanes"));
            assert!(no_verification);
        }
        other => panic!("expected spec plan, got {other:?}"),
    }
    assert!(matches!(
        parse(&["coderight", "spec", "checklist", "spec-1", "--focus", "qa"]).command,
        Command::Spec {
            command: SpecCommand::Checklist { .. }
        }
    ));
    match parse(&[
        "coderight",
        "spec",
        "implement",
        "spec-1",
        "--dry-run",
        "--title",
        "Implement login",
    ])
    .command
    {
        Command::Spec {
            command:
                SpecCommand::Implement {
                    spec_id,
                    title,
                    dry_run,
                },
        } => {
            assert_eq!(spec_id, "spec-1");
            assert_eq!(title.as_deref(), Some("Implement login"));
            assert!(dry_run);
        }
        other => panic!("expected spec implement, got {other:?}"),
    }
}
