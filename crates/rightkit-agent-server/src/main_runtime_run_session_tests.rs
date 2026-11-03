// Behavior tests covering the headless controls added on `coderight run`.
// Included from main_runtime_run_session.rs inside `mod run_session_unit_tests
// { use super::*; ... }`. The wrapping mod inherits the parent module's
// ITEMS only (not its  clauses), so we re-state the trailing imports
// for types not declared in main_runtime.rs's own prelude.

use rightkit_agent_engine_protocol::ProtocolEvent;
use rightkit_agent_policy::WorkspaceTrust;

#[test]
fn stream_input_event_kinds_are_explicit_for_refusal_diagnostics() {
    assert_eq!(
        input_event_kind(&RunInputEvent::UserMessage {
            text: "later".into(),
            nonce: None,
            trust: None,
        }),
        "user_message"
    );
    assert_eq!(
        input_event_kind(&RunInputEvent::ToolResult {
            tool_call_id: "call".into(),
            summary: "result".into(),
            is_error: false,
            process_id: None,
        }),
        "tool_result"
    );
    assert_eq!(
        input_event_kind(&RunInputEvent::Control {
            action: ControlAction::Ping,
            reason: None,
        }),
        "control"
    );
}

#[test]
fn default_runs_receive_unique_session_ids_and_explicit_ids_resume() {
    let first = resolve_run_session_id(None);
    let second = resolve_run_session_id(None);
    assert_ne!(first, second);
    assert!(first.starts_with("cli-"));
    assert!(rightkit_agent_engine_protocol::is_canonical_uuid_v7(
        first.trim_start_matches("cli-")
    ));
    assert_eq!(
        resolve_run_session_id(Some("resume-me".into())),
        "resume-me"
    );
}

// ── 1. --allowed-tools intersects the toolset profile ─────────────────
#[test]
fn apply_toolset_profile_intersects_with_toolset_profile() {
    // `dev-safe` is a builtin profile whose allowlist is narrower than the
    // full registry. Passing `--allowed-tools read_file,shell` must
    // intersect: only the tool in BOTH the requested list AND the profile
    // allowlist survives.
    let profile =
        api_toolsets::resolve_toolset_profile("dev-safe").expect("dev-safe profile should resolve");
    let (allowed, disallowed) = api_toolsets::apply_toolset_profile(
        Some(&profile),
        vec!["read_file".to_string(), "shell".to_string()],
        vec![],
    );
    assert_eq!(
        allowed,
        vec!["read_file".to_string()],
        "allowed tools must intersect with profile cap"
    );
    assert!(
        disallowed.contains(&"shell".to_string()),
        "profile-level denylist must include shell regardless of requested list"
    );
}

#[test]
fn resolve_cli_tool_filters_rejects_unknown_toolset() {
    let err = api_toolsets::resolve_toolset_profile("definitely-not-a-profile").err();
    assert!(err.is_some(), "unknown toolset must yield Err");
}

// ── P-17: pi-style --tools / --exclude-tools / --no-mcp ───────────────
// Semantics: https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/docs/cli.md
fn p17_filters(
    allowed: &[&str],
    disallowed: &[&str],
    no_mcp: bool,
) -> Result<(Vec<String>, Vec<String>), String> {
    resolve_cli_tool_filters_with_mcp(
        None,
        allowed.iter().map(|s| s.to_string()).collect(),
        disallowed.iter().map(|s| s.to_string()).collect(),
        no_mcp,
    )
    .map(|(_, allowed, disallowed)| (allowed, disallowed))
}

#[test]
fn p17_include_glob_expands_against_registered_tool_names() {
    let (allowed, _) = p17_filters(&["*_file"], &[], false).unwrap();
    assert!(allowed.contains(&"read_file".to_string()), "{allowed:?}");
    assert!(allowed.contains(&"write_file".to_string()), "{allowed:?}");
    assert!(!allowed.contains(&"shell".to_string()), "{allowed:?}");
}

#[test]
fn p17_exclude_glob_expands_and_wins_over_include() {
    let (allowed, disallowed) = p17_filters(&["*_file"], &["write_*"], false).unwrap();
    assert!(
        disallowed.contains(&"write_file".to_string()),
        "{disallowed:?}"
    );
    assert!(!allowed.contains(&"write_file".to_string()), "{allowed:?}");
    assert!(allowed.contains(&"read_file".to_string()), "{allowed:?}");
}

#[test]
fn p17_question_mark_is_literal_like_pi() {
    assert!(cli_tool_glob_matches("read_*", "read_file"));
    assert!(!cli_tool_glob_matches("read_fil?", "read_file"));
}

#[test]
fn p17_unknown_only_lists_are_usage_errors_but_mixed_lists_pass() {
    let error = p17_filters(&["no_such_tool_*"], &[], false).unwrap_err();
    assert!(
        error.contains("--tools") && error.contains("no_such_tool_*"),
        "{error}"
    );
    let error = p17_filters(&[], &["nothing_here", "nor_*"], false).unwrap_err();
    assert!(error.contains("--exclude-tools"), "{error}");
    let (allowed, _) = p17_filters(&["read_file", "no_such_tool_*"], &[], false).unwrap();
    assert!(allowed.contains(&"read_file".to_string()));
    assert_eq!(RunExitCode::Usage.as_i32(), 2);
}

#[test]
fn p17_tools_allowlist_keeps_mcp_gateway_unless_mcp_named() {
    let (allowed, _) = p17_filters(&["read_file"], &[], false).unwrap();
    assert!(allowed.contains(&"mcp_call".to_string()), "{allowed:?}");
    let (allowed, _) = p17_filters(&["read_file", "mcp__radius__*"], &[], false).unwrap();
    // Qualified MCP patterns pass through verbatim for the MCP manager.
    assert!(
        allowed.contains(&"mcp__radius__*".to_string()),
        "{allowed:?}"
    );
}

#[test]
fn p17_no_mcp_and_exclude_mcp_star_remove_mcp_gateway_tools() {
    let (_, disallowed) = p17_filters(&[], &[], true).unwrap();
    for tool in [
        "mcp_call",
        "list_mcp_tools",
        "list_mcp_resources",
        "read_mcp_resource",
    ] {
        assert!(
            disallowed.contains(&tool.to_string()),
            "{tool}: {disallowed:?}"
        );
    }
    let (_, disallowed) = p17_filters(&[], &["mcp__*"], false).unwrap();
    assert!(
        disallowed.contains(&"mcp_call".to_string()),
        "{disallowed:?}"
    );
    assert!(disallowed.contains(&"mcp__*".to_string()), "{disallowed:?}");
    let (_, disallowed) = p17_filters(&[], &["mcp__radius__*"], false).unwrap();
    assert!(
        !disallowed.contains(&"mcp_call".to_string()),
        "{disallowed:?}"
    );
}

// ── 2. --max-budget-usd mapping ───────────────────────────────────────
#[test]
fn budget_exceeded_protocol_event_maps_to_budget_exceeded_exit_code() {
    let event = ProtocolEvent::BudgetExceeded {
        session_id: "s-budget".into(),
        seq: 7,
        created_at: "2026-06-30T00:00:00Z".into(),
        scope: "session".into(),
        current_usd: 1.25,
        ceiling_usd: 1.00,
        hint: String::new(),
    };
    let code = run_exit_code_for_event(&event);
    assert_eq!(
        code,
        Some(RunExitCode::BudgetExceeded),
        "engine-side BudgetExceeded must surface as RunExitCode::BudgetExceeded (=5)"
    );
    assert_eq!(RunExitCode::BudgetExceeded.as_i32(), 5);
}

#[test]
fn run_failed_budget_kind_maps_to_budget_exceeded() {
    // ProtocolEvent::RunFailed only carries session_id/seq/created_at/kind/error/hint.
    let event = ProtocolEvent::RunFailed {
        session_id: "s-rf".into(),
        seq: 1,
        created_at: "2026-06-30T00:00:00Z".into(),
        kind: "budget".into(),
        error: "session budget was exceeded".into(),
        hint: String::new(),
    };
    let code = run_exit_code_for_event(&event);
    assert_eq!(code, Some(RunExitCode::BudgetExceeded));
}

// ── 3. --safe-mode strict tier (workspace_trust=Untrusted + no rules) ──
#[test]
fn safe_mode_resolves_to_untrusted_trust_tier() {
    // Calls the production helper used by run_session's SessionEngineConfig.
    assert_eq!(safe_mode_workspace_trust(true), WorkspaceTrust::Untrusted,);
}

#[test]
fn safe_mode_default_is_trusted_when_disabled() {
    // Mirror run_session's default: safe_mode not set => Trusted trust tier.
    assert_eq!(safe_mode_workspace_trust(false), WorkspaceTrust::Trusted,);
}

#[test]
fn safe_mode_toolset_intersection_drops_mutating_tools() {
    // safe-mode implicitly applies the `read-only` profile (caps the union).
    // Even user-supplied `--allowed-tools shell,read_file` is filtered to
    // just `read_file` because `shell` is explicitly disallowed by the
    // read-only profile.
    let profile =
        api_toolsets::resolve_toolset_profile("read-only").expect("read-only profile must resolve");
    assert!(
        profile.disallowed_tools.iter().any(|t| t == "shell"),
        "read-only profile must explicitly deny shell: {:?}",
        profile.disallowed_tools,
    );
    let (allowed, disallowed) = api_toolsets::apply_toolset_profile(
        Some(&profile),
        vec!["shell".to_string(), "read_file".to_string()],
        vec![],
    );
    assert_eq!(
        allowed,
        vec!["read_file".to_string()],
        "safe-mode must restrict intersected allowlist to read-only profile"
    );
    assert!(
        disallowed.contains(&"shell".to_string()),
        "intersected denylist must include shell after safe-mode intersect"
    );
}

pub(super) fn output_mode_args() -> RunArgs {
    RunArgs {
        context_mode: ContextMode::Membrane,
        prompt: "hello".into(),
        print: false,
        output_format: OutputFormat::Text,
        json_event: false,
        model: None,
        provider: Some("openrouter".into()),
        effort: None,
        workspace: None,
        auto_approve: false,
        bypass_approvals: false,
        permission_mode: None,
        stream_json: false,
        exit_on_error: false,
        max_turns: None,
        allowed_tools: Vec::new(),
        disallowed_tools: Vec::new(),
        toolset: None,
        max_budget_usd: None,
        bare: false,
        safe_mode: false,
        system_prompt: None,
        system_prompt_file: None,
        append_system_prompt: None,
        append_system_prompt_file: None,
        mcp_config: None,
        session_id: None,
        add_dir: Vec::new(),
        input_format: InputFormat::Text,
        image: Vec::new(),
        ephemeral: false,
        output_last_message: None,
        color: crate::cli::CliColor::Auto,
        tools: Vec::new(),
        exclude_tools: Vec::new(),
        no_mcp: false,
        output_schema: None,
    }
}

#[test]
fn print_mode_is_distinct_from_default_text_mode() {
    let mut args = output_mode_args();
    assert_eq!(select_run_output_mode(&args, None), RunOutputMode::Text);
    args.print = true;
    assert_eq!(select_run_output_mode(&args, None), RunOutputMode::Print);
    args.print = false;
    args.output_format = OutputFormat::Json;
    assert_eq!(select_run_output_mode(&args, None), RunOutputMode::Json);
    args.output_format = OutputFormat::Text;
    args.stream_json = true;
    assert_eq!(
        select_run_output_mode(&args, None),
        RunOutputMode::StreamJson
    );
}

#[test]
fn stream_json_input_defaults_to_stream_json_output() {
    let mut args = output_mode_args();
    args.input_format = InputFormat::StreamJson;
    assert_eq!(
        select_run_output_mode(&args, None),
        RunOutputMode::StreamJson,
        "JSONL input must keep stdout machine-readable without a second output flag"
    );
}

#[test]
fn schema_mode_is_machine_output_even_before_validator_loads() {
    let mut args = output_mode_args();
    args.output_schema = Some("missing.schema.json".into());
    assert_eq!(
        select_run_output_mode(&args, None),
        RunOutputMode::SchemaJson
    );
}

// FM-107: Print is not a bare alias for the default (Text) one-shot output —
// the two modes render an identical event stream differently.
// `process_daemon_event_with_mode` streams every event as it arrives under
// Text, but under Print it renders nothing for intermediate events and
// defers all output to the terminal event (see `emit_final_print_output`).
// This proves the behavioral distinction the CLI help text (in
// `cli_definitions_command_run.rs`) now documents, not just that the two
// enum variants compare unequal.
#[test]
fn print_mode_suppresses_intermediate_output_that_text_mode_would_render() {
    let session_id = "s-fm107";
    let delta_event = serde_json::json!({
        "$kind": "assistantDelta",
        "sessionId": session_id,
        "seq": 1,
        "createdAt": "t1",
        "text": "partial answer",
        "isReasoning": false,
    });
    let terminal_event = serde_json::json!({
        "$kind": "assistantMessageCompleted",
        "sessionId": session_id,
        "seq": 2,
        "createdAt": "t2",
        "message": {
            "$kind": "assistant",
            "id": "asst-1",
            "createdAt": "t2",
            "content": "partial answer",
            "toolCalls": [],
            "pinned": false,
        },
    });

    // Under Print: the intermediate delta must not flip final_output_emitted
    // (nothing is rendered for it), and only the terminal event triggers
    // `emit_final_print_output`.
    let mut print_state = RunOutputState::default();
    let mut print_first_error = None;
    process_daemon_event_with_mode(
        &delta_event,
        session_id,
        RunOutputMode::Print,
        None,
        crate::cli::CliColor::Auto,
        &mut print_state,
        &mut print_first_error,
    );
    assert!(
        !print_state.final_output_emitted,
        "Print must not emit output for a non-terminal event"
    );
    process_daemon_event_with_mode(
        &terminal_event,
        session_id,
        RunOutputMode::Print,
        None,
        crate::cli::CliColor::Auto,
        &mut print_state,
        &mut print_first_error,
    );
    assert!(
        print_state.final_output_emitted,
        "Print must emit the deferred final output once a terminal event arrives"
    );

    // Under Text: neither event ever sets final_output_emitted — Text has no
    // "final output" concept, it renders every event as it streams, which is
    // exactly the behavior Print exists to suppress.
    let mut text_state = RunOutputState::default();
    let mut text_first_error = None;
    process_daemon_event_with_mode(
        &delta_event,
        session_id,
        RunOutputMode::Text,
        None,
        crate::cli::CliColor::Auto,
        &mut text_state,
        &mut text_first_error,
    );
    process_daemon_event_with_mode(
        &terminal_event,
        session_id,
        RunOutputMode::Text,
        None,
        crate::cli::CliColor::Auto,
        &mut text_state,
        &mut text_first_error,
    );
    assert!(
        !text_state.final_output_emitted,
        "Text mode has no deferred final-output gate — confirming it is not the same code path as Print"
    );
}

#[test]
fn run_options_validate_budget_add_dir_and_mcp_config_as_usage_errors() {
    assert!(validate_run_options(&output_mode_args()).is_ok());
    let mut args = output_mode_args();
    args.max_turns = Some(0);
    assert!(validate_run_options(&args).is_err());
    let mut args = output_mode_args();
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        args.max_budget_usd = Some(bad);
        assert!(validate_run_options(&args)
            .unwrap_err()
            .contains("--max-budget-usd"));
    }
    args.max_budget_usd = Some(0.5);
    assert!(validate_run_options(&args).is_ok());
    args.add_dir = vec!["definitely/not/a/dir".into()];
    assert!(validate_run_options(&args)
        .unwrap_err()
        .contains("--add-dir"));
    args.add_dir.clear();
    args.mcp_config = Some("definitely/not/a/file.json".into());
    assert!(validate_run_options(&args)
        .unwrap_err()
        .contains("--mcp-config"));
    args.mcp_config = None;
    args.add_dir = vec![".".into()];
    assert!(validate_run_options(&args).is_ok());
}

#[test]
fn unknown_toolset_is_a_usage_error_not_generic() {
    let error = resolve_cli_tool_filters(Some("definitely-not-a-profile"), Vec::new(), Vec::new())
        .unwrap_err();
    assert!(error.contains("Unknown toolset"));
    assert_eq!(RunExitCode::Usage.as_i32(), 2);
}
