//! Exact host output shapes.
//!
//! Every shape here is one a shipped consumer already emits:
//!
//! - additional context: `{"hookSpecificOutput":{"hookEventName":E,"additionalContext":T}}` for
//!   `SessionStart`, `UserPromptSubmit`, `PostToolUse` (Pulse `codex_hook.rs`, Membrane `hook.rs`);
//! - Stop continuation: `{"decision":"block","reason":R}` (Pulse, Legion);
//! - Claude PreToolUse: `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":
//!   "deny"|"ask","permissionDecisionReason":R}}` (Legion `protocol.rs`);
//! - Codex PreToolUse denial: empty stdout, reason on stderr, exit 2 (Legion `codex.rs`; Codex has
//!   no `ask`, so it denies);
//! - `systemMessage` is the top-level field both tools show to the person (Pulse).
//!
//! A builder refuses an event that does not take the field, because a host rejects a whole
//! response that carries an unsupported variant (Membrane `HookHostResponseV1`).

use crate::event::{EventKind, Wire};
use serde_json::{json, Map, Value};
use std::fmt;
use std::io::Write;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputError {
    /// The event's host schema does not take this output.
    Unsupported { event: String, output: &'static str },
}

impl fmt::Display for OutputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported { event, output } => {
                write!(f, "{event} does not take {output}")
            }
        }
    }
}

impl std::error::Error for OutputError {}

/// What a hook process writes: stdout JSON, stderr text and an exit code.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Reply {
    pub stdout: Option<Value>,
    pub stderr: Option<String>,
    pub exit_code: i32,
}

impl Reply {
    /// Print nothing, exit 0.
    pub fn none() -> Self {
        Self::default()
    }

    pub fn is_none(&self) -> bool {
        self.stdout.is_none() && self.stderr.is_none() && self.exit_code == 0
    }

    /// Add context for the model. Only events in [`EventKind::takes_additional_context`].
    pub fn additional_context(kind: &EventKind, context: &str) -> Result<Self, OutputError> {
        if !kind.takes_additional_context() {
            return Err(OutputError::Unsupported {
                event: kind.to_string(),
                output: "additionalContext",
            });
        }
        Ok(Self {
            stdout: Some(json!({
                "hookSpecificOutput": {
                    "hookEventName": kind.as_str(),
                    "additionalContext": context,
                }
            })),
            ..Self::default()
        })
    }

    /// Keep a `Stop` run going: `{"decision":"block","reason":R}`.
    pub fn block_stop(reason: &str) -> Self {
        Self {
            stdout: Some(json!({"decision": "block", "reason": reason})),
            ..Self::default()
        }
    }

    /// Refuse a tool call. Claude: `permissionDecision: "deny"`, exit 0. Codex: stderr and exit 2.
    pub fn deny_tool(wire: Wire, reason: &str) -> Self {
        Self::refuse_tool(wire, reason, "deny")
    }

    /// Ask the person to confirm a tool call. Claude: `permissionDecision: "ask"`. Codex has no
    /// ask outcome and denies.
    pub fn ask_tool(wire: Wire, reason: &str) -> Self {
        Self::refuse_tool(wire, reason, "ask")
    }

    fn refuse_tool(wire: Wire, reason: &str, decision: &str) -> Self {
        match wire {
            Wire::Claude => Self {
                stdout: Some(json!({
                    "hookSpecificOutput": {
                        "hookEventName": "PreToolUse",
                        "permissionDecision": decision,
                        "permissionDecisionReason": reason,
                    }
                })),
                ..Self::default()
            },
            Wire::Codex => Self {
                stdout: None,
                stderr: Some(reason.to_owned()),
                exit_code: 2,
            },
        }
    }

    /// Add the top-level `systemMessage` the host shows to the person.
    pub fn with_system_message(mut self, message: &str) -> Self {
        if self.stderr.is_some() {
            return self;
        }
        let object = match self.stdout.take() {
            Some(Value::Object(object)) => object,
            _ => Map::new(),
        };
        let mut object = object;
        object.insert("systemMessage".into(), Value::String(message.to_owned()));
        self.stdout = Some(Value::Object(object));
        self
    }

    /// The single stdout line, if any.
    pub fn stdout_line(&self) -> Option<String> {
        self.stdout
            .as_ref()
            .and_then(|value| serde_json::to_string(value).ok())
    }

    /// Write stdout and stderr; returns the exit code. A write failure on stdout is ignored
    /// for exit-0 replies so the host is never failed by a closed pipe.
    pub fn write_to(&self, mut out: impl Write, mut err: impl Write) -> i32 {
        if let Some(text) = &self.stderr {
            let _ = writeln!(err, "{text}");
        }
        if let Some(line) = self.stdout_line() {
            let _ = writeln!(out, "{line}");
        }
        self.exit_code
    }
}
