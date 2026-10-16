//! Vendor-neutral agent events.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Token counts for one model call or one session total.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageCounts {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

/// A tool call the vendor refused under its permission mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionDenial {
    pub tool: String,
    pub tool_use_id: Option<String>,
    pub input: Value,
}

/// One event from a vendor agent session.
///
/// Unknown vendor fields and messages never fail parsing; they surface as
/// [`AgentEvent::Unknown`] so a CLI upgrade degrades instead of breaking.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AgentEvent {
    Init {
        session_id: String,
        model: Option<String>,
        tools: Vec<String>,
        permission_mode: Option<String>,
    },
    TextDelta {
        text: String,
    },
    Text {
        text: String,
        parent_tool_use_id: Option<String>,
    },
    Thinking {
        text: String,
        parent_tool_use_id: Option<String>,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
        parent_tool_use_id: Option<String>,
    },
    ToolResult {
        id: String,
        is_error: bool,
        parent_tool_use_id: Option<String>,
    },
    /// A tool call that spawns a subagent (Claude `Task`/`Agent`, Codex `collabAgentToolCall`).
    SubagentStarted {
        id: String,
        description: Option<String>,
        parent_tool_use_id: Option<String>,
    },
    SubagentFinished {
        id: String,
        is_error: bool,
    },
    ApprovalRequested {
        id: String,
        tool: String,
        input: Value,
    },
    ApprovalResolved {
        id: String,
    },
    Usage {
        usage: UsageCounts,
    },
    Compacted,
    Result {
        cost_usd: Option<f64>,
        duration_ms: Option<u64>,
        num_turns: Option<u64>,
        permission_denials: Vec<PermissionDenial>,
        is_error: bool,
        subtype: String,
    },
    Stderr {
        line: String,
    },
    Unknown {
        raw: Value,
    },
}

/// The wire line was not usable JSON of the expected outer shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}
