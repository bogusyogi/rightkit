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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver_host::{parse_claude_stream_line, ClaudeLaunchOptions};
    use serde_json::json;

    const PROMPT_LINE: &str = r#"{"type":"control_request","request_id":"req-7","request":{"subtype":"can_use_tool","tool_name":"mcp__host__write_file","input":{"path":"a.txt","content":"hi"},"tool_use_id":"toolu_1","agent_id":"main"}}"#;
    const INIT: &str = r#"{"type":"system","subtype":"init","session_id":"s1"}"#;

    #[test]
    fn can_use_tool_line_parses_into_a_request() {
        let request = parse_claude_can_use_tool(PROMPT_LINE).unwrap().unwrap();
        assert_eq!(request.request_id, "req-7");
        assert_eq!(request.tool_name, "mcp__rightkit__write_file");
        assert_eq!(request.input, json!({"path": "a.txt", "content": "hi"}));
        assert_eq!(request.tool_use_id.as_deref(), Some("toolu_1"));
        assert_eq!(request.agent_id.as_deref(), Some("main"));
    }

    #[test]
    fn allow_frame_carries_updated_input_and_deny_frame_the_message() {
        let allow = claude_permission_response_frame(
            "req-7",
            &ClaudePermissionDecision::Allow {
                updated_input: json!({"path": "a.txt"}),
            },
        );
        assert_eq!(allow["type"], "control_response");
        assert_eq!(allow["response"]["subtype"], "success");
        assert_eq!(allow["response"]["request_id"], "req-7");
        assert_eq!(allow["response"]["response"]["behavior"], "allow");
        assert_eq!(
            allow["response"]["response"]["updatedInput"],
            json!({"path": "a.txt"})
        );
        let deny = claude_permission_response_frame(
            "req-7",
            &ClaudePermissionDecision::deny("operator declined"),
        );
        let body = &deny["response"]["response"];
        assert_eq!(deny["response"]["request_id"], "req-7");
        assert_eq!(body["behavior"], "deny");
        assert_eq!(body["message"], "operator declined");
        assert!(body.get("updatedInput").is_none());
    }

    #[test]
    fn only_explicit_allow_answers_allow() {
        let input = json!({"command": "ls"});
        assert_eq!(
            ClaudePermissionDecision::from_answer(&json!({"behavior": "allow"}), &input),
            ClaudePermissionDecision::Allow {
                updated_input: input.clone()
            }
        );
        let decline = json!({"decision": "decline", "reason": "must use gateway"});
        assert_eq!(
            ClaudePermissionDecision::from_answer(&decline, &input),
            ClaudePermissionDecision::deny("must use gateway")
        );
        assert!(matches!(
            ClaudePermissionDecision::from_answer(&Value::Null, &input),
            ClaudePermissionDecision::Deny { .. }
        ));
        assert!(matches!(
            ClaudePermissionDecision::from_answer(&json!({"behavior": "ask"}), &input),
            ClaudePermissionDecision::Deny { .. }
        ));
    }

    #[test]
    fn other_lines_are_not_permission_requests_and_incomplete_ones_error() {
        assert_eq!(parse_claude_can_use_tool(INIT).unwrap(), None);
        let interrupt =
            r#"{"type":"control_request","request_id":"i","request":{"subtype":"interrupt"}}"#;
        assert_eq!(parse_claude_can_use_tool(interrupt).unwrap(), None);
        let missing_id =
            r#"{"type":"control_request","request":{"subtype":"can_use_tool","tool_name":"x"}}"#;
        assert!(parse_claude_can_use_tool(missing_id).is_err());
        let missing_tool =
            r#"{"type":"control_request","request_id":"r","request":{"subtype":"can_use_tool"}}"#;
        assert!(parse_claude_can_use_tool(missing_tool).is_err());
        // The shared parser still treats the request line as bookkeeping.
        assert!(parse_claude_stream_line(PROMPT_LINE, 1).unwrap().is_none());
    }

    #[test]
    fn stdio_prompt_flag_is_opt_in_and_not_duplicated() {
        let options = ClaudeLaunchOptions {
            executable: "claude".into(),
            cwd: ".".into(),
            mcp_config: "{}".into(),
            session_id: None,
            resume_session_id: None,
            model: None,
        };
        let baseline = options.launch_spec().unwrap();
        assert!(!baseline
            .args
            .iter()
            .any(|arg| arg == "--permission-prompt-tool"));
        let mut spec = options.launch_spec().unwrap();
        enable_stdio_permission_prompt_for_spec(&mut spec);
        enable_stdio_permission_prompt_for_spec(&mut spec);
        let tail = &spec.args[spec.args.len() - 2..];
        assert_eq!(tail, ["--permission-prompt-tool", "stdio"]);
        assert_eq!(
            spec.args
                .iter()
                .filter(|arg| *arg == "--permission-prompt-tool")
                .count(),
            1
        );
    }
}
