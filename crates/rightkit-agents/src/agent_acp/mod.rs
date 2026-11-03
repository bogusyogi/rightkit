//! ACP (Agent Client Protocol) wire types for the host app's stdio bridge.
//!
//! the host app acts as the **agent** side of ACP (<https://agentclientprotocol.com>).
//! This crate mirrors the stable v1 JSON-RPC 2.0 method surface published at
//! `agentclientprotocol/agent-client-protocol` (`schema/v1/meta.json`,
//! `agent-client-protocol-schema/src/v1/{agent,client,error}.rs`) with the exact
//! wire field names/casing, without pulling in the upstream crate (which drags
//! `schemars`/`derive_more`/`serde_with` for a JSON shape CR can hand-roll).
//!
//! Scope: the host app owns its own file/shell/execution tools, so this bridge
//! implements the agent-bound methods (what a client calls on CR) and the
//! client-bound methods CR must be able to SEND (`session/update`,
//! `session/request_permission`). It does not implement the client-bound
//! `fs/*` / `terminal/*` delegation methods — those let an agent ask the ACP
//! *client* (e.g. Zed) to perform file/terminal work through the client's own
//! sandbox, which is redundant for an agent that already has first-class local
//! tools. That is a deliberate scope choice, not an oversight; see
//! `docs/canon/daemon-master-canon-atoms.md` Lane 1D.

mod content;
mod error;
mod session;
mod session_update;

pub use content::{
    Annotations, ContentBlock, EmbeddedResource, ImageContent, ResourceLink, TextContent,
};
pub use error::{AcpError, AcpErrorCode};
pub use session::{
    prompt_to_text, CancelNotification, CloseSessionRequest, CloseSessionResponse,
    DeleteSessionRequest, DeleteSessionResponse, ListSessionsRequest, ListSessionsResponse,
    LoadSessionRequest, LoadSessionResponse, McpServerStdio, NewSessionRequest, NewSessionResponse,
    PromptRequest, PromptResponse, ResumeSessionRequest, ResumeSessionResponse, SessionInfo,
    StopReason,
};
pub use session_update::{
    outcome_to_approval, standard_permission_options, AgentMessageChunk, PermissionOption,
    PermissionOptionKind, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionUpdate, SessionUpdateKind,
    ToolCallStatus, ToolCallUpdate, UpdateSessionNotification,
};

use serde::{Deserialize, Serialize};

/// The ACP protocol version the host app's bridge speaks. Version 1 is the
/// current stable release; see `ProtocolVersion::LATEST` upstream.
pub const ACP_PROTOCOL_VERSION: u16 = 1;

/// Implementation name/version CR reports in `initialize`.
pub const ACP_AGENT_NAME: &str = "rightkit-agent";

// ── Agent-bound method names (client → CR) ─────────────────────────────────
pub const METHOD_INITIALIZE: &str = "initialize";
pub const METHOD_AUTHENTICATE: &str = "authenticate";
pub const METHOD_SESSION_NEW: &str = "session/new";
pub const METHOD_SESSION_LOAD: &str = "session/load";
pub const METHOD_SESSION_PROMPT: &str = "session/prompt";
pub const METHOD_SESSION_CANCEL: &str = "session/cancel";
pub const METHOD_SESSION_LIST: &str = "session/list";
pub const METHOD_SESSION_DELETE: &str = "session/delete";
pub const METHOD_SESSION_RESUME: &str = "session/resume";
pub const METHOD_SESSION_CLOSE: &str = "session/close";
pub const METHOD_LOGOUT: &str = "logout";

// ── Client-bound method names (CR → client) ────────────────────────────────
pub const METHOD_SESSION_REQUEST_PERMISSION: &str = "session/request_permission";
pub const METHOD_SESSION_UPDATE: &str = "session/update";

/// All agent-bound methods CR's stdio bridge answers. Used by tests and the
/// `initialize` capability payload to keep the advertised surface honest.
pub const AGENT_METHODS: &[&str] = &[
    METHOD_INITIALIZE,
    METHOD_AUTHENTICATE,
    METHOD_SESSION_NEW,
    METHOD_SESSION_LOAD,
    METHOD_SESSION_PROMPT,
    METHOD_SESSION_CANCEL,
    METHOD_SESSION_LIST,
    METHOD_SESSION_DELETE,
    METHOD_SESSION_RESUME,
    METHOD_SESSION_CLOSE,
    METHOD_LOGOUT,
];

fn is_false(value: &bool) -> bool {
    !*value
}

// ── initialize ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Implementation {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub version: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    #[serde(default, skip_serializing_if = "is_false")]
    pub fs: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub terminal: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InitializeRequest {
    pub protocol_version: u16,
    #[serde(default)]
    pub client_capabilities: ClientCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_info: Option<Implementation>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PromptCapabilities {
    #[serde(default, skip_serializing_if = "is_false")]
    pub image: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub audio: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub embedded_context: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionCapabilities {
    #[serde(default, skip_serializing_if = "is_false")]
    pub load: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub list: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub delete: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub resume: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub close: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    #[serde(default)]
    pub prompt_capabilities: PromptCapabilities,
    #[serde(default)]
    pub session_capabilities: SessionCapabilities,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AuthMethod {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    pub protocol_version: u16,
    #[serde(default)]
    pub agent_capabilities: AgentCapabilities,
    #[serde(default)]
    pub auth_methods: Vec<AuthMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_info: Option<Implementation>,
}

/// CR's stdio bridge does not require login before use (the daemon is already
/// authenticated by its own launch token); `authenticate` is a documented no-op
/// that always succeeds so clients that unconditionally probe it don't stall.
pub fn default_initialize_response() -> InitializeResponse {
    InitializeResponse {
        protocol_version: ACP_PROTOCOL_VERSION,
        agent_capabilities: AgentCapabilities {
            prompt_capabilities: PromptCapabilities {
                image: true,
                audio: false,
                embedded_context: true,
            },
            session_capabilities: SessionCapabilities {
                load: true,
                list: true,
                delete: true,
                resume: true,
                close: true,
            },
        },
        auth_methods: Vec::new(),
        agent_info: Some(Implementation {
            name: ACP_AGENT_NAME.to_string(),
            title: Some("Agent".to_string()),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }),
    }
}

/// Negotiates the protocol version: CR only speaks v1, so any client request
/// is answered with v1 regardless of what the client asked for (matches the
/// spec's "client should disconnect if it doesn't support this version").
pub fn negotiate_protocol_version(_requested: u16) -> u16 {
    ACP_PROTOCOL_VERSION
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AuthenticateRequest {
    pub method_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AuthenticateResponse {}
