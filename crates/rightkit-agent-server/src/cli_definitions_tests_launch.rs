// Top-level launch flags (`LaunchCli`): permission modes, `--model`, `-p`.

fn launch(args: &[&str]) -> LaunchCli {
    LaunchCli::try_parse_from(args).unwrap_or_else(|error| panic!("{args:?}: {error}"))
}

#[test]
fn launch_permission_flags_map_to_modes() {
    use rightkit_agent_policy::PermissionMode;
    assert_eq!(launch(&["coderight"]).permission_mode(), None);
    for (flag, mode) in [
        ("--ask", PermissionMode::Ask),
        ("--accept-edits", PermissionMode::AcceptEdits),
        ("--auto", PermissionMode::AutoSafe),
        ("--yolo", PermissionMode::Bypass),
    ] {
        assert_eq!(
            launch(&["coderight", flag]).permission_mode(),
            Some(mode),
            "{flag}"
        );
    }
}

#[test]
fn launch_permission_flags_are_mutually_exclusive() {
    let flags = ["--ask", "--accept-edits", "--auto", "--yolo"];
    for (i, first) in flags.iter().enumerate() {
        for second in &flags[i + 1..] {
            let error = LaunchCli::try_parse_from(["coderight", first, second])
                .expect_err("two permission flags must conflict");
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::ArgumentConflict,
                "{first} {second}"
            );
        }
    }
}

#[test]
fn launch_model_and_print_parse_and_combine() {
    let parsed = launch(&[
        "coderight",
        "--model",
        "openai/gpt-x",
        "--yolo",
        "-p",
        "fix it",
    ]);
    assert_eq!(parsed.model.as_deref(), Some("openai/gpt-x"));
    assert_eq!(parsed.print.as_deref(), Some("fix it"));
    assert!(parsed.yolo);

    let parsed = launch(&["coderight", "--print", "hello", "--model=gpt-x", "--plain"]);
    assert_eq!(parsed.print.as_deref(), Some("hello"));
    assert_eq!(parsed.model.as_deref(), Some("gpt-x"));
    assert!(parsed.plain && !parsed.reduced_motion);

    assert!(LaunchCli::try_parse_from(["coderight", "-p"]).is_err());
    assert!(LaunchCli::try_parse_from(["coderight", "--model"]).is_err());
    assert!(LaunchCli::try_parse_from(["coderight", "--context-mode", "bogus"]).is_err());
}

#[test]
fn launch_image_flag_repeats_and_parses() {
    let parsed = launch(&["coderight", "--image", "a.png", "--image", "b.jpg", "look"]);
    assert_eq!(parsed.image, vec!["a.png".to_string(), "b.jpg".to_string()]);
    assert_eq!(parsed.prompt.as_deref(), Some("look"));
    assert!(LaunchCli::try_parse_from(["coderight", "--image"]).is_err());
}

#[test]
fn subcommands_keep_owning_their_own_model_and_permission_flags() {
    // `coderight run --model ...` stays the `run` subcommand's flag, unaffected
    // by the launch flags.
    let cli = Cli::try_parse_from([
        "coderight",
        "run",
        "--model",
        "m",
        "--bypass-approvals",
        "hi",
    ])
    .unwrap();
    assert!(matches!(cli.command, Command::Run { .. }));
}

fn run_args_p12(args: &[&str]) -> cli_definitions_command_run::RunArgs {
    match Cli::try_parse_from(args).expect("parse").command {
        Command::Run { args } => args,
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn p12_run_ephemeral_and_no_session_parse() {
    assert!(run_args_p12(&["coderight", "run", "hi", "--ephemeral"]).ephemeral);
    assert!(run_args_p12(&["coderight", "run", "hi", "--no-session"]).no_session);
    let d = run_args_p12(&["coderight", "run", "hi"]);
    assert!(!d.ephemeral && !d.no_session);
}

#[test]
fn p12_run_ephemeral_conflicts_with_session_id() {
    assert!(Cli::try_parse_from([
        "coderight",
        "run",
        "hi",
        "--ephemeral",
        "--session-id",
        "s1"
    ])
    .is_err());
    assert!(Cli::try_parse_from([
        "coderight",
        "run",
        "hi",
        "--no-session",
        "--session-id",
        "s1"
    ])
    .is_err());
}

#[test]
fn p12_run_output_last_message_short_and_long() {
    assert_eq!(
        run_args_p12(&["coderight", "run", "hi", "-o", "out.txt"])
            .output_last_message
            .as_deref(),
        Some("out.txt")
    );
    assert_eq!(
        run_args_p12(&["coderight", "run", "hi", "--output-last-message", "o2.txt"])
            .output_last_message
            .as_deref(),
        Some("o2.txt")
    );
    assert!(run_args_p12(&["coderight", "run", "hi"])
        .output_last_message
        .is_none());
}

#[test]
fn p12_run_color_values_and_rejection() {
    assert_eq!(
        run_args_p12(&["coderight", "run", "hi"]).color,
        CliColor::Auto
    );
    assert_eq!(
        run_args_p12(&["coderight", "run", "hi", "--color", "always"]).color,
        CliColor::Always
    );
    assert_eq!(
        run_args_p12(&["coderight", "run", "hi", "--color", "never"]).color,
        CliColor::Never
    );
    assert!(Cli::try_parse_from(["coderight", "run", "hi", "--color", "rainbow"]).is_err());
}

// P-17: pi-style `--tools` / `--exclude-tools` / `--no-mcp`.
// Semantics: https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/docs/cli.md

#[test]
fn p17_run_tool_filter_flags_parse_comma_lists() {
    let args = run_args_p12(&[
        "coderight",
        "run",
        "hi",
        "--tools",
        "read_file,mcp__radius__*",
        "--exclude-tools",
        "write_*,shell",
        "--no-mcp",
    ]);
    assert_eq!(args.tools, vec!["read_file", "mcp__radius__*"]);
    assert_eq!(args.exclude_tools, vec!["write_*", "shell"]);
    assert!(args.no_mcp);
    let d = run_args_p12(&["coderight", "run", "hi"]);
    assert!(d.tools.is_empty() && d.exclude_tools.is_empty() && !d.no_mcp);
}

#[test]
fn p17_run_tool_filter_flags_require_values() {
    assert!(Cli::try_parse_from(["coderight", "run", "hi", "--tools"]).is_err());
    assert!(Cli::try_parse_from(["coderight", "run", "hi", "--exclude-tools"]).is_err());
}

#[test]
fn p17_launch_tool_filter_flags_parse() {
    let filtered = launch(&[
        "coderight",
        "--tools",
        "read_file,grep_*",
        "--exclude-tools=shell",
        "--no-mcp",
    ]);
    assert_eq!(filtered.tools, vec!["read_file", "grep_*"]);
    assert_eq!(filtered.exclude_tools, vec!["shell"]);
    assert!(filtered.no_mcp);
    let d = launch(&["coderight"]);
    assert!(d.tools.is_empty() && d.exclude_tools.is_empty() && !d.no_mcp);
    assert!(LaunchCli::try_parse_from(["coderight", "--tools"]).is_err());
}

#[test]
fn launch_preset_fills_unset_fields_and_flags_win() {
    // Preset semantics: explicit flags override (https://code.claude.com/docs/en/cli-reference).
    let preset = rightkit_app_settings::agent_config::presets::Preset {
        model: Some("anthropic/p".into()),
        effort: Some("high".into()),
        permission_mode: Some("accept-edits".into()),
        tools: vec!["read_file".into()],
        system_prompt: Some("sys".into()),
        ..Default::default()
    };
    let mut parsed = launch(&["coderight", "--preset", "r", "--effort", "low"]);
    assert_eq!(parsed.preset.as_deref(), Some("r"));
    assert_eq!(parsed.apply_preset(&preset).as_deref(), Some("sys"));
    assert_eq!(parsed.model.as_deref(), Some("anthropic/p"));
    assert_eq!(parsed.effort.as_deref(), Some("low"));
    assert_eq!(parsed.tools, vec!["read_file"]);
    assert!(parsed.accept_edits);

    let mut parsed = launch(&["coderight", "--preset", "r", "--auto"]);
    parsed.apply_preset(&preset);
    assert!(parsed.auto && !parsed.accept_edits);
}
