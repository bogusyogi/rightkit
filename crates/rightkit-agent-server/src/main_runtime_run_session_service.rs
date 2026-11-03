#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RunOutputMode {
    Text,
    Print,
    Json,
    StreamJson,
    SchemaJson,
    /// Interactive surfaces (the Ratatui TUI) own the terminal viewport and
    /// render every event through their own projection from the durable
    /// event stream. Raw stdout/stderr writes would land wherever the
    /// cursor happens to be and corrupt the frame, so this mode suppresses
    /// all direct output while preserving exit-code semantics.
    Silent,
}

impl RunOutputMode {
    fn is_machine(self) -> bool {
        matches!(self, Self::Json | Self::StreamJson | Self::SchemaJson)
    }
}

#[derive(Debug, Default)]
struct RunOutputState {
    assistant_deltas: String,
    assistant_message: Option<String>,
    output_error: Option<RunExitCode>,
    deferred_error: Option<String>,
    final_output_emitted: bool,
    /// Set when Membrane context was unavailable and the run is degraded.
    context_degraded: Option<String>,
}

pub(super) fn validate_run_options(args: &RunArgs) -> Result<(), String> {
    if args.max_turns == Some(0) {
        return Err("--max-turns must be greater than zero".into());
    }
    if let Some(budget) = args.max_budget_usd {
        if !budget.is_finite() || budget <= 0.0 {
            return Err("--max-budget-usd must be a positive, finite amount".into());
        }
    }
    for dir in &args.add_dir {
        if !std::path::Path::new(dir).is_dir() {
            return Err(format!("--add-dir `{dir}` is not an existing directory"));
        }
    }
    if let Some(config) = args.mcp_config.as_deref() {
        if !std::path::Path::new(config).is_file() {
            return Err(format!("--mcp-config `{config}` is not an existing file"));
        }
    }
    Ok(())
}

pub(super) fn select_run_output_mode(
    args: &RunArgs,
    schema: Option<&OutputSchema>,
) -> RunOutputMode {
    if matches!(args.output_format, OutputFormat::Silent) {
        RunOutputMode::Silent
    } else if args.stream_json
        || args.json_event
        || matches!(args.output_format, OutputFormat::StreamJson)
        || matches!(args.input_format, InputFormat::StreamJson)
    {
        RunOutputMode::StreamJson
    } else if schema.is_some() || args.output_schema.is_some() {
        RunOutputMode::SchemaJson
    } else if matches!(args.output_format, OutputFormat::Json) {
        RunOutputMode::Json
    } else if args.print {
        RunOutputMode::Print
    } else {
        RunOutputMode::Text
    }
}

fn apply_prepared_prompt_append(prompt: String, append: Option<String>) -> String {
    let Some(append) = append.filter(|value| !value.trim().is_empty()) else {
        return prompt;
    };
    if prompt.trim().is_empty() {
        append
    } else {
        format!("{prompt}\n\n{append}")
    }
}

fn run_error_envelope(code: RunExitCode, message: &str) -> Value {
    serde_json::json!({
        "schema_version": "coderight.cli.error.v1",
        "event": "error",
        "error": {
            "class": code.class(),
            "exit_code": code.as_i32(),
            "message": message,
        },
    })
}

pub(super) fn run_error_exit(mode: RunOutputMode, code: RunExitCode, message: &str) -> i32 {
    if mode.is_machine() {
        emit_cli_json_line(&run_error_envelope(code, message));
    } else if !matches!(mode, RunOutputMode::Silent) {
        eprintln!("{message}");
    }
    code.as_i32()
}

fn daemon_event_error_message(event: &rightkit_agent_engine_protocol::ProtocolEvent) -> &str {
    match event {
        rightkit_agent_engine_protocol::ProtocolEvent::RunFailed { error, .. } => error.as_str(),
        rightkit_agent_engine_protocol::ProtocolEvent::RunInterrupted { reason, .. } => {
            reason.as_deref().unwrap_or("run interrupted")
        }
        rightkit_agent_engine_protocol::ProtocolEvent::BudgetExceeded { hint, .. } => {
            if hint.is_empty() {
                "run budget exceeded"
            } else {
                hint.as_str()
            }
        }
        rightkit_agent_engine_protocol::ProtocolEvent::ToolApprovalResolved { .. } => {
            "tool approval denied"
        }
        _ => "run failed",
    }
}

fn emit_final_print_output(state: &mut RunOutputState) {
    if state.final_output_emitted {
        return;
    }
    let text = state
        .assistant_message
        .as_deref()
        .filter(|text| !text.is_empty())
        .unwrap_or(state.assistant_deltas.as_str());
    if !text.is_empty() {
        print!("{text}");
        if !text.ends_with('\n') {
            println!();
        }
    }
    state.final_output_emitted = true;
}

fn write_last_message(path: &str, state: &RunOutputState) -> Result<(), String> {
    let text = state
        .assistant_message
        .as_deref()
        .filter(|text| !text.is_empty())
        .unwrap_or(state.assistant_deltas.as_str());
    std::fs::write(path, text)
        .map_err(|error| format!("could not write final assistant message to {path}: {error}"))
}

fn emit_schema_output(state: &mut RunOutputState, schema: Option<&OutputSchema>) {
    if state.final_output_emitted {
        return;
    }
    state.final_output_emitted = true;
    if let Some(message) = state.deferred_error.take() {
        emit_cli_json_line(&run_error_envelope(RunExitCode::SchemaViolation, &message));
        return;
    }
    let Some(schema) = schema else {
        state.output_error = Some(RunExitCode::SchemaViolation);
        let message = "--output-schema validator was not loaded";
        emit_cli_json_line(&run_error_envelope(RunExitCode::SchemaViolation, message));
        return;
    };
    let response = state
        .assistant_message
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .unwrap_or(state.assistant_deltas.as_str());
    match normalize_output_response(schema, response) {
        Ok(output) => emit_cli_json_line(
            &serde_json::from_str(&output).unwrap_or_else(|_| serde_json::json!({})),
        ),
        Err(message) => {
            state.output_error = Some(RunExitCode::SchemaViolation);
            emit_cli_json_line(&run_error_envelope(RunExitCode::SchemaViolation, &message));
        }
    }
}

/// Parse, render, & record one raw daemon event. An invalid payload records a
/// schema violation while leaving transport reconnect handling to caller.
fn process_daemon_event_with_mode(
    event: &Value,
    session_id: &str,
    mode: RunOutputMode,
    schema: Option<&OutputSchema>,
    color: crate::cli::CliColor,
    state: &mut RunOutputState,
    first_error: &mut Option<RunExitCode>,
) {
    use rightkit_agent_engine_protocol::ProtocolEvent;

    let parsed = serde_json::from_value::<ProtocolEvent>(event.clone());
    match parsed {
        Ok(protocol_event) => {
            match &protocol_event {
                ProtocolEvent::AssistantDelta {
                    text,
                    is_reasoning: false,
                    ..
                } => state.assistant_deltas.push_str(text),
                _ => {
                    if let Some(text) = assistant_message_text(&protocol_event) {
                        state.assistant_message = Some(text.to_string());
                    }
                }
            }
            let terminal = daemon_event_is_terminal(event);
            let is_resync = daemon_event_kind(event) == Some("streamResyncRequired");
            match mode {
                RunOutputMode::StreamJson => {
                    emit_cli_json_line(&render_cli_stream_event(&protocol_event));
                }
                RunOutputMode::Json if terminal => {
                    let mut envelope = serde_json::json!({
                        "schema_version": "coderight.cli.rpc.v1",
                        "session_id": session_id,
                        "terminal_event": protocol_event,
                    });
                    if let Some(reason) = state.context_degraded.as_deref() {
                        envelope["context_degraded"] = serde_json::json!({ "reason": reason });
                    }
                    emit_cli_json_line(&envelope)
                }
                RunOutputMode::Print if terminal && !is_resync => {
                    if run_exit_code_for_event(&protocol_event).is_some() {
                        print_event(&protocol_event, color);
                    } else {
                        emit_final_print_output(state);
                    }
                }
                RunOutputMode::SchemaJson if terminal && !is_resync => {
                    if let Some(code) = run_exit_code_for_event(&protocol_event) {
                        state.output_error = Some(code);
                        state.final_output_emitted = true;
                        emit_cli_json_line(&run_error_envelope(
                            code,
                            daemon_event_error_message(&protocol_event),
                        ));
                    } else {
                        emit_schema_output(state, schema);
                    }
                }
                RunOutputMode::Text => print_event(&protocol_event, color),
                _ => {}
            }
            if first_error.is_none() {
                *first_error = run_exit_code_for_event(&protocol_event);
            }
        }
        Err(error) => {
            let message = format!("[daemon] ignored invalid event payload: {error}");
            first_error.get_or_insert(RunExitCode::SchemaViolation);
            match mode {
                RunOutputMode::StreamJson | RunOutputMode::Json => {
                    emit_cli_json_line(&run_error_envelope(RunExitCode::SchemaViolation, &message));
                }
                RunOutputMode::SchemaJson => {
                    state.output_error = Some(RunExitCode::SchemaViolation);
                    state.deferred_error.get_or_insert(message);
                }
                RunOutputMode::Text | RunOutputMode::Print => eprintln!("{message}"),
                RunOutputMode::Silent => {}
            }
        }
    }
}

/// Compatibility wrapper for existing stream-consumer tests & callers that
/// predate explicit output modes.
fn process_daemon_event(
    event: &Value,
    session_id: &str,
    stream_json: bool,
    structured_output: bool,
    first_error: &mut Option<RunExitCode>,
) {
    let mode = if stream_json {
        RunOutputMode::StreamJson
    } else if structured_output {
        RunOutputMode::Json
    } else {
        RunOutputMode::Text
    };
    let mut state = RunOutputState::default();
    process_daemon_event_with_mode(
        event,
        session_id,
        mode,
        None,
        crate::cli::CliColor::Auto,
        &mut state,
        first_error,
    );
}

fn daemon_event_run_exit_code(event: &Value) -> Option<RunExitCode> {
    serde_json::from_value::<rightkit_agent_engine_protocol::ProtocolEvent>(event.clone())
        .map(|event| run_exit_code_for_event(&event))
        .unwrap_or(Some(RunExitCode::SchemaViolation))
}

/// An explicit session id is an opt-in resume. Every default invocation mints
/// a distinct sortable identity so unrelated automation cannot accidentally
/// append to the shared historical `cli` session.
fn resolve_run_session_id(explicit: Option<String>) -> String {
    explicit.unwrap_or_else(|| format!("cli-{}", rightkit_agent_engine_protocol::uuid_v7()))
}
