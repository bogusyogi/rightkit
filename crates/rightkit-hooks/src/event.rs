//! Typed hook input for Claude Code and Codex.
//!
//! Field spellings follow `@rightkit/hooks` `normalizeHookEvent` (event: `hook_event_name`,
//! `hookEventName`, `event`; session: `thread_id`, `session_id`, `sessionId`; tool: `tool_name`,
//! `toolName`; tool use: `tool_use_id`, `toolUseId`). Every other typed field is one that shipped
//! Pulse, Legion or Membrane code already reads. The untouched payload stays in [`HookEvent::raw`].

use serde_json::Value;
use std::fmt;
use std::io::Read;
use std::path::Path;

/// Largest hook payload read from stdin (Pulse's `STDIN_CAP`).
pub const STDIN_CAP: u64 = 4 * 1024 * 1024;

/// Which harness sent the hook. The harness is chosen by the registration (Pulse's
/// `claude-hook` / `codex-hook`), not sniffed from the payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Wire {
    Claude,
    Codex,
}

impl Wire {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

impl fmt::Display for Wire {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A hook event name. Legion's lower-case aliases (`session-start`, `pre-effect`, ...) normalize to
/// the host names. Unknown names are kept verbatim, never rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventKind {
    SessionStart,
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    Stop,
    SubagentStart,
    SubagentStop,
    PreCompact,
    PostCompact,
    SessionEnd,
    Notification,
    Other(String),
}

impl EventKind {
    pub fn from_name(name: &str) -> Self {
        match name {
            "SessionStart" | "session-start" => Self::SessionStart,
            "UserPromptSubmit" | "user-prompt-submit" => Self::UserPromptSubmit,
            "PreToolUse" | "pre-effect" => Self::PreToolUse,
            "PostToolUse" | "post-effect" => Self::PostToolUse,
            "PostToolUseFailure" | "post-effect-failure" => Self::PostToolUseFailure,
            "Stop" | "stop" => Self::Stop,
            "SubagentStart" | "subagent-start" => Self::SubagentStart,
            "SubagentStop" | "subagent-stop" => Self::SubagentStop,
            "PreCompact" | "pre-compact" => Self::PreCompact,
            "PostCompact" | "post-compact" => Self::PostCompact,
            "SessionEnd" => Self::SessionEnd,
            "Notification" => Self::Notification,
            other => Self::Other(other.to_owned()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::PostToolUseFailure => "PostToolUseFailure",
            Self::Stop => "Stop",
            Self::SubagentStart => "SubagentStart",
            Self::SubagentStop => "SubagentStop",
            Self::PreCompact => "PreCompact",
            Self::PostCompact => "PostCompact",
            Self::SessionEnd => "SessionEnd",
            Self::Notification => "Notification",
            Self::Other(name) => name,
        }
    }

    /// Events whose host schema accepts `hookSpecificOutput.additionalContext` in shipped
    /// Pulse (`SessionStart`, `UserPromptSubmit`, `PostToolUse`). Other events would make the
    /// host reject the whole response, so the output builders refuse them.
    pub fn takes_additional_context(&self) -> bool {
        matches!(
            self,
            Self::SessionStart | Self::UserPromptSubmit | Self::PostToolUse
        )
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a payload was not accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The bytes are not JSON.
    Malformed(String),
    /// The JSON is not an object (`HookHost payload must be an object`).
    NotObject,
    /// No event name field (`HookHost event is required`).
    EventRequired,
    /// Input larger than [`STDIN_CAP`].
    TooLarge,
    /// Reading stdin failed.
    Io(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(e) => write!(f, "hook payload is not JSON: {e}"),
            Self::NotObject => f.write_str("HookHost payload must be an object"),
            Self::EventRequired => f.write_str("HookHost event is required"),
            Self::TooLarge => write!(f, "hook payload exceeds {STDIN_CAP} bytes"),
            Self::Io(e) => write!(f, "hook payload could not be read: {e}"),
        }
    }
}

impl std::error::Error for ParseError {}

/// One hook invocation, normalized.
#[derive(Clone, Debug, PartialEq)]
pub struct HookEvent {
    pub wire: Wire,
    pub kind: EventKind,
    /// `thread_id`, `session_id` or `sessionId`, first present non-blank string in that order.
    pub session_id: Option<String>,
    pub tool_name: Option<String>,
    pub tool_use_id: Option<String>,
    pub cwd: Option<String>,
    pub transcript_path: Option<String>,
    /// Codex's per-turn id (Legion's Codex Stop fixture).
    pub turn_id: Option<String>,
    /// Claude marks a subagent's hook input with `agent_id`.
    pub agent_id: Option<String>,
    pub parent_tool_use_id: Option<String>,
    /// `prompt`, falling back to `user_prompt` (Membrane's reader).
    pub prompt: Option<String>,
    pub stop_hook_active: Option<bool>,
    pub last_assistant_message: Option<String>,
    pub tool_input: Option<Value>,
    /// The original payload, unmodified.
    pub raw: Value,
}

fn first_string(object: &serde_json::Map<String, Value>, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        object
            .get(*name)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
    })
}

impl HookEvent {
    /// Normalize a payload that is already JSON.
    pub fn from_value(wire: Wire, payload: Value) -> Result<Self, ParseError> {
        let object = payload.as_object().ok_or(ParseError::NotObject)?;
        let name = first_string(object, &["hook_event_name", "hookEventName", "event"])
            .ok_or(ParseError::EventRequired)?;
        Ok(Self {
            wire,
            kind: EventKind::from_name(&name),
            session_id: first_string(object, &["thread_id", "session_id", "sessionId"]),
            tool_name: first_string(object, &["tool_name", "toolName"]),
            tool_use_id: first_string(object, &["tool_use_id", "toolUseId"]),
            cwd: first_string(object, &["cwd"]),
            transcript_path: first_string(object, &["transcript_path"]),
            turn_id: first_string(object, &["turn_id"]),
            agent_id: first_string(object, &["agent_id"]),
            parent_tool_use_id: first_string(object, &["parent_tool_use_id"]),
            prompt: first_string(object, &["prompt", "user_prompt"]),
            stop_hook_active: object.get("stop_hook_active").and_then(Value::as_bool),
            last_assistant_message: first_string(object, &["last_assistant_message"]),
            tool_input: object.get("tool_input").cloned(),
            raw: payload,
        })
    }

    pub fn from_slice(wire: Wire, bytes: &[u8]) -> Result<Self, ParseError> {
        let value: Value =
            serde_json::from_slice(bytes).map_err(|e| ParseError::Malformed(e.to_string()))?;
        Self::from_value(wire, value)
    }

    /// Read at most [`STDIN_CAP`] bytes and parse. Larger input is [`ParseError::TooLarge`].
    pub fn from_reader(wire: Wire, reader: impl Read) -> Result<Self, ParseError> {
        let mut raw = Vec::new();
        reader
            .take(STDIN_CAP + 1)
            .read_to_end(&mut raw)
            .map_err(|e| ParseError::Io(e.to_string()))?;
        if raw.len() as u64 > STDIN_CAP {
            return Err(ParseError::TooLarge);
        }
        Self::from_slice(wire, &raw)
    }

    /// Whether this hook belongs to the chat's own turn rather than a subagent's, Pulse's rule:
    /// every event but `PostToolUse` is the chat's own; for `PostToolUse` Claude's subagent hooks
    /// carry `agent_id`, and Codex reports the parent's session id with a different transcript
    /// file, so the transcript file name must contain the session id.
    pub fn is_own_turn(&self) -> bool {
        if self.kind != EventKind::PostToolUse {
            return true;
        }
        match self.wire {
            Wire::Claude => self.agent_id.is_none(),
            Wire::Codex => match (&self.session_id, &self.transcript_path) {
                (Some(session), Some(path)) => Path::new(path)
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().contains(session.as_str())),
                _ => false,
            },
        }
    }
}
