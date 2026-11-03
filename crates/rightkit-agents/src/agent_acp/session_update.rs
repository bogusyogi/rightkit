//! Client-bound ACP types: `session/update` notifications CR streams while a
//! turn runs, and `session/request_permission` CR sends when a tool call
//! needs operator approval. Mirrors
//! `agent-client-protocol-schema/src/v1/client.rs` field names/casing for the
//! subset CR emits (message/thought chunks, tool-call updates, plan updates
//! are out of scope for this lane — CR does not yet stream a structured
//! plan over ACP; message + tool-call updates are the load-bearing ones a
//! real ACP client renders).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSessionNotification {
    pub session_id: String,
    pub update: SessionUpdate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentMessageChunk {
    pub content: ContentBlock,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallUpdate {
    pub tool_call_id: String,
    pub title: String,
    pub status: ToolCallStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_input: Option<serde_json::Value>,
}

/// Discriminated `sessionUpdate` payload. CR emits `agent_message_chunk` for
/// streamed assistant text, `user_message_chunk` to echo the accepted user
/// turn, and `tool_call_update` for tool lifecycle — the three updates a
/// baseline ACP client needs to render a live turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "sessionUpdate", rename_all = "snake_case")]
pub enum SessionUpdate {
    UserMessageChunk(AgentMessageChunk),
    AgentMessageChunk(AgentMessageChunk),
    AgentThoughtChunk(AgentMessageChunk),
    ToolCallUpdate(ToolCallUpdate),
}

/// Convenience discriminator for callers that only need to branch on kind
/// without matching the full payload (used by the daemon-event bridge).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionUpdateKind {
    UserMessageChunk,
    AgentMessageChunk,
    AgentThoughtChunk,
    ToolCallUpdate,
}

impl SessionUpdate {
    pub fn kind(&self) -> SessionUpdateKind {
        match self {
            SessionUpdate::UserMessageChunk(_) => SessionUpdateKind::UserMessageChunk,
            SessionUpdate::AgentMessageChunk(_) => SessionUpdateKind::AgentMessageChunk,
            SessionUpdate::AgentThoughtChunk(_) => SessionUpdateKind::AgentThoughtChunk,
            SessionUpdate::ToolCallUpdate(_) => SessionUpdateKind::ToolCallUpdate,
        }
    }
}

// ── session/request_permission ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionOptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
    pub option_id: String,
    pub name: String,
    pub kind: PermissionOptionKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RequestPermissionRequest {
    pub session_id: String,
    pub tool_call: ToolCallUpdate,
    pub options: Vec<PermissionOption>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SelectedPermissionOutcome {
    pub option_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RequestPermissionOutcome {
    Cancelled,
    #[serde(rename_all = "camelCase")]
    Selected(SelectedPermissionOutcome),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RequestPermissionResponse {
    pub outcome: RequestPermissionOutcome,
}

/// CR's built-in tool-approval model is a binary allow/deny (see
/// `POST /sessions/:id/tool-approval`). The standard two-option ACP
/// permission set that maps onto it 1:1 without inventing new semantics.
pub fn standard_permission_options() -> Vec<PermissionOption> {
    vec![
        PermissionOption {
            option_id: "allow_once".to_string(),
            name: "Allow".to_string(),
            kind: PermissionOptionKind::AllowOnce,
        },
        PermissionOption {
            option_id: "reject_once".to_string(),
            name: "Reject".to_string(),
            kind: PermissionOptionKind::RejectOnce,
        },
    ]
}

/// Maps a selected ACP permission option back to CR's binary approve/deny.
/// Any `allow_*` option id approves; anything else (including `Cancelled`
/// and unrecognized option ids) denies — fail-closed on ambiguity.
pub fn outcome_to_approval(outcome: &RequestPermissionOutcome) -> bool {
    match outcome {
        RequestPermissionOutcome::Selected(selected) => selected.option_id.starts_with("allow"),
        RequestPermissionOutcome::Cancelled => false,
    }
}
