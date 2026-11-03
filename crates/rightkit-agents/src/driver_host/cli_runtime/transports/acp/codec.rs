//! ACP wire codec.
//!
//! PR 7 of the architecture plan. Implements the JSON-RPC 2.0 envelope
//! used by the Agent Client Protocol over a child process's
//! stdin/stdout. The transport is a *client* of ACP: it speaks
//! JSON-RPC 2.0 requests to the vendor CLI and consumes the vendor's
//! JSON-RPC 2.0 responses and notifications.
//!
//! The codec is intentionally transport-agnostic. The transport owns
//! the file descriptors and is responsible for feeding complete lines
//! to [`decode_message`]. The codec is only the envelope.
//!
//! JSON-RPC 2.0 reference: <https://www.jsonrpc.org/specification>.
//! ACP v1 references: <https://agentclientprotocol.com>.
//!
//! The codec uses [`serde_json`] for the wire format but never assumes
//! a specific schema for the `params` or `result` payloads — those are
//! always preserved as opaque [`serde_json::Value`] so the higher
//! layers can decode them with their own ACP DTO types.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// ACP protocol version the host app speaks. Matches the stable v1
/// surface; the official crate labels it `ProtocolVersion::V0001_00_00`
/// upstream. We use the integer form on the wire because that is what
/// every published ACP client and server uses.
pub const ACP_PROTOCOL_VERSION: u16 = 1;

/// JSON-RPC 2.0 protocol marker. Every envelope member must carry it.
pub const JSONRPC_VERSION: &str = "2.0";

/// A unique request-id generator. JSON-RPC 2.0 requires the id to be a
/// string, number, or null; we use monotonically-increasing positive
/// integers so log forensics can sort responses by arrival. Each
/// transport keeps its own generator so multiple in-flight transports
/// never reuse an id within the same process.
#[derive(Debug, Default)]
pub struct AcpRequestIdGenerator {
    next: AtomicU64,
}

impl AcpRequestIdGenerator {
    pub fn new() -> Self {
        Self {
            next: AtomicU64::new(1),
        }
    }

    /// Allocate the next request id. Thread-safe; each call returns a
    /// distinct id, including under concurrent use.
    pub fn next_id(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    /// Inspect the id that will be handed out by the next call. Used
    /// in tests; never read the counter and act on the value, only
    /// inspect.
    pub fn peek(&self) -> u64 {
        self.next.load(Ordering::Relaxed)
    }
}

/// An outbound JSON-RPC 2.0 request. `params` is preserved verbatim
/// (as `Value::Null` if the method carries no parameters) because
/// the ACP spec varies the per-method parameter shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcpRequest {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    pub params: Value,
}

impl AcpRequest {
    /// Build a new request with the given method and params. The
    /// id is allocated from the supplied generator.
    pub fn new(
        id_generator: &AcpRequestIdGenerator,
        method: impl Into<String>,
        params: Value,
    ) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: id_generator.next_id(),
            method: method.into(),
            params,
        }
    }

    /// Encode the request as a single newline-terminated JSON line.
    /// ACP requires each JSON-RPC frame to be exactly one line of
    /// UTF-8 text terminated by `\n`. Callers write the returned
    /// string followed by `\n` to the child's stdin.
    pub fn encode(&self) -> Result<String, AcpCodecError> {
        serde_json::to_string(self).map_err(|error| AcpCodecError::Encode {
            method: self.method.clone(),
            message: error.to_string(),
        })
    }
}

/// An inbound JSON-RPC 2.0 response. Carries exactly one of `result`
/// or `error`; the codec preserves both so callers can match
/// exhaustively without re-parsing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcpResponse {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<AcpRpcError>,
}

/// An inbound JSON-RPC 2.0 notification. Identified by the absence
/// of the `id` field. ACP servers use notifications to stream
/// `session/update` events while a turn is running.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcpNotification {
    pub jsonrpc: String,
    pub method: String,
    pub params: Value,
}

/// JSON-RPC 2.0 error object. The standard shape: `code`, `message`,
/// and optional `data`. the host app treats any non-zero `code` as a
/// hard failure on the wire; the broker does not differentiate
/// vendor-specific codes by number alone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcpRpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

// `serde_json::Value` does not derive `Eq`, but JSON values carried in an
// ACP error are treated as opaque diagnostics. ACP error equality is used
// only for deterministic transport tests, where structural `PartialEq` is
// sufficient to uphold this marker trait.
impl Eq for AcpRpcError {}

/// One decoded inbound frame. The transport switches on this enum
/// to decide whether to forward a response to a waiting request, to
/// stream a notification event, or to surface a protocol-level error.
#[derive(Debug, Clone, PartialEq)]
pub enum AcpInbound {
    Response(AcpResponse),
    Notification(AcpNotification),
    /// The frame was a valid JSON-RPC envelope but failed one of
    /// the protocol checks (missing jsonrpc, missing method, etc).
    /// The transport raises this as a hard error.
    ProtocolError(AcpRpcError),
}

/// What the wire codec can reject. The codec never invents a value
/// on its own; every variant corresponds to something the JSON-RPC
/// 2.0 spec defines as malformed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpCodecError {
    /// The line could not be parsed as a JSON document.
    DecodeJson { line: String, message: String },
    /// The line parsed as JSON but did not carry `jsonrpc: "2.0"`.
    BadEnvelope { line: String, message: String },
    /// The line was a JSON-RPC envelope but was missing the `method`
    /// or `id` fields the transport needs to route it.
    MissingField { line: String, field: String },
    /// `serde_json::to_string` failed when encoding an outbound
    /// request. Carries the originating method for diagnostics.
    Encode { method: String, message: String },
}

impl std::fmt::Display for AcpCodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AcpCodecError::DecodeJson { line, message } => {
                write!(f, "ACP frame is not valid JSON: {message} (line={line:?})")
            }
            AcpCodecError::BadEnvelope { line, message } => {
                write!(
                    f,
                    "ACP frame failed envelope check: {message} (line={line:?})"
                )
            }
            AcpCodecError::MissingField { line, field } => {
                write!(
                    f,
                    "ACP frame missing required field {field} (line={line:?})"
                )
            }
            AcpCodecError::Encode { method, message } => {
                write!(f, "ACP encode failed for method {method}: {message}")
            }
        }
    }
}

impl std::error::Error for AcpCodecError {}

/// Parse one line into the appropriate [`AcpInbound`] variant. The
/// codec never produces a synthetic response; if the JSON-RPC envelope
/// is malformed the function returns [`AcpCodecError`].
pub fn decode_message(line: &str) -> Result<AcpInbound, AcpCodecError> {
    let value: Value = serde_json::from_str(line).map_err(|error| AcpCodecError::DecodeJson {
        line: line.to_string(),
        message: error.to_string(),
    })?;
    let object = value
        .as_object()
        .ok_or_else(|| AcpCodecError::BadEnvelope {
            line: line.to_string(),
            message: "root must be a JSON object".to_string(),
        })?;
    let jsonrpc = object
        .get("jsonrpc")
        .and_then(Value::as_str)
        .ok_or_else(|| AcpCodecError::MissingField {
            line: line.to_string(),
            field: "jsonrpc".to_string(),
        })?;
    if jsonrpc != JSONRPC_VERSION {
        return Err(AcpCodecError::BadEnvelope {
            line: line.to_string(),
            message: format!("jsonrpc must be \"{JSONRPC_VERSION}\", got {jsonrpc:?}"),
        });
    }
    let method = object.get("method").and_then(Value::as_str);
    let id = object.get("id").cloned();

    match (method, id) {
        // Response: has an `id`, no `method`.
        (None, Some(id_value)) => {
            let id = id_to_u64(&id_value).ok_or_else(|| AcpCodecError::BadEnvelope {
                line: line.to_string(),
                message: format!("response id must be a positive integer, got {id_value:?}"),
            })?;
            let result = object.get("result").cloned();
            let error = match object.get("error") {
                Some(err_value) => {
                    Some(serde_json::from_value(err_value.clone()).map_err(|error| {
                        AcpCodecError::BadEnvelope {
                            line: line.to_string(),
                            message: format!("error object is malformed: {error}"),
                        }
                    })?)
                }
                None => None,
            };
            Ok(AcpInbound::Response(AcpResponse {
                jsonrpc: JSONRPC_VERSION.to_string(),
                id,
                result,
                error,
            }))
        }
        // Notification: has a `method`, no `id`.
        (Some(method), None) => {
            let params = object.get("params").cloned().unwrap_or(Value::Null);
            Ok(AcpInbound::Notification(AcpNotification {
                jsonrpc: JSONRPC_VERSION.to_string(),
                method: method.to_string(),
                params,
            }))
        }
        // Both: malformed per the spec.
        (Some(method), Some(id_value)) => Err(AcpCodecError::BadEnvelope {
            line: line.to_string(),
            message: format!(
                "frame carries both method={method:?} and id={id_value:?}; \
                 JSON-RPC 2.0 forbids this combination"
            ),
        }),
        // Neither: a server-side error frame still has no method but
        // is identified by the absence of an id at the wire level.
        // Per the spec a true error is sent as a response, so this
        // combination is malformed here.
        (None, None) => Err(AcpCodecError::MissingField {
            line: line.to_string(),
            field: "method or id".to_string(),
        }),
    }
}

fn id_to_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        // JSON-RPC allows string ids, but ACP clients/servers in the
        // wild always use integer ids. Accept strings that parse as
        // positive integers so we do not break against an exotic peer.
        Value::String(text) => text.parse::<u64>().ok(),
        _ => None,
    }
}

/// Convenience wrapper that exposes the codec's encode + decode
/// surface together. The transport constructs one codec per process
/// (because the request-id generator must be per-process) and stores
/// it for the lifetime of the binding.
#[derive(Debug, Default)]
pub struct AcpCodec {
    ids: AcpRequestIdGenerator,
}

impl AcpCodec {
    pub fn new() -> Self {
        Self {
            ids: AcpRequestIdGenerator::new(),
        }
    }

    /// Borrow the id generator so callers can mint a request id and
    /// bundle it into the params struct before calling
    /// [`AcpRequest::new`].
    pub fn id_generator(&self) -> &AcpRequestIdGenerator {
        &self.ids
    }

    pub fn decode(&self, line: &str) -> Result<AcpInbound, AcpCodecError> {
        decode_message(line)
    }

    pub fn encode_request(
        &self,
        method: impl Into<String>,
        params: Value,
    ) -> Result<String, AcpCodecError> {
        AcpRequest::new(&self.ids, method, params).encode()
    }

    /// Encode an `initialize` request carrying the the host app client
    /// identity and capability advertisement. The transport invokes
    /// this as its first wire action after the child process comes
    /// up.
    pub fn encode_initialize(&self, version: &str) -> Result<String, AcpCodecError> {
        let client_capabilities: Value =
            serde_json::from_str(super::capabilities::HOST_CLIENT_CAPABILITIES)
                .expect("Agent client capability JSON is constant");
        self.encode_request(
            "initialize",
            json!({
                "protocolVersion": ACP_PROTOCOL_VERSION,
                "clientInfo": {
                    "name": super::capabilities::HOST_CLIENT_NAME,
                    "version": version,
                },
                "clientCapabilities": client_capabilities,
            }),
        )
    }

    /// Encode a `session/new` request. The transport passes the
    /// resolved working directory and the *single* the host app MCP
    /// server entry. The transport is responsible upstream for
    /// refusing any launch that did not pin exactly one MCP entry;
    /// this helper assumes that invariant.
    pub fn encode_session_new(
        &self,
        cwd: &str,
        mcp_server: &serde_json::Value,
        policy_hash: &str,
        binding_id: &str,
        session_id: &str,
        agent_id: &str,
    ) -> Result<String, AcpCodecError> {
        self.encode_request(
            "session/new",
            json!({
                "cwd": cwd,
                "mcpServers": [mcp_server],
                "_meta": {
                    "rightkitBindingId": binding_id,
                    "rightkitPolicyHash": policy_hash,
                },
                "clientSessionId": session_id,
                "agentId": agent_id,
            }),
        )
    }

    /// Encode a `session/load` request that re-attaches to an
    /// existing vendor session. The transport passes the resolved
    /// MCP server entry unchanged so the resumed session inherits the
    /// same harness bindings.
    pub fn encode_session_load(
        &self,
        vendor_session_id: &str,
        cwd: &str,
        mcp_server: &serde_json::Value,
        binding_id: &str,
    ) -> Result<String, AcpCodecError> {
        self.encode_request(
            "session/load",
            json!({
                "sessionId": vendor_session_id,
                "cwd": cwd,
                "mcpServers": [mcp_server],
                "_meta": {
                    "rightkitBindingId": binding_id,
                },
            }),
        )
    }

    /// Encode a `session/prompt` request carrying the user's text.
    /// `model_id` is forwarded when present so the server can switch
    /// models mid-session.
    pub fn encode_session_prompt(
        &self,
        vendor_session_id: &str,
        text: &str,
        model_id: Option<&str>,
    ) -> Result<String, AcpCodecError> {
        let mut params = json!({
            "sessionId": vendor_session_id,
            "prompt": [{ "type": "text", "text": text }],
        });
        if let Some(model) = model_id {
            params["modelId"] = json!(model);
        }
        self.encode_request("session/prompt", params)
    }

    /// Encode a `session/cancel` request. The server is expected to
    /// abort the in-flight turn and emit a final `session/update`
    /// notification of type `turn/cancelled` before returning.
    pub fn encode_session_cancel(&self, vendor_session_id: &str) -> Result<String, AcpCodecError> {
        self.encode_request("session/cancel", json!({ "sessionId": vendor_session_id }))
    }
}

/// Decoded shape of a `session/update` notification. The transport
/// pulls these from `read_event` and the executor feeds them into the
/// pipeline that emits [`crate::driver_host::NormalizedEvent`]s.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcpSessionUpdate {
    /// The vendor session the update belongs to. ACP servers tag
    /// every streamed update with the originating session so a
    /// shared child process can host multiple sessions in flight.
    pub session_id: String,
    /// The update payload. The shape is provider-specific; the
    /// transport passes it through to the higher-layer normalizer
    /// unchanged.
    pub update: AcpUpdateBody,
}

/// Body of a `session/update` notification. We tag on `type` so the
/// downstream parser can switch on the wire kind, and we preserve the
/// full inner object so a vendor extension stays observable in
/// telemetry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AcpUpdateBody {
    /// A streamed chunk of the assistant message.
    AgentMessageChunk {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chunk_id: Option<String>,
    },
    /// A streamed chunk of reasoning content. Treated as presentation
    /// only — never promoted to a ToolCall.
    AgentThoughtChunk {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chunk_id: Option<String>,
    },
    /// A tool call request from the model. The downstream
    /// normalizer turns this into a `NormalizedEvent::ToolCallStarted`
    /// and consults the permission broker before any executor is
    /// reached.
    ToolCall {
        call_id: String,
        #[serde(default)]
        title: Option<String>,
        tool_name: String,
        #[serde(default)]
        server_name: Option<String>,
        #[serde(default)]
        arguments: Value,
        #[serde(default)]
        kind: Option<String>,
    },
    /// A tool result forwarded by the model. Downstream this becomes
    /// a `NormalizedEvent::ToolResult`.
    ToolResult {
        call_id: String,
        #[serde(default)]
        is_error: bool,
        #[serde(default)]
        content: Value,
    },
    /// A request from the model to ask the user for approval. The
    /// transport must consult the permission broker; for ACP the
    /// default is to deny because the harness already owns the
    /// approval surface.
    ApprovalRequest {
        request_id: String,
        action: String,
        #[serde(default)]
        details: Value,
    },
    /// A plan update from the model. Forwarded verbatim to the
    /// normalizer so the desktop UI can render it.
    Plan { entries: Vec<Value> },
    /// Terminal marker for a successful turn.
    TurnCompleted,
    /// Terminal marker for a failed turn. Carries the reason for
    /// forensics; the executor surfaces it as
    /// `NormalizedEvent::TurnFailed`.
    TurnFailed { message: String },
    /// Terminal marker for a turn that the user (or harness)
    /// cancelled. The transport surfaces this as
    /// `NormalizedEvent::TurnInterrupted`.
    TurnCancelled,
    /// The server emitted a body shape we did not anticipate. The
    /// transport refuses to promote unknown execution events to
    /// [`crate::driver_host::NormalizedEventKind::ToolCallStarted`] or any
    /// downstream executor; the parser surfaces an error.
    #[serde(other)]
    Unknown,
}

impl AcpUpdateBody {
    /// True for terminal update bodies. The transport stops reading
    /// the stream after the first terminal body for a turn.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            AcpUpdateBody::TurnCompleted
                | AcpUpdateBody::TurnFailed { .. }
                | AcpUpdateBody::TurnCancelled
        )
    }
}

/// Parse a `session/update` notification's `params` field. Returns
/// [`AcpCodecError::MissingField`] when `sessionId` is absent because
/// the transport cannot route an untagged update to its session.
pub fn parse_session_update(params: &Value) -> Result<AcpSessionUpdate, AcpCodecError> {
    let object = params
        .as_object()
        .ok_or_else(|| AcpCodecError::BadEnvelope {
            line: params.to_string(),
            message: "session/update params must be a JSON object".to_string(),
        })?;
    let session_id = object
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| AcpCodecError::MissingField {
            line: params.to_string(),
            field: "sessionId".to_string(),
        })?
        .to_string();
    let update_value = object.get("update").cloned().unwrap_or(Value::Null);
    let update: AcpUpdateBody =
        serde_json::from_value(update_value).map_err(|error| AcpCodecError::BadEnvelope {
            line: params.to_string(),
            message: format!("session/update body failed to decode: {error}"),
        })?;
    Ok(AcpSessionUpdate { session_id, update })
}

/// Errors raised when decoding the result of a JSON-RPC response.
/// The transport wraps these as [`AcpCodecError::BadEnvelope`] so
/// the existing error envelope keeps its shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpResultError {
    /// The response carried `error` instead of `result`. The
    /// transport raises this so the executor can record the
    /// provider's reason.
    Rpc(AcpRpcError),
    /// The response carried no `result` and no `error` — malformed
    /// per JSON-RPC 2.0.
    MissingResult,
}

impl std::fmt::Display for AcpResultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AcpResultError::Rpc(error) => {
                write!(f, "ACP rpc error {}: {}", error.code, error.message)
            }
            AcpResultError::MissingResult => {
                write!(f, "ACP response missing both result and error")
            }
        }
    }
}

impl std::error::Error for AcpResultError {}

/// Pull the `result` payload out of an [`AcpResponse`] as the type
/// `T`. Returns [`AcpResultError::Rpc`] if the server returned a
/// JSON-RPC error object, [`AcpResultError::MissingResult`] if the
/// response had neither field.
pub fn decode_result<T: serde::de::DeserializeOwned>(
    response: &AcpResponse,
) -> Result<T, AcpResultError> {
    if let Some(error) = &response.error {
        return Err(AcpResultError::Rpc(error.clone()));
    }
    match &response.result {
        Some(value) => serde_json::from_value(value.clone()).map_err(|error| {
            AcpResultError::Rpc(AcpRpcError {
                code: -32603,
                message: format!("result payload failed to decode: {error}"),
                data: None,
            })
        }),
        None => Err(AcpResultError::MissingResult),
    }
}
