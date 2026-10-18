//! Claude Code `stream-json` protocol: line parser, frame writer, argv builder, control
//! channel and permission helpers.
//!
//! Lifted and adapted from CodeRight (`engine/crates/driver_host/src/claude.rs` and
//! `cli_runtime/transports/claude_stream_json.rs`). The structural rules are kept: the
//! `system/init` session id, `stream_event` text deltas, full `assistant` messages as the
//! authoritative tool-use source, `user` tool results, `result` as the turn outcome, and
//! control frames as transport bookkeeping. CodeRight's `NormalizedEvent` and MCP flags are
//! not carried over. Unknown messages surface as [`AgentEvent::Unknown`] unless the parser is
//! built with [`UnknownPolicy::FailClosed`].

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use serde_json::{json, Value};

use crate::event::{
    AgentEvent, ControlOutcome, ControlResponse, McpServerStatus, ParseError, PermissionDenial,
    RateLimitInfo, ResultSummary, SessionInit, UnknownPolicy, UsageCounts,
};

/// Tool names Claude Code uses for a call that spawns a subagent.
const SUBAGENT_TOOLS: &[&str] = &["Task", "Agent"];

/// Permission modes the host allows on the argv. `bypassPermissions` is refused.
pub const PERMISSION_MODES: &[&str] = &["default", "acceptEdits", "dontAsk", "plan"];

/// Capability a session advertises in `system/init` when it reports interrupt receipts.
pub const INTERRUPT_RECEIPT_CAPABILITY: &str = "interrupt_receipt_v1";

/// How long [`answer_can_use_tool`] waits for a host answer before it fails closed.
pub const DEFAULT_PERMISSION_TIMEOUT: Duration = Duration::from_secs(300);

/// Denial message sent when no host answer arrives (no responder, or the timeout passed).
pub const NO_ANSWER_MESSAGE: &str = "Denied: no host answer in time";

/// Top-level `type` values this parser knows. Anything else is unknown.
const KNOWN_MESSAGE_TYPES: &[&str] = &[
    "system",
    "stream_event",
    "assistant",
    "user",
    "result",
    "control_request",
    "control_response",
    "rate_limit_event",
];

/// Known telemetry `system` subtypes. They never fail a turn, whatever they are named.
const TELEMETRY_SUBTYPES: &[&str] = &[
    "hook_started",
    "hook_response",
    "hook_progress",
    "status",
    "thinking_tokens",
    "api_retry",
    "session_state_changed",
    "commands_changed",
    "files_persisted",
    "local_command_output",
    "informational",
    "plugin_install",
    "worker_shutting_down",
    "background_tasks_changed",
];

/// A non-telemetry `system` subtype containing one of these words fails closed.
const REQUIRED_SUBTYPE_WORDS: &[&str] = &["permission", "denied", "tool", "result", "error"];

/// Any `system` message carrying one of these top-level keys fails closed.
const REQUIRED_PAYLOAD_KEYS: &[&str] = &[
    "tool_use_id",
    "tool_name",
    "tool_result",
    "permission",
    "permissions",
    "permission_denials",
    "decision",
    "result",
    "is_error",
];

/// How much of the user's own Claude configuration the session loads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConfigMode {
    /// User settings, plugins, hooks and CLAUDE.md load (CodeRight daily use).
    #[default]
    Full,
    /// `--bare`: no user settings, plugins or hooks; reproducible (Pulse).
    Bare,
}

/// Inputs to [`build_args`]. Arguments exclude the program itself.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClaudeOptions {
    pub config: ConfigMode,
    pub model: Option<String>,
    pub resume_session_id: Option<String>,
    pub session_id: Option<String>,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub permission_mode: Option<String>,
    pub max_budget_usd: Option<f64>,
    pub append_system_prompt: Option<String>,
    /// Path to a settings file passed with `--settings` (used later to register hooks).
    pub settings_path: Option<String>,
    pub include_partial_messages: bool,
    /// Route tool-permission prompts to the host over stdin/stdout as `can_use_tool`
    /// control requests (answered with [`permission_allow_frame`] / [`permission_deny_frame`])
    /// instead of leaving them to the CLI's own non-interactive policy. `Session` sets this.
    pub host_approvals: bool,
}

/// Argv options added after the original [`ClaudeOptions`] shape. They live in their own
/// struct so `ClaudeOptions` literals keep compiling. Pass them with [`build_args_with`], or
/// through `SessionExtras::claude` for a session.
///
/// `#[non_exhaustive]`: start from `ClaudeExtras::default()` and assign the fields you need.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct ClaudeExtras {
    /// `--tools <list>`, comma-joined. `Some(vec![])` emits `--tools ""`, which disables every
    /// built-in tool.
    pub tools: Option<Vec<String>>,
    /// `--no-chrome`.
    pub no_chrome: bool,
    /// `--fork-session`: start a new session from the resumed one. Requires
    /// `resume_session_id`.
    pub fork_session: bool,
    /// `--resume-session-at <id>`. Requires `resume_session_id`. The CLI's exact requirement
    /// is unverified; this crate requires the resume id so the flag is never orphaned.
    pub resume_session_at: Option<String>,
    /// `--max-turns <n>`.
    pub max_turns: Option<u32>,
    /// `--fallback-model <id>`. Must differ from `model`.
    pub fallback_model: Option<String>,
}

/// Flag and value that make Claude Code ask the host (the stdio control channel, not an MCP
/// tool) about each tool permission. Spec status: see `CHANGELOG.md` 0.2.0, unverified.
const HOST_PROMPT_ARG: (&str, &str) = ("--permission-prompt-tool", "stdio");

/// Build the argv (without the program) for one headless stream-json session.
///
/// Every id and model is checked for shape before it reaches argv: non-empty, at most 256
/// bytes, no leading `-`, and only ASCII letters, digits and `- _ . : / [ ]`.
pub fn build_args(options: &ClaudeOptions) -> Result<Vec<String>, String> {
    build_args_with(options, &ClaudeExtras::default())
}

/// [`build_args`] with the extra options in `extras`.
pub fn build_args_with(
    options: &ClaudeOptions,
    extras: &ClaudeExtras,
) -> Result<Vec<String>, String> {
    if options.session_id.is_some() && options.resume_session_id.is_some() {
        return Err("session_id and resume_session_id are mutually exclusive".into());
    }
    if let Some(model) = &options.model {
        check_id("model", model)?;
    }
    if let Some(model) = &extras.fallback_model {
        check_id("fallback_model", model)?;
        if options.model.as_deref() == Some(model.as_str()) {
            return Err("fallback_model must differ from model".into());
        }
    }
    if let Some(id) = &options.session_id {
        check_id("session_id", id)?;
    }
    if let Some(id) = &options.resume_session_id {
        check_id("resume_session_id", id)?;
    }
    if let Some(id) = &extras.resume_session_at {
        check_id("resume_session_at", id)?;
    }
    if extras.fork_session && options.resume_session_id.is_none() {
        return Err("fork_session requires resume_session_id".into());
    }
    if extras.resume_session_at.is_some() && options.resume_session_id.is_none() {
        return Err("resume_session_at requires resume_session_id".into());
    }
    if let Some(mode) = &options.permission_mode {
        check_permission_mode(mode)?;
    }
    if let Some(tools) = &extras.tools {
        if let Some(tool) = tools
            .iter()
            .find(|tool| tool.starts_with('-') || tool.contains(','))
        {
            return Err(format!(
                "tool name must not start with '-' or contain ',': {tool}"
            ));
        }
    }

    let mut args: Vec<String> = [
        "--print",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    if options.include_partial_messages {
        args.push("--include-partial-messages".into());
    }
    if options.config == ConfigMode::Bare {
        args.push("--bare".into());
    }
    if let Some(model) = &options.model {
        args.extend(["--model".to_string(), model.clone()]);
    }
    if let Some(id) = &options.session_id {
        args.extend(["--session-id".to_string(), id.clone()]);
    }
    if let Some(id) = &options.resume_session_id {
        args.extend(["--resume".to_string(), id.clone()]);
    }
    if extras.fork_session {
        args.push("--fork-session".into());
    }
    if let Some(id) = &extras.resume_session_at {
        args.extend(["--resume-session-at".to_string(), id.clone()]);
    }
    if let Some(tools) = &extras.tools {
        args.extend(["--tools".to_string(), tools.join(",")]);
    }
    if extras.no_chrome {
        args.push("--no-chrome".into());
    }
    if !options.allowed_tools.is_empty() {
        args.push("--allowedTools".into());
        args.extend(options.allowed_tools.iter().cloned());
    }
    if !options.disallowed_tools.is_empty() {
        args.push("--disallowedTools".into());
        args.extend(options.disallowed_tools.iter().cloned());
    }
    if let Some(mode) = &options.permission_mode {
        args.extend(["--permission-mode".to_string(), mode.clone()]);
    }
    if let Some(budget) = options.max_budget_usd {
        args.extend(["--max-budget-usd".to_string(), format!("{budget}")]);
    }
    if let Some(turns) = extras.max_turns {
        args.extend(["--max-turns".to_string(), turns.to_string()]);
    }
    if let Some(model) = &extras.fallback_model {
        args.extend(["--fallback-model".to_string(), model.clone()]);
    }
    if let Some(prompt) = &options.append_system_prompt {
        args.extend(["--append-system-prompt".to_string(), prompt.clone()]);
    }
    if options.host_approvals {
        args.extend([HOST_PROMPT_ARG.0.to_string(), HOST_PROMPT_ARG.1.to_string()]);
    }
    if let Some(path) = &options.settings_path {
        args.extend(["--settings".to_string(), path.clone()]);
    }
    Ok(args)
}

/// Shape check for an id or model name placed in argv. The message names the field, never
/// the value.
fn check_id(field: &str, value: &str) -> Result<(), String> {
    let shaped = !value.is_empty()
        && value.len() <= 256
        && !value.starts_with('-')
        && value.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/' | '[' | ']')
        });
    if shaped {
        Ok(())
    } else {
        Err(format!(
            "{field} is not a valid identifier for the Claude argv"
        ))
    }
}

/// Allowed permission modes. `bypassPermissions` gets its own refusal message.
pub fn check_permission_mode(mode: &str) -> Result<(), String> {
    if mode == "bypassPermissions" {
        return Err("permission mode bypassPermissions is refused by the host".into());
    }
    if PERMISSION_MODES.contains(&mode) {
        Ok(())
    } else {
        Err(format!(
            "permission mode must be one of {}",
            PERMISSION_MODES.join(", ")
        ))
    }
}

/// One stdin frame carrying a user turn, newline-terminated.
pub fn user_turn_frame(text: &str) -> String {
    let mut line =
        json!({"type": "user", "message": {"role": "user", "content": text}}).to_string();
    line.push('\n');
    line
}

/// One stdin frame asking Claude to interrupt the running turn, newline-terminated.
pub fn interrupt_frame(request_id: &str) -> String {
    let mut line = json!({
        "type": "control_request",
        "request_id": request_id,
        "request": {"subtype": "interrupt"}
    })
    .to_string();
    line.push('\n');
    line
}

/// One stdin frame answering a `can_use_tool` request with allow, newline-terminated.
/// `updated_input` is the tool input to run with; echo the request's input to approve as-is.
pub fn permission_allow_frame(request_id: &str, updated_input: &Value) -> String {
    control_success_frame(
        request_id,
        json!({"behavior": "allow", "updatedInput": updated_input}),
    )
}

/// One stdin frame answering a `can_use_tool` request with deny, newline-terminated.
/// `interrupt` also stops the running turn.
pub fn permission_deny_frame(request_id: &str, message: &str, interrupt: bool) -> String {
    control_success_frame(
        request_id,
        json!({"behavior": "deny", "message": message, "interrupt": interrupt}),
    )
}

fn control_success_frame(request_id: &str, response: Value) -> String {
    let mut line = json!({
        "type": "control_response",
        "response": {"subtype": "success", "request_id": request_id, "response": response}
    })
    .to_string();
    line.push('\n');
    line
}

fn control_request_frame(request_id: &str, request: Value) -> String {
    let mut line = json!({
        "type": "control_request",
        "request_id": request_id,
        "request": request
    })
    .to_string();
    line.push('\n');
    line
}

/// Process-wide counter behind [`next_request_id`].
static REQUEST_IDS: AtomicU64 = AtomicU64::new(1);

/// A control request id unique within this process: `rk-<kind>-<n>`.
pub fn next_request_id(kind: &str) -> String {
    format!("rk-{kind}-{}", REQUEST_IDS.fetch_add(1, Ordering::Relaxed))
}

/// `initialize` control request, newline-terminated. `hooks` is passed through unchanged
/// when given. Unverified: the wire body follows the issue's description of the control
/// channel, not a recorded session.
pub fn initialize_frame(request_id: &str, hooks: Option<&Value>) -> String {
    let mut request = json!({"subtype": "initialize"});
    if let Some(hooks) = hooks {
        request["hooks"] = hooks.clone();
    }
    control_request_frame(request_id, request)
}

/// `set_model` control request, newline-terminated. `None` selects the default model.
/// Unverified: `null` as the default is an assumption, not a recorded reply.
pub fn set_model_frame(request_id: &str, model: Option<&str>) -> Result<String, String> {
    if let Some(model) = model {
        check_id("model", model)?;
    }
    Ok(control_request_frame(
        request_id,
        json!({"subtype": "set_model", "model": model}),
    ))
}

/// `set_permission_mode` control request, newline-terminated. `bypassPermissions` is refused.
/// Unverified: the subtype and field names follow the issue, not a recorded session.
pub fn set_permission_mode_frame(request_id: &str, mode: &str) -> Result<String, String> {
    check_permission_mode(mode)?;
    Ok(control_request_frame(
        request_id,
        json!({"subtype": "set_permission_mode", "mode": mode}),
    ))
}

/// Parse a `control_response` envelope into its request id and outcome. `None` when the
/// envelope has no request id or an unknown subtype. Unverified: the error arm reads an
/// `error` string, as the issue describes.
pub fn parse_control_response(value: &Value) -> Option<ControlResponse> {
    let response = value.get("response")?;
    let request_id = str_field(response, "request_id")?;
    let outcome = match response.get("subtype").and_then(Value::as_str)? {
        "success" => ControlOutcome::Success {
            payload: response.get("response").cloned().unwrap_or(Value::Null),
        },
        "error" => ControlOutcome::Error {
            message: str_field(response, "error").unwrap_or_default(),
        },
        _ => return None,
    };
    Some(ControlResponse {
        request_id,
        outcome,
    })
}

/// Account identity from the `initialize` answer. Unverified key names: `accountInfo` with
/// `email`, `organization` (or `org`) and `plan`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaudeAccountIdentity {
    pub email: Option<String>,
    pub organization: Option<String>,
    pub plan: Option<String>,
}

/// The typed `initialize` answer. Unverified: `models` is read as strings or as objects
/// with a `value` field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InitializeResponse {
    pub account: Option<ClaudeAccountIdentity>,
    pub models: Vec<String>,
}

/// Parse the success payload of an `initialize` control response.
pub fn parse_initialize_response(payload: &Value) -> InitializeResponse {
    let account = payload
        .get("accountInfo")
        .map(|info| ClaudeAccountIdentity {
            email: str_field(info, "email"),
            organization: str_field(info, "organization").or_else(|| str_field(info, "org")),
            plan: str_field(info, "plan"),
        });
    let models: Vec<String> = payload
        .get("models")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.as_str()
                        .map(str::to_string)
                        .or_else(|| str_field(item, "value"))
                })
                .collect()
        })
        .unwrap_or_default();
    InitializeResponse { account, models }
}

/// The typed interrupt answer. `queued_user_messages` counts the messages the CLI reports as
/// queued when the turn was interrupted. Unverified key name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InterruptReceipt {
    pub queued_user_messages: u64,
}

/// Parse an interrupt answer. `None` unless the session advertised
/// [`INTERRUPT_RECEIPT_CAPABILITY`] in `system/init`.
pub fn parse_interrupt_receipt(
    payload: &Value,
    capabilities: &[String],
) -> Option<InterruptReceipt> {
    if !capabilities
        .iter()
        .any(|capability| capability == INTERRUPT_RECEIPT_CAPABILITY)
    {
        return None;
    }
    let queued = payload
        .get("queuedUserMessages")
        .and_then(Value::as_array)
        .map_or(0, |items| items.len() as u64);
    Some(InterruptReceipt {
        queued_user_messages: queued,
    })
}

/// A `can_use_tool` request: what the CLI asks the host to decide. Field names follow the
/// issue; the wire placement of `tool_use_id`, `agent_id` and `blocked_path` is unverified.
#[derive(Debug, Clone, PartialEq)]
pub struct CanUseTool {
    pub request_id: String,
    pub tool_name: String,
    pub input: Value,
    pub tool_use_id: Option<String>,
    pub agent_id: Option<String>,
    pub blocked_path: Option<String>,
}

/// Parse a `control_request` with subtype `can_use_tool`. `None` for any other message.
pub fn parse_can_use_tool(value: &Value) -> Option<CanUseTool> {
    let request = value.get("request")?;
    if request.get("subtype").and_then(Value::as_str) != Some("can_use_tool") {
        return None;
    }
    Some(CanUseTool {
        request_id: str_field(value, "request_id").unwrap_or_default(),
        tool_name: str_field(request, "tool_name").unwrap_or_default(),
        input: request.get("input").cloned().unwrap_or(Value::Null),
        tool_use_id: str_field(request, "tool_use_id"),
        agent_id: str_field(request, "agent_id"),
        blocked_path: str_field(request, "blocked_path"),
    })
}

/// The host's answer to a `can_use_tool` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionAnswer {
    Allow,
    /// `message` is shown to the model; `None` uses "Denied by the host".
    Deny {
        message: Option<String>,
    },
}

/// A decision ready to send back to the CLI.
#[derive(Debug, Clone, PartialEq)]
pub enum PermissionDecision {
    Allow { updated_input: Value },
    Deny { message: String },
}

impl PermissionDecision {
    /// Build the decision for a host answer. Allow runs the tool with the original input.
    pub fn from_answer(answer: PermissionAnswer, original_input: &Value) -> Self {
        match answer {
            PermissionAnswer::Allow => PermissionDecision::Allow {
                updated_input: original_input.clone(),
            },
            PermissionAnswer::Deny { message } => PermissionDecision::Deny {
                message: message.unwrap_or_else(|| "Denied by the host".to_string()),
            },
        }
    }

    /// The newline-terminated control response that carries this decision.
    pub fn frame(&self, request_id: &str) -> String {
        match self {
            PermissionDecision::Allow { updated_input } => {
                permission_allow_frame(request_id, updated_input)
            }
            PermissionDecision::Deny { message } => {
                permission_deny_frame(request_id, message, false)
            }
        }
    }
}

fn no_answer() -> PermissionDecision {
    PermissionDecision::Deny {
        message: NO_ANSWER_MESSAGE.to_string(),
    }
}

/// Wait for the host's answer to this request and fail closed. No thread is spawned.
/// The host creates one answer channel per request, dispatches `request` itself, and owns
/// the producer's cancellation & cleanup. Passing a receiver does not transfer producer
/// lifecycle ownership into this crate. Do not reuse an answer channel for other requests.
///
/// No receiver, timeout (use [`DEFAULT_PERMISSION_TIMEOUT`]) or disconnect yields denial.
/// The receiver is consumed, so a late producer send fails after this call returns. Hosts
/// must still cancel/join their own pending work; a failed send does not stop that work.
pub fn answer_can_use_tool(
    request: &CanUseTool,
    answer: Option<Receiver<PermissionAnswer>>,
    timeout: Duration,
) -> PermissionDecision {
    let Some(answer) = answer else {
        return no_answer();
    };
    match answer.recv_timeout(timeout) {
        Ok(answer) => PermissionDecision::from_answer(answer, &request.input),
        Err(_) => no_answer(),
    }
}

/// Make `args` ask the host about permissions. Idempotent: an existing permission-prompt
/// flag is set to the stdio channel and never duplicated.
pub fn enable_permission_prompt(args: &mut Vec<String>) {
    match args.iter().position(|arg| arg == HOST_PROMPT_ARG.0) {
        Some(index) => match args.get_mut(index + 1) {
            Some(value) => *value = HOST_PROMPT_ARG.1.to_string(),
            None => args.push(HOST_PROMPT_ARG.1.to_string()),
        },
        None => args.extend([HOST_PROMPT_ARG.0.to_string(), HOST_PROMPT_ARG.1.to_string()]),
    }
}

/// Stateful line parser. State tracks subagent tool ids so a later `tool_result` for a
/// `Task` call yields [`AgentEvent::SubagentFinished`].
#[derive(Debug, Default)]
pub struct StreamParser {
    subagents: HashSet<String>,
    policy: UnknownPolicy,
}

impl StreamParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// A parser with the given unknown-message policy.
    pub fn with_policy(policy: UnknownPolicy) -> Self {
        Self {
            subagents: HashSet::new(),
            policy,
        }
    }

    /// Parse one NDJSON line into zero or more events. `Err` for malformed JSON, a line that
    /// is not an object with a `type`, or (under [`UnknownPolicy::FailClosed`]) a line of an
    /// unknown required kind; see [`ParseError::unknown_required_method`].
    pub fn parse_line(&mut self, line: &str) -> Result<Vec<AgentEvent>, ParseError> {
        let value: Value =
            serde_json::from_str(line).map_err(|error| ParseError(error.to_string()))?;
        let message_type = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| ParseError("missing type".into()))?
            .to_string();
        if self.policy == UnknownPolicy::FailClosed {
            if let Some(method) = unknown_required_method(&value, &message_type) {
                return Err(ParseError::unknown_required(&method));
            }
        }
        let parent = parent_tool_use_id(&value);
        let mut events = Vec::new();
        match message_type.as_str() {
            "system" => self.system(&value, &mut events),
            "stream_event" => stream_event(&value, &mut events),
            "assistant" => self.assistant(&value, parent, &mut events),
            "user" => self.user(&value, parent, &mut events),
            "result" => {
                events.push(AgentEvent::ResultDetail(parse_result_summary(&value)));
                events.push(result_event(&value));
            }
            "control_request" => control_request(&value, &mut events),
            "control_response" => {
                events.extend(parse_control_response(&value).map(AgentEvent::ControlResponse));
            }
            "rate_limit_event" => events.push(AgentEvent::RateLimit(parse_rate_limit(&value))),
            _ => events.push(AgentEvent::Unknown { raw: value }),
        }
        Ok(events)
    }

    fn system(&mut self, value: &Value, events: &mut Vec<AgentEvent>) {
        match value.get("subtype").and_then(Value::as_str) {
            Some("init") => {
                let session_id = str_field(value, "session_id").unwrap_or_default();
                let tools = string_array(value, "tools");
                let model = str_field(value, "model");
                let permission_mode = str_field(value, "permissionMode");
                events.push(AgentEvent::Init {
                    session_id: session_id.clone(),
                    model: model.clone(),
                    tools: tools.clone(),
                    permission_mode: permission_mode.clone(),
                });
                events.push(AgentEvent::SessionInit(SessionInit {
                    session_id,
                    model,
                    cwd: str_field(value, "cwd"),
                    tools,
                    permission_mode,
                    capabilities: string_array(value, "capabilities"),
                    mcp_servers: mcp_servers(value),
                }));
            }
            Some("compact_boundary") => events.push(AgentEvent::Compacted),
            Some("hook_started" | "hook_response" | "status" | "thinking_tokens") => {}
            _ => events.push(AgentEvent::Unknown { raw: value.clone() }),
        }
    }

    fn assistant(&mut self, value: &Value, parent: Option<String>, events: &mut Vec<AgentEvent>) {
        let message = value.get("message");
        let blocks = message
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array);
        // Every tool_use block of the message is emitted, not only the first one.
        for block in blocks.into_iter().flatten() {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => events.push(AgentEvent::Text {
                    text: str_field(block, "text").unwrap_or_default(),
                    parent_tool_use_id: parent.clone(),
                }),
                Some("thinking") => events.push(AgentEvent::Thinking {
                    text: str_field(block, "thinking").unwrap_or_default(),
                    parent_tool_use_id: parent.clone(),
                }),
                Some("tool_use") => {
                    let id = str_field(block, "id").unwrap_or_default();
                    let name = str_field(block, "name").unwrap_or_default();
                    let input = block.get("input").cloned().unwrap_or(Value::Null);
                    if SUBAGENT_TOOLS.contains(&name.as_str()) {
                        self.subagents.insert(id.clone());
                        events.push(AgentEvent::SubagentStarted {
                            id: id.clone(),
                            description: input
                                .get("description")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            parent_tool_use_id: parent.clone(),
                        });
                    }
                    events.push(AgentEvent::ToolUse {
                        id,
                        name,
                        input,
                        parent_tool_use_id: parent.clone(),
                    });
                }
                _ => {}
            }
        }
        if let Some(usage) = message.and_then(|m| m.get("usage")) {
            events.push(AgentEvent::Usage {
                usage: usage_counts(usage),
            });
        }
    }

    fn user(&mut self, value: &Value, parent: Option<String>, events: &mut Vec<AgentEvent>) {
        let blocks = value
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array);
        for block in blocks.into_iter().flatten() {
            if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            let id = str_field(block, "tool_use_id").unwrap_or_default();
            let is_error = block
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            events.push(AgentEvent::ToolResultContent {
                id: id.clone(),
                is_error,
                content: tool_result_text(block),
                parent_tool_use_id: parent.clone(),
            });
            events.push(AgentEvent::ToolResult {
                id: id.clone(),
                is_error,
                parent_tool_use_id: parent.clone(),
            });
            if self.subagents.remove(&id) {
                events.push(AgentEvent::SubagentFinished { id, is_error });
            }
        }
    }
}

/// The unknown-message classifier behind [`UnknownPolicy::FailClosed`]. Returns the refused
/// kind, or `None` when the line may be handled.
fn unknown_required_method(value: &Value, message_type: &str) -> Option<String> {
    if !KNOWN_MESSAGE_TYPES.contains(&message_type) {
        return Some(message_type.to_string());
    }
    if message_type != "system" {
        return None;
    }
    let subtype = value.get("subtype").and_then(Value::as_str).unwrap_or("");
    if matches!(subtype, "init" | "compact_boundary") {
        return None;
    }
    let telemetry = TELEMETRY_SUBTYPES.contains(&subtype) || subtype.starts_with("task_");
    let named_required = !telemetry
        && REQUIRED_SUBTYPE_WORDS
            .iter()
            .any(|word| subtype.contains(word));
    let carries_required = value.as_object().is_some_and(|object| {
        REQUIRED_PAYLOAD_KEYS
            .iter()
            .any(|key| object.contains_key(*key))
    });
    if named_required || carries_required {
        Some(format!("system:{subtype}"))
    } else {
        None
    }
}

fn stream_event(value: &Value, events: &mut Vec<AgentEvent>) {
    let Some(event) = value.get("event") else {
        return;
    };
    let index = event.get("index").and_then(Value::as_u64);
    match event.get("type").and_then(Value::as_str) {
        Some("content_block_start") => {
            let block = event.get("content_block");
            events.push(AgentEvent::ContentBlockStart {
                index,
                block_type: block.and_then(|b| str_field(b, "type")).unwrap_or_default(),
                id: block.and_then(|b| str_field(b, "id")),
                name: block.and_then(|b| str_field(b, "name")),
            });
        }
        Some("content_block_delta") => {
            let delta = event.get("delta");
            if delta.and_then(|d| d.get("type")).and_then(Value::as_str) == Some("text_delta") {
                let text = delta
                    .and_then(|d| d.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                events.push(AgentEvent::TextDelta { text: text.clone() });
                events.push(AgentEvent::TextBlockDelta { index, text });
            }
        }
        _ => {}
    }
}

fn control_request(value: &Value, events: &mut Vec<AgentEvent>) {
    let request = value.get("request");
    if request
        .and_then(|r| r.get("subtype"))
        .and_then(Value::as_str)
        == Some("can_use_tool")
    {
        events.push(AgentEvent::ApprovalRequested {
            id: str_field(value, "request_id").unwrap_or_default(),
            tool: request
                .and_then(|r| r.get("tool_name"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            input: request
                .and_then(|r| r.get("input"))
                .cloned()
                .unwrap_or(Value::Null),
        });
    }
}

/// The rate-limit report of a `rate_limit_event`. Unverified: the fields are read from
/// `rate_limit_info` when present, otherwise from the message itself; the names follow the
/// issue (`status`, `rateLimitType`, `resetsAt`, `utilization`, `overageStatus`, `errorCode`).
pub fn parse_rate_limit(value: &Value) -> RateLimitInfo {
    let info = value.get("rate_limit_info").unwrap_or(value);
    RateLimitInfo {
        status: str_field(info, "status").unwrap_or_default(),
        rate_limit_type: str_field(info, "rateLimitType"),
        resets_at: info.get("resetsAt").and_then(Value::as_u64),
        utilization: info.get("utilization").and_then(Value::as_f64),
        overage_status: str_field(info, "overageStatus"),
        error_code: str_field(info, "errorCode"),
    }
}

/// The detail of a `result` message: failure text, usage and per-model usage. `interrupted`
/// is set for subtype `interrupted`, the subtype the session fixtures use for a host
/// interrupt. Per-model keys (`inputTokens` and so on) are unverified.
pub fn parse_result_summary(value: &Value) -> ResultSummary {
    let subtype = str_field(value, "subtype").unwrap_or_default();
    let is_error = value
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let errors: Vec<String> = value
        .get("errors")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.as_str()
                        .map(str::to_string)
                        .or_else(|| str_field(item, "message"))
                })
                .collect::<Vec<String>>()
        })
        .unwrap_or_default();
    let failure_message = if !errors.is_empty() {
        Some(errors.join("\n"))
    } else if is_error {
        str_field(value, "result")
    } else {
        None
    };
    let model_usage: BTreeMap<String, UsageCounts> = value
        .get("modelUsage")
        .and_then(Value::as_object)
        .map(|models| {
            models
                .iter()
                .map(|(name, usage)| (name.clone(), model_usage_counts(usage)))
                .collect::<BTreeMap<String, UsageCounts>>()
        })
        .unwrap_or_default();
    ResultSummary {
        interrupted: subtype == "interrupted",
        is_error,
        failure_message,
        usage: value.get("usage").map(usage_counts),
        model_usage,
        subtype,
    }
}

fn result_event(value: &Value) -> AgentEvent {
    let denials = value
        .get("permission_denials")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| PermissionDenial {
                    tool: str_field(item, "tool_name").unwrap_or_default(),
                    tool_use_id: str_field(item, "tool_use_id"),
                    input: item.get("tool_input").cloned().unwrap_or(Value::Null),
                })
                .collect()
        })
        .unwrap_or_default();
    AgentEvent::Result {
        cost_usd: value.get("total_cost_usd").and_then(Value::as_f64),
        duration_ms: value.get("duration_ms").and_then(Value::as_u64),
        num_turns: value.get("num_turns").and_then(Value::as_u64),
        permission_denials: denials,
        is_error: value
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        subtype: str_field(value, "subtype").unwrap_or_default(),
    }
}

/// Text of a `tool_result` block. A string content is returned as is; an array content
/// contributes its `text` parts joined with newlines.
fn tool_result_text(block: &Value) -> String {
    match block.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<&str>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn mcp_servers(value: &Value) -> Vec<McpServerStatus> {
    value
        .get("mcp_servers")
        .or_else(|| value.get("mcpServers"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| McpServerStatus {
                    name: str_field(item, "name").unwrap_or_default(),
                    status: str_field(item, "status").unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn string_array(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn usage_counts(usage: &Value) -> UsageCounts {
    let get = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    UsageCounts {
        input: get("input_tokens"),
        output: get("output_tokens"),
        cache_read: get("cache_read_input_tokens"),
        cache_write: get("cache_creation_input_tokens"),
    }
}

/// Per-model usage uses camelCase keys. Unverified against a recorded result.
fn model_usage_counts(usage: &Value) -> UsageCounts {
    let get = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    UsageCounts {
        input: get("inputTokens"),
        output: get("outputTokens"),
        cache_read: get("cacheReadInputTokens"),
        cache_write: get("cacheCreationInputTokens"),
    }
}

fn parent_tool_use_id(value: &Value) -> Option<String> {
    value
        .get("parent_tool_use_id")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn str_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_refuses_bypass_and_hostile_ids_before_interpolation() {
        let bypass = ClaudeOptions {
            permission_mode: Some("bypassPermissions".into()),
            ..ClaudeOptions::default()
        };
        assert!(build_args(&bypass).is_err());
        let hostile = ClaudeOptions {
            model: Some("--dangerously".into()),
            ..ClaudeOptions::default()
        };
        assert!(build_args(&hostile).is_err());
        let fork = ClaudeExtras {
            fork_session: true,
            ..ClaudeExtras::default()
        };
        assert!(build_args_with(&ClaudeOptions::default(), &fork).is_err());
    }

    #[test]
    fn argv_emits_tool_lockdown_and_fallback_flags() {
        let options = ClaudeOptions {
            model: Some("opus".into()),
            ..ClaudeOptions::default()
        };
        let extras = ClaudeExtras {
            tools: Some(Vec::new()),
            no_chrome: true,
            fallback_model: Some("sonnet".into()),
            max_turns: Some(4),
            ..ClaudeExtras::default()
        };
        let args = build_args_with(&options, &extras).unwrap();
        let at = |flag: &str| args.iter().position(|arg| arg == flag).unwrap();
        assert_eq!(args[at("--tools") + 1], "");
        assert!(args.contains(&"--no-chrome".to_string()));
        assert_eq!(args[at("--fallback-model") + 1], "sonnet");
        assert_eq!(args[at("--max-turns") + 1], "4");
        let same = ClaudeOptions {
            model: Some("opus".into()),
            ..ClaudeOptions::default()
        };
        let same_extras = ClaudeExtras {
            fallback_model: Some("opus".into()),
            ..ClaudeExtras::default()
        };
        assert!(build_args_with(&same, &same_extras).is_err());
        // The original entry point never emits the extension flags.
        let plain = build_args(&options).unwrap();
        assert!(!plain.contains(&"--no-chrome".to_string()));
        assert!(!plain.contains(&"--fallback-model".to_string()));
    }

    #[test]
    fn enable_permission_prompt_is_idempotent() {
        let mut args = vec!["--print".to_string()];
        enable_permission_prompt(&mut args);
        enable_permission_prompt(&mut args);
        assert_eq!(
            args.iter().filter(|arg| *arg == HOST_PROMPT_ARG.0).count(),
            1
        );
        assert_eq!(args.last().map(String::as_str), Some(HOST_PROMPT_ARG.1));
    }

    #[test]
    fn fail_closed_refuses_required_kinds_and_keeps_telemetry() {
        let mut strict = StreamParser::with_policy(UnknownPolicy::FailClosed);
        let refused = strict.parse_line(r#"{"type":"future_kind"}"#).unwrap_err();
        assert_eq!(refused.unknown_required_method(), Some("future_kind"));
        assert!(strict
            .parse_line(r#"{"type":"system","subtype":"task_progress"}"#)
            .is_ok());
        assert!(strict
            .parse_line(r#"{"type":"system","subtype":"permission_request"}"#)
            .is_err());
        assert!(strict
            .parse_line(r#"{"type":"system","subtype":"hook_progress","tool_use_id":"t1"}"#)
            .is_err());
        let mut lenient = StreamParser::new();
        assert!(lenient.parse_line(r#"{"type":"future_kind"}"#).is_ok());
    }

    #[test]
    fn stream_parser_emits_lossless_tool_result_and_result_detail() {
        let mut parser = StreamParser::new();
        let events = parser
            .parse_line(r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}]}}"#)
            .unwrap();
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolResultContent { content, .. } if content == "a\nb"
        )));
        let events = parser
            .parse_line(r#"{"type":"result","subtype":"error_max_turns","is_error":true,"errors":["too many"],"modelUsage":{"m":{"inputTokens":3}}}"#)
            .unwrap();
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ResultDetail(summary) if summary.failure_message.as_deref() == Some("too many")
                && summary.model_usage["m"].input == 3
        )));
    }
}
