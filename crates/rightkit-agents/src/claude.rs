//! Claude Code `stream-json` protocol: line parser, frame writer, argv builder.
//!
//! Lifted and adapted from CodeRight (`engine/crates/driver_host/src/claude.rs` and
//! `cli_runtime/transports/claude_stream_json.rs`). The structural rules are kept: the
//! `system/init` session id, `stream_event` text deltas, full `assistant` messages as the
//! authoritative tool-use source, `user` tool results, `result` as the turn outcome, and
//! control frames as transport bookkeeping. CodeRight's `NormalizedEvent`, MCP flags and
//! fail-closed unknown-event policy are not carried over: here unknown messages surface
//! as [`AgentEvent::Unknown`].

use std::collections::HashSet;

use serde_json::{json, Value};

use crate::event::{AgentEvent, ParseError, PermissionDenial, UsageCounts};

/// Tool names Claude Code uses for a call that spawns a subagent.
const SUBAGENT_TOOLS: &[&str] = &["Task", "Agent"];

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

/// Flag and value that make Claude Code ask the host (the stdio control channel, not an MCP
/// tool) about each tool permission. Spec status: see `CHANGELOG.md` 0.2.0, unverified.
const HOST_PROMPT_ARG: (&str, &str) = ("--permission-prompt-tool", "stdio");

/// Build the argv (without the program) for one headless stream-json session.
pub fn build_args(options: &ClaudeOptions) -> Result<Vec<String>, String> {
    if options.session_id.is_some() && options.resume_session_id.is_some() {
        return Err("session_id and resume_session_id are mutually exclusive".into());
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
        args.extend(["--model".into(), model.clone()]);
    }
    if let Some(id) = &options.session_id {
        args.extend(["--session-id".into(), id.clone()]);
    }
    if let Some(id) = &options.resume_session_id {
        args.extend(["--resume".into(), id.clone()]);
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
        args.extend(["--permission-mode".into(), mode.clone()]);
    }
    if let Some(budget) = options.max_budget_usd {
        args.extend(["--max-budget-usd".into(), format!("{budget}")]);
    }
    if let Some(prompt) = &options.append_system_prompt {
        args.extend(["--append-system-prompt".into(), prompt.clone()]);
    }
    if options.host_approvals {
        args.extend([HOST_PROMPT_ARG.0.into(), HOST_PROMPT_ARG.1.into()]);
    }
    if let Some(path) = &options.settings_path {
        args.extend(["--settings".into(), path.clone()]);
    }
    Ok(args)
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

/// Stateful line parser. State tracks subagent tool ids so a later `tool_result` for a
/// `Task` call yields [`AgentEvent::SubagentFinished`].
#[derive(Debug, Default)]
pub struct StreamParser {
    subagents: HashSet<String>,
}

impl StreamParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse one NDJSON line into zero or more events. `Err` only for malformed JSON or a
    /// line that is not an object with a `type`.
    pub fn parse_line(&mut self, line: &str) -> Result<Vec<AgentEvent>, ParseError> {
        let value: Value =
            serde_json::from_str(line).map_err(|error| ParseError(error.to_string()))?;
        let message_type = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| ParseError("missing type".into()))?
            .to_string();
        let parent = parent_tool_use_id(&value);
        let mut events = Vec::new();
        match message_type.as_str() {
            "system" => self.system(&value, &mut events),
            "stream_event" => stream_event(&value, &mut events),
            "assistant" => self.assistant(&value, parent, &mut events),
            "user" => self.user(&value, parent, &mut events),
            "result" => events.push(result_event(&value)),
            "control_request" => control_request(&value, &mut events),
            "control_response" | "rate_limit_event" => {}
            _ => events.push(AgentEvent::Unknown { raw: value }),
        }
        Ok(events)
    }

    fn system(&mut self, value: &Value, events: &mut Vec<AgentEvent>) {
        match value.get("subtype").and_then(Value::as_str) {
            Some("init") => {
                let session_id = str_field(value, "session_id").unwrap_or_default();
                let tools = value
                    .get("tools")
                    .and_then(Value::as_array)
                    .map(|tools| {
                        tools
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                events.push(AgentEvent::Init {
                    session_id,
                    model: str_field(value, "model"),
                    tools,
                    permission_mode: str_field(value, "permissionMode"),
                });
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

fn stream_event(value: &Value, events: &mut Vec<AgentEvent>) {
    let Some(event) = value.get("event") else {
        return;
    };
    if event.get("type").and_then(Value::as_str) != Some("content_block_delta") {
        return;
    }
    let delta = event.get("delta");
    if delta.and_then(|d| d.get("type")).and_then(Value::as_str) == Some("text_delta") {
        events.push(AgentEvent::TextDelta {
            text: delta
                .and_then(|d| d.get("text"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        });
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

fn usage_counts(usage: &Value) -> UsageCounts {
    let get = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    UsageCounts {
        input: get("input_tokens"),
        output: get("output_tokens"),
        cache_read: get("cache_read_input_tokens"),
        cache_write: get("cache_creation_input_tokens"),
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
