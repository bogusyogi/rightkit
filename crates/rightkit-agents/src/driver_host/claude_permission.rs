//! Claude `can_use_tool` control requests (decision #336).
//!
//! With `--permission-prompt-tool stdio` the Claude CLI asks its host before using a tool by
//! writing a `control_request` line on stdout:
//!
//! ```json
//! {"type":"control_request","request_id":"<id>","request":{"subtype":"can_use_tool",
//!  "tool_name":"mcp__coderight__write_file","input":{...},"tool_use_id":"toolu_..",
//!  "permission_suggestions":[...],"blocked_path":"..","agent_id":".."}}
//! ```
//!
//! and waits for a `control_response` line on stdin carrying the same `request_id`:
//!
//! ```json
//! {"type":"control_response","response":{"subtype":"success","request_id":"<id>",
//!  "response":{"behavior":"allow","updatedInput":{...}}}}
//! {"type":"control_response","response":{"subtype":"success","request_id":"<id>",
//!  "response":{"behavior":"deny","message":"..."}}}
//! ```
//!
//! the host app never decides here. The daemon answers through the ordinary approval flow (same
//! card, same session grants) and hands the answer to [`ClaudePermissionDecision::from_answer`]:
//! only an explicit `allow` produces an allow. The request parse, the allow/deny frames and the
//! `--permission-prompt-tool stdio` flag are `crate::claude` (`parse_can_use_tool`,
//! `PermissionDecision::frame`, `enable_permission_prompt`).

use crate::claude as rk;
use crate::driver_host::{invalid, DriverHostError, DriverKind, LaunchSpec};
use serde_json::Value;

/// Launch-spec form of RightKit's `enable_permission_prompt` for the persistent (bidirectional)
/// host: routes tool-permission prompts to the host over the stdio control channel. The frozen
/// baseline argv from `ClaudeLaunchOptions::launch_spec` is left untouched.
pub fn enable_stdio_permission_prompt_for_spec(spec: &mut LaunchSpec) {
    rk::enable_permission_prompt(&mut spec.args);
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClaudePermissionRequest {
    /// Correlation id the `control_response` must echo.
    pub request_id: String,
    pub tool_name: String,
    pub input: Value,
    pub tool_use_id: Option<String>,
    pub agent_id: Option<String>,
    pub blocked_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClaudePermissionDecision {
    Allow { updated_input: Value },
    Deny { message: String },
}

impl ClaudePermissionDecision {
    pub fn deny(message: impl Into<String>) -> Self {
        Self::Deny {
            message: message.into(),
        }
    }

    /// Interpret a daemon-side answer. Only an explicit `{"behavior":"allow"}` (the shape the
    /// existing `permission_prompt` tool returns) or `{"decision":"allow"}` allows; every other
    /// shape, including the Codex-style `{"decision":"decline"}`, denies.
    pub fn from_answer(answer: &Value, original_input: &Value) -> Self {
        let verdict = answer
            .get("behavior")
            .or_else(|| answer.get("decision"))
            .and_then(Value::as_str);
        if verdict == Some("allow") {
            let updated = answer
                .get("updatedInput")
                .filter(|input| input.is_object())
                .cloned()
                .unwrap_or_else(|| original_input.clone());
            return Self::Allow {
                updated_input: updated,
            };
        }
        let message = answer
            .get("message")
            .or_else(|| answer.get("reason"))
            .and_then(Value::as_str)
            .unwrap_or("Denied by Agent");
        Self::deny(message)
    }

    fn to_rightkit(&self) -> rk::PermissionDecision {
        match self {
            Self::Allow { updated_input } => rk::PermissionDecision::Allow {
                updated_input: updated_input.clone(),
            },
            Self::Deny { message } => rk::PermissionDecision::Deny {
                message: message.clone(),
            },
        }
    }
}

/// Parse one stdout line. `Ok(None)` when it is not a `can_use_tool` control request.
pub fn parse_claude_can_use_tool(
    line: &str,
) -> Result<Option<ClaudePermissionRequest>, DriverHostError> {
    // Cheap pre-filter: most stdout lines are not control requests.
    if !line.contains("can_use_tool") {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(line)
        .map_err(|error| invalid(DriverKind::Claude, error.to_string()))?;
    if value.get("type").and_then(Value::as_str) != Some("control_request") {
        return Ok(None);
    }
    let Some(request) = rk::parse_can_use_tool(&value) else {
        return Ok(None);
    };
    if request.request_id.is_empty() {
        return Err(invalid(
            DriverKind::Claude,
            "can_use_tool missing request_id",
        ));
    }
    if request.tool_name.is_empty() {
        return Err(invalid(
            DriverKind::Claude,
            "can_use_tool missing tool_name",
        ));
    }
    Ok(Some(ClaudePermissionRequest {
        request_id: request.request_id,
        tool_name: request.tool_name,
        input: request.input,
        tool_use_id: request.tool_use_id,
        agent_id: request.agent_id,
        blocked_path: request.blocked_path,
    }))
}

/// The `control_response` line (RightKit's allow/deny frame) for a decision, as JSON.
pub fn claude_permission_response_frame(
    request_id: &str,
    decision: &ClaudePermissionDecision,
) -> Value {
    serde_json::from_str(decision.to_rightkit().frame(request_id).trim_end()).unwrap_or(Value::Null)
}
