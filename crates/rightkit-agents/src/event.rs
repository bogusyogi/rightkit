//! Vendor-neutral agent events.

use std::collections::BTreeMap;

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

/// One MCP server as the CLI reported it at session start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerStatus {
    pub name: String,
    pub status: String,
}

/// The full Claude `system/init` payload. Emitted right after [`AgentEvent::Init`], which
/// keeps its shape for existing consumers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionInit {
    pub session_id: String,
    pub model: Option<String>,
    pub cwd: Option<String>,
    pub tools: Vec<String>,
    pub permission_mode: Option<String>,
    /// Capability names the CLI advertises, for example `interrupt_receipt_v1`.
    pub capabilities: Vec<String>,
    pub mcp_servers: Vec<McpServerStatus>,
}

/// Failure text, usage and per-model usage of one Claude `result` message. Emitted right
/// before the matching [`AgentEvent::Result`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResultSummary {
    pub subtype: String,
    pub is_error: bool,
    /// The turn ended because the host interrupted it.
    pub interrupted: bool,
    /// The joined `errors` entries, else the `result` text of an error result.
    pub failure_message: Option<String>,
    pub usage: Option<UsageCounts>,
    pub model_usage: BTreeMap<String, UsageCounts>,
}

/// A rate-limit report. Claude reports it as `rate_limit_event`; a vendor rejection of a usage
/// limit is reported with `status` `rejected`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RateLimitInfo {
    pub status: String,
    pub rate_limit_type: Option<String>,
    /// Unix seconds at which the limit resets, when the vendor says so.
    pub resets_at: Option<u64>,
    pub utilization: Option<f64>,
    pub overage_status: Option<String>,
    pub error_code: Option<String>,
}

/// Outcome of one control request sent to the CLI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ControlOutcome {
    Success { payload: Value },
    Error { message: String },
}

/// A `control_response` from the CLI, answering a control request this host sent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlResponse {
    pub request_id: String,
    pub outcome: ControlOutcome,
}

/// How a line parser treats messages it does not know.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UnknownPolicy {
    /// Unknown messages become [`AgentEvent::Unknown`]. The default.
    #[default]
    Lenient,
    /// Lines of unknown required kinds are refused with a [`ParseError`] whose
    /// [`ParseError::unknown_required_method`] names the kind.
    FailClosed,
}

/// One event from a vendor agent session.
///
/// Unknown vendor fields and messages never fail parsing; they surface as
/// [`AgentEvent::Unknown`] so a CLI upgrade degrades instead of breaking.
///
/// Variants added after 0.2.0 are additive. Where a variant repeats information another
/// variant already carries (`TextBlockDelta` next to `TextDelta`, `ToolResultContent` next to
/// `ToolResult`, `SessionInit` next to `Init`, `ResultDetail` next to `Result`), the older
/// variant is kept for compatibility: render from one of the pair, not both.
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
    /// The full session start payload (Claude). Follows [`AgentEvent::Init`].
    SessionInit(SessionInit),
    TextDelta {
        text: String,
    },
    /// The same text delta as [`AgentEvent::TextDelta`], with the content block index the
    /// partial-message stream reported.
    TextBlockDelta {
        index: Option<u64>,
        text: String,
    },
    /// A content block opened on the partial-message stream. `id` and `name` are set for
    /// `tool_use` blocks.
    ContentBlockStart {
        index: Option<u64>,
        block_type: String,
        id: Option<String>,
        name: Option<String>,
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
    /// The text of one `tool_result` block (text parts joined with newlines). Emitted right
    /// before the matching [`AgentEvent::ToolResult`].
    ToolResultContent {
        id: String,
        is_error: bool,
        content: String,
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
    /// Failure text, usage and per-model usage of the turn. Precedes [`AgentEvent::Result`].
    ResultDetail(ResultSummary),
    /// A rate-limit report from the vendor, or a rejected usage limit.
    RateLimit(RateLimitInfo),
    /// The CLI answered a control request this host sent.
    ControlResponse(ControlResponse),
    Stderr {
        line: String,
    },
    Unknown {
        raw: Value,
    },
}

/// The wire line was not usable JSON of the expected outer shape, or a parser under
/// [`UnknownPolicy::FailClosed`] refused an unknown required message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl ParseError {
    const UNKNOWN_REQUIRED_PREFIX: &'static str = "unknown required event: ";

    /// The refusal for an unknown required message kind named `method`.
    pub fn unknown_required(method: &str) -> Self {
        ParseError(format!("{}{method}", Self::UNKNOWN_REQUIRED_PREFIX))
    }

    /// The refused message kind when this error is a fail-closed refusal; `None` otherwise.
    pub fn unknown_required_method(&self) -> Option<&str> {
        self.0.strip_prefix(Self::UNKNOWN_REQUIRED_PREFIX)
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}
