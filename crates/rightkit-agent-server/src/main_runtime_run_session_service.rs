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

/// What a run says when its turn spent the round budget before the work was done.
const TURN_LIMIT_MESSAGE: &str =
    "the run reached its turn limit (--max-turns) before the work was finished; the answer is a summary of what was done so far";

#[derive(Debug, Default)]
pub(super) struct RunOutputState {
    assistant_deltas: String,
    /// Text mode: the answer text printed so far for this run's current assistant message, from
    /// live deltas. Compared with the completed message, because deltas are a live projection and
    /// can be missed (a subscriber that attaches after the first ones, or one that falls behind).
    text_printed: String,
    assistant_message: Option<String>,
    /// The run's own turn spent its round budget (`--max-turns`) and ended on the cut-off summary.
    turn_limit_reached: bool,
    output_error: Option<RunExitCode>,
    deferred_error: Option<String>,
    final_output_emitted: bool,
    /// `--schema-retries`: follow-ups still allowed after an answer that breaks the schema.
    schema_retries_left: u32,
    /// The follow-up message to send as a new turn instead of failing the run.
    schema_retry_prompt: Option<String>,
    /// Set when Membrane context was unavailable and the run is degraded.
    context_degraded: Option<String>,
    /// The text of the failure that ended the run, for the stderr error object of `--json`.
    failure_message: Option<String>,
    /// The last `final` assistant answer. `--json` reports it as `terminal_event` when the
    /// turn ends on `sessionTurnFinished` (issue 450), so the envelope keeps the reply.
    final_assistant_event: Option<serde_json::Value>,
    /// The last assistant answer of any phase: the fallback when no `final` answer came.
    last_assistant_event: Option<serde_json::Value>,
    /// The text of those two answers, for the flat `result` field of the JSON result.
    final_result_text: Option<String>,
    last_result_text: Option<String>,
    /// Totals over this run's own session, for the JSON result: provider responses, token counts
    /// and the session's cumulative cost (`None` while unpriced or not yet reported).
    turns: u32,
    input_tokens: i64,
    cached_input_tokens: i64,
    output_tokens: i64,
    reasoning_tokens: i64,
    cost_usd: Option<f64>,
    /// Every priced request carried the provider's own charge (`cost_source` "provider").
    cost_billed: bool,
}

/// Set by `run --verbose`: each model response, retry and the final status print one line on stderr.
pub(super) static RUN_VERBOSE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn verbose_event_line(event: &rightkit_agent_engine_protocol::ProtocolEvent) -> Option<String> {
    use rightkit_agent_engine_protocol::ProtocolEvent;
    match event {
        ProtocolEvent::TurnUsage {
            input_tokens,
            cached_input_tokens,
            output_tokens,
            total_ms,
            ..
        } => Some(format!(
            "[verbose] model response: input={input_tokens} cached={cached_input_tokens} output={output_tokens} total_ms={}",
            total_ms.map_or("-".to_string(), |ms| ms.to_string())
        )),
        ProtocolEvent::ProviderRetrying {
            provider,
            reason,
            attempt,
            max_attempts,
            delay_ms,
            ..
        } => Some(format!(
            "[verbose] provider retry: provider={provider} attempt={attempt}/{max_attempts} delay_ms={delay_ms} reason={reason}"
        )),
        ProtocolEvent::SessionTurnFinished { status, .. } => {
            Some(format!("[verbose] run finished: status={status}"))
        }
        ProtocolEvent::RunFailed { error, .. } => {
            Some(format!("[verbose] run finished: status=failed error={error}"))
        }
        _ => None,
    }
}

/// When this process first saw a run event: the start of `duration_ms` in the JSON result.
static RUN_FIRST_EVENT: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// What the unattended policy (headless-cli R8) did with one interaction
/// event of a headless run.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum UnattendedInteraction {
    /// Not an interaction event: the run goes on.
    Continue,
    /// An approval was denied at once as `unattended`; the run goes on and
    /// the caller remembers the denial for the exit code.
    ApprovalDenied,
    /// A question was declined at once as `unattended`; the run goes on.
    QuestionDeclined,
    /// A plan card ends the run: exit 1 `interaction_required`.
    PlanEndsRun,
}

/// The request id an approval event names: the nested `call.id`, else the
/// flat `toolCallId` / `requestId`.
pub(super) fn approval_call_id(event: &serde_json::Value) -> Option<String> {
    event
        .pointer("/call/id")
        .or_else(|| event.get("toolCallId"))
        .or_else(|| event.get("requestId"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// The body that denies an approval at once (headless R8): never a wait.
pub(super) fn unattended_denial_body(tool_call_id: &str) -> serde_json::Value {
    serde_json::json!({
        "toolCallId": tool_call_id,
        "approved": false,
        "reason": "unattended",
    })
}

/// The body that answers an approval the run raised under `--approval-callback`: the daemon's
/// `ResolveApproval` (camelCase). `remember` and `reason` are omitted when absent.
pub(super) fn approval_response_body(
    tool_call_id: &str,
    approved: bool,
    remember: Option<&str>,
    reason: Option<&str>,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "toolCallId": tool_call_id,
        "approved": approved,
    });
    if let Some(remember) = remember {
        body["remember"] = serde_json::Value::String(remember.to_string());
    }
    if let Some(reason) = reason {
        body["reason"] = serde_json::Value::String(reason.to_string());
    }
    body
}

/// The body that denies an open callback approval when its client is gone (input ended).
pub(super) fn detached_denial_body(tool_call_id: &str) -> serde_json::Value {
    serde_json::json!({
        "toolCallId": tool_call_id,
        "approved": false,
        "reason": "detached",
    })
}

/// The answers payload that declines an `ask_user` question at once
/// (headless R8; wire form `declined: true`, reason `unattended`).
pub(super) fn unattended_decline_answers() -> serde_json::Value {
    serde_json::json!({ "declined": true, "reason": "unattended" })
}

/// Apply the unattended policy to one event of a headless run: an approval is
/// denied at once as `unattended`, an `ask_user` is declined at once, and a
/// plan card ends the run. Nothing waits on a timer. A resolve error means the
/// engine already answered the same request, so it is not a run failure.
pub(super) async fn apply_unattended_policy(
    host: &super::SessionExecutionHost,
    session_id: &str,
    event: &serde_json::Value,
    approvals_unattended: bool,
) -> UnattendedInteraction {
    match daemon_event_kind(event) {
        Some("toolApprovalRequested") => {
            // `--approval-callback`: the approval stays open for the client's answer.
            if !approvals_unattended {
                return UnattendedInteraction::Continue;
            }
            if let Some(call_id) = approval_call_id(event) {
                let _ = host
                    .resolve_approval_body(session_id, unattended_denial_body(&call_id))
                    .await;
            }
            UnattendedInteraction::ApprovalDenied
        }
        Some("userQuestionAsked") => {
            if let Some(question_id) = event.get("questionId").and_then(serde_json::Value::as_str) {
                let _ = host
                    .answer_question(session_id, question_id, unattended_decline_answers())
                    .await;
            }
            UnattendedInteraction::QuestionDeclined
        }
        Some("planPresented") => UnattendedInteraction::PlanEndsRun,
        _ => UnattendedInteraction::Continue,
    }
}

/// The exit code of a finished unattended run: a run that ended unmet after
/// an unattended denial is `tool_denied`. A completed plan-mode run exits 0
/// with the plan as its result; only an unanswerable plan approval card ends
/// a run with `interaction_required` (headless R8, section 2.5).
pub(super) fn unattended_exit_code(code: RunExitCode, denied_unattended: bool) -> RunExitCode {
    if denied_unattended && code == RunExitCode::Generic {
        return RunExitCode::ToolDenied;
    }
    code
}

/// Output flags that name two different machine formats are a usage error
/// (exit 2), never a silent override: a stream (`--stream-json`, `--json-event`,
/// `--output-format stream-json`, or `--input-format stream-json` driving a
/// stream) cannot also be one `--output-format json` envelope or a
/// `--output-schema` result.
pub(super) fn conflicting_output_flags(args: &RunArgs) -> Option<String> {
    if args.approval_callback && !matches!(args.input_format, InputFormat::StreamJson) {
        return Some("--approval-callback needs --input-format stream-json".to_string());
    }
    let streams = args.stream_json
        || args.json_event
        || matches!(args.output_format, OutputFormat::StreamJson)
        || matches!(args.input_format, InputFormat::StreamJson);
    let streaming_flag = if args.stream_json {
        "--stream-json"
    } else if args.json_event {
        "--json-event"
    } else if matches!(args.output_format, OutputFormat::StreamJson) {
        "--output-format stream-json"
    } else {
        "--input-format stream-json"
    };
    if streams && matches!(args.output_format, OutputFormat::Json) {
        return Some(format!(
            "{streaming_flag} streams events and cannot be combined with --output-format json, which prints one envelope; pick one"
        ));
    }
    if streams && args.output_schema.is_some() {
        return Some(format!(
            "{streaming_flag} streams events and cannot be combined with --output-schema, which prints one validated result; pick one"
        ));
    }
    None
}

pub(super) fn validate_run_options(args: &RunArgs) -> Result<(), String> {
    if let Some(message) = conflicting_output_flags(args) {
        return Err(message);
    }
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

/// R15: an interrupt settles into exactly one outcome: exit 1, `interrupted`.
/// The settle wait that can instead end as exit 3 `unknown` lives in the host.
pub(super) fn interrupted_exit(mode: RunOutputMode) -> i32 {
    run_error_exit(mode, RunExitCode::UserAbort, "run interrupted")
}

/// R15: the instant by which a sent interrupt must have settled, or `None`
/// while no interrupt is pending. The clock starts when the interrupt was first
/// seen as sent and restarts for a later turn (`interrupt_sent` reset).
pub(super) fn interrupt_settle_deadline(
    interrupt_sent: bool,
    interrupt_at: &mut Option<tokio::time::Instant>,
    now: tokio::time::Instant,
) -> Option<tokio::time::Instant> {
    if !interrupt_sent {
        *interrupt_at = None;
        return None;
    }
    let at = *interrupt_at.get_or_insert(now);
    Some(at + RUN_INTERRUPT_SETTLE_WAIT)
}

/// The exit code of a run that ended on a terminal event. R10: in `--json` mode a failure also prints the
/// stderr object `{"error":{code,message,retryable}}` (the envelope on stdout carries the event, not the
/// stable code), so a caller reads the class of the failure from one place in every mode.
pub(super) fn exit_code_with_failure_object(
    mode: RunOutputMode,
    state: &RunOutputState,
    code: RunExitCode,
) -> i32 {
    if matches!(mode, RunOutputMode::Json) && code != RunExitCode::Success {
        let message = state
            .failure_message
            .as_deref()
            .unwrap_or("the run ended without reaching its goal");
        emit_cli_error_json(&run_error_envelope(code, message));
    }
    code.as_i32()
}

pub(super) fn run_error_exit(mode: RunOutputMode, code: RunExitCode, message: &str) -> i32 {
    if matches!(mode, RunOutputMode::Silent) {
        return code.as_i32();
    }
    run_error_exit_on_stderr(mode.is_machine(), code, message)
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
        emit_cli_error_json(&run_error_envelope(RunExitCode::SchemaViolation, &message));
        return;
    }
    let Some(schema) = schema else {
        state.output_error = Some(RunExitCode::SchemaViolation);
        let message = "--output-schema validator was not loaded";
        emit_cli_error_json(&run_error_envelope(RunExitCode::SchemaViolation, message));
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
        Err(message) if state.schema_retries_left > 0 => {
            // Ask again, as a new turn of the same session; nothing is printed for this answer.
            state.schema_retries_left -= 1;
            state.final_output_emitted = false;
            state.schema_retry_prompt = Some(schema_retry_prompt(&message));
        }
        Err(message) => {
            state.output_error = Some(RunExitCode::SchemaViolation);
            emit_cli_error_json(&run_error_envelope(RunExitCode::SchemaViolation, &message));
        }
    }
}

/// Set by `run --fork`: the session being run is a fresh fork, which adopts the caller's folder.
pub(super) static RUN_FORKED_SESSION: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The usage error for resuming a stored session whose workspace folder no longer exists, or
/// `None` when the session's folder is there (or is the folder the run is in).
fn missing_session_workspace_message(
    row: &serde_json::Value,
    workspace_root: &str,
) -> Option<String> {
    let stored = row.get("workspaceRoot")?.as_str()?.trim();
    if stored.is_empty() || stored == workspace_root || std::path::Path::new(stored).is_dir() {
        return None;
    }
    Some(format!(
        "session workspace {stored} no longer exists; pass --fork to fork it into {workspace_root}"
    ))
}

/// How many schema corrections this process may ask for (`run --schema-retries`).
pub(super) static SCHEMA_RETRIES: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0);

/// The follow-up that asks the model to fix an answer that broke the schema.
fn schema_retry_prompt(error: &str) -> String {
    format!(
        "Your answer did not match the schema: {error}. Reply with only JSON that matches the schema."
    )
}

/// Set once the unpriced-model warning has been printed (PROV-13).
static UNPRICED_MODEL_WARNED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Parse, render, & record one raw daemon event. An invalid payload records a
/// schema violation while leaving transport reconnect handling to caller.
/// Text mode prints the answer from live deltas, which can be missed. When the completed message
/// holds text the deltas did not deliver, print it: the missing end when what was printed is the
/// start of the answer, otherwise the whole answer after a note on stderr (text already written to
/// stdout cannot be taken back). Text that only differs in other ways is left as printed.
fn print_unstreamed_answer_text(printed: &str, complete: &str) {
    use std::io::Write as _;
    let (printed_text, complete_text) = (printed.trim(), complete.trim());
    if complete_text.len() <= printed_text.len() {
        return;
    }
    if let Some(missing) = complete_text.strip_prefix(printed_text) {
        print!("{missing}");
    } else {
        eprintln!("\n[part of the answer was not received while it streamed; the complete answer follows]");
        if !printed.is_empty() {
            println!();
        }
        print!("{complete_text}");
    }
    let _ = std::io::stdout().flush();
}

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

    RUN_FIRST_EVENT.get_or_init(std::time::Instant::now);
    let parsed = serde_json::from_value::<ProtocolEvent>(event.clone());
    match parsed {
        Ok(protocol_event) => {
            if RUN_VERBOSE.load(std::sync::atomic::Ordering::Relaxed)
                && !matches!(mode, RunOutputMode::Silent)
            {
                if let Some(line) = verbose_event_line(&protocol_event) {
                    eprintln!("{line}");
                }
            }
            match &protocol_event {
                // A request that went out with context off (over budget, no room): the run says so.
                ProtocolEvent::OrchestrationObservationRecorded {
                    session_id: event_session,
                    observation,
                    ..
                } if event_session == session_id && observation.kind == "context_degraded" => {
                    if state.context_degraded.is_none() {
                        state.context_degraded = Some(
                            observation
                                .fields
                                .get("reason")
                                .cloned()
                                .unwrap_or_else(|| observation.summary.clone()),
                        );
                    }
                }
                ProtocolEvent::OrchestrationObservationRecorded {
                    session_id: event_session,
                    observation,
                    ..
                } if event_session == session_id
                    && observation.kind
                        == rightkit_agent_engine_core::TURN_LIMIT_OBSERVATION_KIND =>
                {
                    state.turn_limit_reached = true;
                }
                ProtocolEvent::TurnUsage {
                    session_id: event_session,
                    input_tokens,
                    cached_input_tokens,
                    output_tokens,
                    reasoning_tokens,
                    ..
                } if event_session == session_id => {
                    state.turns += 1;
                    state.input_tokens += input_tokens;
                    state.cached_input_tokens += cached_input_tokens;
                    state.output_tokens += output_tokens;
                    state.reasoning_tokens += reasoning_tokens;
                }
                ProtocolEvent::UsageUpdated {
                    session_id: event_session,
                    cost_usd,
                    cost_known,
                    cost_billed,
                    ..
                } if event_session == session_id => {
                    state.cost_usd = cost_known.then_some(*cost_usd);
                    state.cost_billed = *cost_billed;
                    // PROV-13: an unpriced model reports no cost; say so once, where the person
                    // reading the run will see it, instead of leaving `cost_usd: null` unexplained.
                    if !*cost_known
                        && !matches!(mode, RunOutputMode::Silent)
                        && !UNPRICED_MODEL_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed)
                    {
                        eprintln!(
                            "warning: no price is known for this model, so the cost of this run cannot be computed (cost_usd is null) and a cost ceiling cannot stop it; add the model's price to the model metadata to get a cost"
                        );
                    }
                }
                _ => {}
            }
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
                    // Only this run's own session: a child's answer is never this run's reply.
                    if let ProtocolEvent::AssistantMessageCompleted {
                        session_id: event_session,
                        phase,
                        ..
                    } = &protocol_event
                    {
                        if event_session == session_id {
                            match serde_json::to_value(&protocol_event) {
                                Ok(value) => {
                                    let text =
                                        assistant_message_text(&protocol_event).map(str::to_string);
                                    if *phase == rightkit_agent_engine_protocol::AssistantMessagePhase::Final {
                                        state.final_assistant_event = Some(value.clone());
                                        state.final_result_text = text.clone();
                                    }
                                    state.last_assistant_event = Some(value);
                                    state.last_result_text = text;
                                }
                                Err(error) => tracing::warn!(
                                    %error,
                                    "run --json: could not keep the assistant answer"
                                ),
                            }
                        }
                    }
                }
            }
            let terminal = daemon_event_is_terminal(event);
            let is_resync = daemon_event_kind(event) == Some("streamResyncRequired");
            // CLI-01: a turn cut off by its round budget ends `completed` with a summary. That is
            // not a finished run: it exits 1 `timeout`, as the exit-code contract says.
            let cut_off = terminal
                && state.turn_limit_reached
                && matches!(
                    &protocol_event,
                    ProtocolEvent::SessionTurnFinished { status, .. } if status == "completed"
                );
            if cut_off {
                state.turn_limit_reached = false;
                state.output_error.get_or_insert(RunExitCode::TimeoutStall);
                state.failure_message = Some(TURN_LIMIT_MESSAGE.to_string());
                if matches!(mode, RunOutputMode::Text | RunOutputMode::Print) {
                    eprintln!("error: {TURN_LIMIT_MESSAGE}");
                }
            }
            match mode {
                RunOutputMode::StreamJson => {
                    emit_cli_json_line(&render_cli_stream_event(&protocol_event));
                }
                RunOutputMode::Json if terminal => {
                    let mut envelope = serde_json::json!({
                        "schema_version": crate::host_identity::schema("cli.rpc.v1"),
                        "session_id": session_id,
                        "terminal_event": protocol_event,
                    });
                    // Issue 450: the turn ends on `sessionTurnFinished`, which carries no reply.
                    // Keep the envelope's contract: the final answer stays `terminal_event`, and the
                    // turn-end event rides beside it.
                    if matches!(protocol_event, ProtocolEvent::SessionTurnFinished { .. }) {
                        let answer = state
                            .final_assistant_event
                            .take()
                            .or_else(|| state.last_assistant_event.take());
                        if let Some(answer) = answer {
                            envelope["turn_finished"] = envelope["terminal_event"].take();
                            envelope["terminal_event"] = answer;
                        }
                    }
                    // Flat result fields beside the envelope (added, never renamed): the reply
                    // text, whether the run failed, and what it cost.
                    envelope["result"] = serde_json::Value::String(
                        state
                            .final_result_text
                            .clone()
                            .or_else(|| state.last_result_text.clone())
                            .unwrap_or_else(|| state.assistant_deltas.clone()),
                    );
                    envelope["is_error"] = serde_json::Value::Bool(
                        cut_off || run_exit_code_for_event(&protocol_event).is_some(),
                    );
                    if run_exit_code_for_event(&protocol_event).is_some() {
                        state.failure_message =
                            Some(daemon_event_error_message(&protocol_event).to_string());
                    }
                    envelope["turns"] = state.turns.into();
                    envelope["duration_ms"] = RUN_FIRST_EVENT
                        .get()
                        .map_or(0, |first| first.elapsed().as_millis() as u64)
                        .into();
                    envelope["usage"] = serde_json::json!({
                        "input_tokens": state.input_tokens,
                        "cached_input_tokens": state.cached_input_tokens,
                        "output_tokens": state.output_tokens,
                        "reasoning_tokens": state.reasoning_tokens,
                    });
                    // Null, never 0, while the model has no curated price.
                    envelope["cost_usd"] = state
                        .cost_usd
                        .map_or(serde_json::Value::Null, serde_json::Value::from);
                    // Where the figure came from: the provider's own charge, or the price table.
                    envelope["cost_source"] = match state.cost_usd {
                        None => serde_json::Value::Null,
                        Some(_) if state.cost_billed => "provider".into(),
                        Some(_) => "estimated".into(),
                    };
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
                        emit_cli_error_json(&run_error_envelope(
                            code,
                            daemon_event_error_message(&protocol_event),
                        ));
                    } else {
                        emit_schema_output(state, schema);
                    }
                }
                RunOutputMode::Text => {
                    match &protocol_event {
                        ProtocolEvent::AssistantDelta {
                            session_id: event_session,
                            text,
                            is_reasoning: false,
                            ..
                        } if event_session == session_id => state.text_printed.push_str(text),
                        ProtocolEvent::AssistantMessageCompleted {
                            session_id: event_session,
                            ..
                        } if event_session == session_id => {
                            let printed = std::mem::take(&mut state.text_printed);
                            let complete = assistant_message_text(&protocol_event).unwrap_or("");
                            print_unstreamed_answer_text(&printed, complete);
                        }
                        _ => {}
                    }
                    print_event(&protocol_event, color)
                }
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
                    emit_cli_error_json(&run_error_envelope(
                        RunExitCode::SchemaViolation,
                        &message,
                    ));
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

// Lane 4 coverage for the SSE consumer added to `run_session`. Kept in a
// separate module (rather than the shared `main_runtime_run_session_tests.rs`
// include) so this lane's tests live entirely inside the file it owns.

// Headless R8: the unattended policy of a headless run.

#[cfg(test)]
mod interrupt_settle_tests {
    use super::*;

    #[test]
    fn an_interrupted_run_exits_one_in_every_output_mode() {
        for mode in [
            RunOutputMode::Text,
            RunOutputMode::Print,
            RunOutputMode::Json,
            RunOutputMode::StreamJson,
            RunOutputMode::SchemaJson,
            RunOutputMode::Silent,
        ] {
            assert_eq!(interrupted_exit(mode), 1);
        }
    }

    #[test]
    fn the_interrupted_object_is_retryable_and_the_unknown_one_is_not() {
        let interrupted = run_error_envelope(RunExitCode::UserAbort, "run interrupted");
        assert_eq!(interrupted["error"]["code"], "interrupted");
        assert_eq!(interrupted["error"]["retryable"], true);
        let unknown = run_error_envelope(
            RunExitCode::Unknown,
            "Turn cancellation is still awaiting engine settlement.",
        );
        assert_eq!(unknown["error"]["code"], "unknown");
        assert_eq!(unknown["error"]["retryable"], false);
        assert_eq!(RunExitCode::Unknown.as_i32(), 3);
        assert_eq!(
            run_error_exit(RunOutputMode::Silent, RunExitCode::Unknown, "x"),
            3
        );
    }

    #[test]
    fn the_settle_clock_starts_at_the_interrupt_and_restarts_for_the_next_turn() {
        let t0 = tokio::time::Instant::now();
        let mut at = None;
        assert_eq!(interrupt_settle_deadline(false, &mut at, t0), None);
        let first = interrupt_settle_deadline(true, &mut at, t0).expect("deadline");
        assert_eq!(first, t0 + RUN_INTERRUPT_SETTLE_WAIT);
        assert_eq!(
            RUN_INTERRUPT_SETTLE_WAIT,
            std::time::Duration::from_secs(10)
        );
        // A later pass keeps the first deadline: waiting does not extend it.
        let later = t0 + std::time::Duration::from_secs(7);
        assert_eq!(interrupt_settle_deadline(true, &mut at, later), Some(first));
        // A new turn resets the flag, and the next interrupt gets a fresh clock.
        assert_eq!(interrupt_settle_deadline(false, &mut at, later), None);
        let fresh = interrupt_settle_deadline(true, &mut at, later).expect("deadline");
        assert_eq!(fresh, later + RUN_INTERRUPT_SETTLE_WAIT);
    }
}

#[cfg(test)]
mod missing_workspace_tests {
    use super::*;

    #[test]
    fn a_stored_folder_that_is_gone_is_named_with_the_way_out() {
        let row = serde_json::json!({"workspaceRoot": "/definitely/not/here/w25"});
        let message = missing_session_workspace_message(&row, "/work/now").expect("a message");
        assert!(message.contains("/definitely/not/here/w25"), "{message}");
        assert!(
            message.contains("--fork") && message.contains("/work/now"),
            "{message}"
        );
    }

    #[test]
    fn an_existing_folder_or_the_current_one_or_no_folder_is_fine() {
        let here = std::env::temp_dir();
        let row = serde_json::json!({"workspaceRoot": here.to_string_lossy()});
        assert_eq!(missing_session_workspace_message(&row, "/work/now"), None);
        let gone = serde_json::json!({"workspaceRoot": "/definitely/not/here/w25"});
        assert_eq!(
            missing_session_workspace_message(&gone, "/definitely/not/here/w25"),
            None
        );
        assert_eq!(
            missing_session_workspace_message(&serde_json::json!({}), "/x"),
            None
        );
    }
}

#[cfg(test)]
mod schema_retry_tests {
    use super::*;

    fn schema() -> OutputSchema {
        let path = std::env::temp_dir().join(format!(
            "{}-schema-retry-{}-{}.json",
            crate::host_identity::app(),
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(
            &path,
            r#"{"type":"object","required":["n"],"properties":{"n":{"type":"integer"}}}"#,
        )
        .unwrap();
        let loaded = load_output_schema(&path.to_string_lossy()).expect("schema loads");
        let _ = std::fs::remove_file(&path);
        loaded
    }

    #[test]
    fn a_bad_answer_with_retries_left_is_held_back_and_asks_again() {
        let schema = schema();
        let mut state = RunOutputState {
            schema_retries_left: 2,
            assistant_message: Some("this is not json".to_string()),
            ..Default::default()
        };
        emit_schema_output(&mut state, Some(&schema));
        assert_eq!(state.schema_retries_left, 1);
        assert!(state.output_error.is_none(), "the run is not failed yet");
        assert!(
            !state.final_output_emitted,
            "nothing was printed for the rejected answer"
        );
        let prompt = state.schema_retry_prompt.expect("a follow-up");
        assert!(
            prompt.starts_with("Your answer did not match the schema:"),
            "{prompt}"
        );
        assert!(prompt.contains("only JSON"), "{prompt}");
    }

    #[test]
    fn a_bad_answer_with_no_retries_left_is_a_schema_violation() {
        let schema = schema();
        let mut state = RunOutputState {
            assistant_message: Some("this is not json".to_string()),
            ..Default::default()
        };
        emit_schema_output(&mut state, Some(&schema));
        assert_eq!(state.output_error, Some(RunExitCode::SchemaViolation));
        assert!(state.schema_retry_prompt.is_none());
        assert!(state.final_output_emitted);
    }

    #[test]
    fn a_conforming_answer_needs_no_retry() {
        let schema = schema();
        let mut state = RunOutputState {
            schema_retries_left: 3,
            assistant_message: Some(r#"{"n":3}"#.to_string()),
            ..Default::default()
        };
        emit_schema_output(&mut state, Some(&schema));
        assert!(state.output_error.is_none());
        assert!(state.schema_retry_prompt.is_none());
        assert_eq!(state.schema_retries_left, 3, "no retry was spent");
    }
}
