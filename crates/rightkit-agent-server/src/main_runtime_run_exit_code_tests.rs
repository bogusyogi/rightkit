// Test bodies for `RunExitCode`. Included from main_runtime_run_exit_code.rs.
// No `//!` header, no top-level `use` — items are reachable because the parent
// `mod run_exit_code_unit_tests { use super::*; use rightkit_agent_engine_protocol::ProtocolEvent; ... }`
// has already imported them into scope.

fn run_failed(kind: &str) -> ProtocolEvent {
    ProtocolEvent::RunFailed {
        session_id: "s".into(),
        seq: 1,
        created_at: "2026-01-01T00:00:00Z".into(),
        error: "x".into(),
        kind: kind.into(),
        hint: String::new(),
    }
}

fn run_failed_with_error(kind: &str, error: &str) -> ProtocolEvent {
    ProtocolEvent::RunFailed {
        session_id: "s".into(),
        seq: 1,
        created_at: "2026-01-01T00:00:00Z".into(),
        error: error.into(),
        kind: kind.into(),
        hint: String::new(),
    }
}

fn tool_denied() -> ProtocolEvent {
    ProtocolEvent::ToolApprovalResolved {
        session_id: "s".into(),
        seq: 1,
        created_at: "t".into(),
        tool_call_id: "tc".into(),
        approved: false,
    }
}

fn budget_exceeded() -> ProtocolEvent {
    ProtocolEvent::BudgetExceeded {
        session_id: "s".into(),
        seq: 1,
        created_at: "t".into(),
        scope: String::new(),
        current_usd: 1.0,
        ceiling_usd: 0.5,
        hint: String::new(),
    }
}

fn run_interrupted() -> ProtocolEvent {
    ProtocolEvent::RunInterrupted {
        session_id: "s".into(),
        seq: 1,
        created_at: "t".into(),
        reason: Some("manual".into()),
    }
}

#[test]
fn exit_code_numbers_are_stable() {
    assert_eq!(RunExitCode::Success.as_i32(), 0);
    assert_eq!(RunExitCode::Generic.as_i32(), 1);
    assert_eq!(RunExitCode::Usage.as_i32(), 2);
    assert_eq!(RunExitCode::ProviderAuthFailure.as_i32(), 3);
    assert_eq!(RunExitCode::ToolDenied.as_i32(), 4);
    assert_eq!(RunExitCode::BudgetExceeded.as_i32(), 5);
    assert_eq!(RunExitCode::Sandbox.as_i32(), 6);
    assert_eq!(RunExitCode::TimeoutStall.as_i32(), 7);
    assert_eq!(RunExitCode::UserAbort.as_i32(), 8);
    assert_eq!(RunExitCode::SchemaViolation.as_i32(), 9);
}

#[test]
fn class_labels_match_spec() {
    assert_eq!(
        RunExitCode::ProviderAuthFailure.class(),
        "provider_auth_failure"
    );
    assert_eq!(RunExitCode::ToolDenied.class(), "tool_denied");
    assert_eq!(RunExitCode::BudgetExceeded.class(), "budget_exceeded");
    assert_eq!(RunExitCode::TimeoutStall.class(), "timeout_stall");
    assert_eq!(RunExitCode::UserAbort.class(), "user_abort");
    assert_eq!(RunExitCode::Sandbox.class(), "sandbox");
    assert_eq!(RunExitCode::SchemaViolation.class(), "schema_violation");
}

#[test]
fn cli_to_run_mapping_preserves_legacy_codes_for_first_six() {
    assert_eq!(
        run_exit_code_from_cli(CliExitCode::Success),
        RunExitCode::Success
    );
    assert_eq!(
        run_exit_code_from_cli(CliExitCode::Generic),
        RunExitCode::Generic
    );
    assert_eq!(
        run_exit_code_from_cli(CliExitCode::Usage),
        RunExitCode::Usage
    );
    assert_eq!(
        run_exit_code_from_cli(CliExitCode::Auth),
        RunExitCode::ProviderAuthFailure
    );
    assert_eq!(
        run_exit_code_from_cli(CliExitCode::Permission),
        RunExitCode::ToolDenied
    );
    assert_eq!(
        run_exit_code_from_cli(CliExitCode::Budget),
        RunExitCode::BudgetExceeded
    );
    assert_eq!(
        run_exit_code_from_cli(CliExitCode::Sandbox),
        RunExitCode::Sandbox
    );
    assert_eq!(
        run_exit_code_from_cli(CliExitCode::TimeoutStall),
        RunExitCode::TimeoutStall
    );
    assert_eq!(
        run_exit_code_from_cli(CliExitCode::UserAbort),
        RunExitCode::UserAbort
    );
    assert_eq!(
        run_exit_code_from_cli(CliExitCode::SchemaViolation),
        RunExitCode::SchemaViolation
    );
}

#[test]
fn run_failed_auth_yields_provider_auth_failure() {
    assert_eq!(
        run_exit_code_for_events(&[run_failed("auth")]),
        Some(RunExitCode::ProviderAuthFailure)
    );
}

#[test]
fn run_failed_permission_yields_tool_denied() {
    assert_eq!(
        run_exit_code_for_events(&[run_failed("permission")]),
        Some(RunExitCode::ToolDenied)
    );
    assert_eq!(
        run_exit_code_for_events(&[tool_denied()]),
        Some(RunExitCode::ToolDenied)
    );
}

#[test]
fn run_failed_budget_yields_budget_exceeded() {
    assert_eq!(
        run_exit_code_for_events(&[run_failed("budget")]),
        Some(RunExitCode::BudgetExceeded)
    );
    // A standalone `BudgetExceeded` event also resolves to BudgetExceeded.
    assert_eq!(
        run_exit_code_for_events(&[budget_exceeded()]),
        Some(RunExitCode::BudgetExceeded)
    );
    // First-match-wins: when BudgetExceeded appears FIRST, it dominates any
    // typed RunFailed that follows.
    let events = vec![budget_exceeded(), run_failed("network")];
    assert_eq!(
        run_exit_code_for_events(&events),
        Some(RunExitCode::BudgetExceeded)
    );
}

#[test]
fn run_failed_max_iterations_or_timeout_yields_timeout_stall() {
    assert_eq!(
        run_exit_code_for_events(&[run_failed("max_iterations")]),
        Some(RunExitCode::TimeoutStall)
    );
    assert_eq!(
        run_exit_code_for_events(&[run_failed("timeout")]),
        Some(RunExitCode::TimeoutStall)
    );
    assert_eq!(
        run_exit_code_for_events(&[run_failed("stall")]),
        Some(RunExitCode::TimeoutStall)
    );
}

#[test]
fn run_interrupted_yields_user_abort() {
    assert_eq!(
        run_exit_code_for_events(&[run_interrupted()]),
        Some(RunExitCode::UserAbort)
    );
}

#[test]
fn empty_events_yield_none() {
    assert_eq!(run_exit_code_for_events(&[]), None);
}

#[test]
fn unknown_failure_kind_with_unmatched_error_falls_back_to_generic() {
    // Unknown kind AND error text contains none of the classifier keywords.
    let events = vec![run_failed("not_a_real_kind")];
    assert_eq!(
        run_exit_code_for_events(&events),
        Some(RunExitCode::Generic)
    );
}

#[test]
fn unknown_failure_kind_falls_back_to_error_text_classifier() {
    // REGRESSION PIN: an unmapped kind ("network") plus error text mentioning
    // "api key" must still resolve to ProviderAuthFailure via the legacy
    // `cli_exit_code_from_error` fallback. Without that funnel, the previous
    // mapping regressed to Generic for rate-limited / auth-key failures.
    assert_eq!(
        run_exit_code_for_events(&[run_failed_with_error("network", "missing api key")]),
        Some(RunExitCode::ProviderAuthFailure)
    );
    assert_eq!(
        run_exit_code_for_events(&[run_failed_with_error("provider_error", "401 Unauthorized")]),
        Some(RunExitCode::ProviderAuthFailure)
    );
    assert_eq!(
        run_exit_code_for_events(&[run_failed_with_error("other", "permission denied by rule")]),
        Some(RunExitCode::ToolDenied)
    );
    assert_eq!(
        run_exit_code_for_events(&[run_failed_with_error("other", "session budget exceeded")]),
        Some(RunExitCode::BudgetExceeded)
    );
}

#[test]
fn short_circuit_on_exit_on_error_returns_first_typed_when_present() {
    // Three typed entries; the first matching event dominates.
    let events = vec![run_failed("network"), run_failed("auth"), tool_denied()];
    let code = final_run_exit_code(
        true,
        true,
        run_exit_code_for_events(&events),
        None,
        RunExitCode::Generic,
    );
    // `network` with the default `error: "x"` is unknown + not matching →
    // falls back through `cli_exit_code_from_error("x")` → Generic.
    assert_eq!(code, RunExitCode::Generic);
}

#[test]
fn planted_error_step_with_exit_on_error_returns_first_typed_code() {
    // "planted erroring step": one happy AssistantDelta then a typed
    // ToolApprovalResolved{approved: false}.
    use rightkit_agent_engine_protocol::ProtocolEvent::AssistantDelta;
    let step = AssistantDelta {
        session_id: "s".into(),
        seq: 1,
        created_at: "t".into(),
        text: "i will write the file now".into(),
        is_reasoning: false,
    };
    let events = vec![step, tool_denied()];
    let first_typed = run_exit_code_for_events(&events);
    assert_eq!(first_typed, Some(RunExitCode::ToolDenied));
    // With --exit-on-error the run handler returns the FIRST typed code...
    let with_flag = final_run_exit_code(
        true, // engine.submit returned Ok
        true, // --exit-on-error set
        first_typed,
        None,
        RunExitCode::Generic,
    );
    assert_eq!(with_flag, RunExitCode::ToolDenied);
    // ...without the flag the legacy path returns Success on Ok, even
    // though a typed error happened mid-run (existing behavior preserved).
    let without_flag = final_run_exit_code(true, false, first_typed, None, RunExitCode::Generic);
    assert_eq!(without_flag, RunExitCode::Success);
}

#[test]
fn exit_on_error_falls_back_to_terminal_event_when_no_first_typed() {
    let code = final_run_exit_code(
        true,
        true,
        None, // no first typed
        Some(RunExitCode::BudgetExceeded),
        RunExitCode::Generic,
    );
    assert_eq!(code, RunExitCode::BudgetExceeded);
}

#[test]
fn exit_on_error_with_no_observations_returns_success() {
    let code = final_run_exit_code(true, true, None, None, RunExitCode::Generic);
    assert_eq!(code, RunExitCode::Success);
}

#[test]
fn legacy_err_path_uses_terminal_event_or_fallback() {
    assert_eq!(
        final_run_exit_code(
            false,
            false,
            None,
            Some(RunExitCode::ToolDenied),
            RunExitCode::Generic
        ),
        RunExitCode::ToolDenied
    );
    assert_eq!(
        final_run_exit_code(false, false, None, None, RunExitCode::Generic),
        RunExitCode::Generic
    );
    assert_eq!(
        final_run_exit_code(true, false, None, None, RunExitCode::Generic),
        RunExitCode::Success
    );
}
