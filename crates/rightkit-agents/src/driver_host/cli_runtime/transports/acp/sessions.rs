//! ACP session lifecycle.
//!
//! PR 7 of the architecture plan. The transport drives two JSON-RPC
//! methods here:
//!
//! - [`AcpSessions::create`] issues `session/new` and asks the server
//!   to spin up a new vendor session bound to the *single* the host app
//!   MCP server entry. The resulting `VendorSessionHandle` carries
//!   the vendor's session id verbatim so the rest of the pipeline
//!   can route follow-up traffic.
//! - [`AcpSessions::load`] issues `session/load` and re-attaches to
//!   an existing vendor session. The transport passes the same MCP
//!   server entry so the resumed session inherits the harness
//!   bindings.
//!
//! Both methods share a one-MCP-server invariant: the transport
//! refuses to invoke them unless `AcpMcpServer::validate_single`
//! has confirmed exactly one entry. That invariant is enforced
//! upstream in `super::AcpTransport::start`.

use crate::driver_host::cli_runtime::adapter::{
    CliMcpServerSpec, CliRuntimeError, CreateVendorSession, LoadVendorSession, StartedCliProcess,
    VendorSessionHandle,
};
use crate::driver_host::cli_runtime::ids::{CliProviderId, CliTransportKind};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::codec::{
    decode_message, decode_result, AcpCodec, AcpCodecError, AcpInbound, AcpRequest, AcpResponse,
    AcpResultError,
};

/// Wire-level result of a `session/new` response. ACP servers are
/// free to nest the session id under different keys depending on
/// version; we accept either top-level `sessionId` or
/// `session.sessionId` to keep the transport robust.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpSessionNewResult {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
}

impl AcpSessionNewResult {
    /// Pull a session id out of a free-form JSON value. ACP versions
    /// in the wild disagree on where the id lives; this helper
    /// covers both `{"sessionId": ...}` and
    /// `{"session": {"sessionId": ...}}`.
    pub fn from_value(value: &Value) -> Result<Self, AcpSessionError> {
        if let Some(text) = value.get("sessionId").and_then(Value::as_str) {
            return Ok(Self {
                session_id: text.to_string(),
                model_id: value
                    .get("modelId")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
        }
        if let Some(text) = value
            .get("session")
            .and_then(|v| v.get("sessionId"))
            .and_then(Value::as_str)
        {
            return Ok(Self {
                session_id: text.to_string(),
                model_id: value
                    .get("session")
                    .and_then(|v| v.get("modelId"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
        }
        Err(AcpSessionError::MissingSessionId(value.clone()))
    }
}

/// Compatibility alias for the shared ACP session-id response shape.
/// Both `session/new` and `session/load` accept the same top-level or nested
/// vendor session identity forms.
pub type AcpSessionLoadResult = AcpSessionNewResult;

/// What can go wrong at the session-lifecycle layer. Mapped to
/// [`CliRuntimeError`] by [`AcpSessions::create`] /
/// [`AcpSessions::load`] with stable `code` strings so the
/// supervisor can branch on them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpSessionError {
    /// The transport issued a request and the wire codec rejected it
    /// before it ever left the harness.
    Codec(AcpCodecError),
    /// The server returned a JSON-RPC error object. Carries the
    /// JSON-RPC error verbatim.
    Rpc { code: i64, message: String },
    /// The server returned a success envelope with no recognizable
    /// session id. This means the wire shape drifted; the transport
    /// refuses to guess.
    MissingSessionId(Value),
    /// The provider advertised `session/load` but does not implement
    /// it. The transport reports this so the executor can fall back
    /// to a fresh session.
    Unsupported(String),
}

impl std::fmt::Display for AcpSessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AcpSessionError::Codec(error) => write!(f, "ACP codec error: {error}"),
            AcpSessionError::Rpc { code, message } => {
                write!(f, "ACP RPC error {code}: {message}")
            }
            AcpSessionError::MissingSessionId(value) => {
                write!(f, "ACP response missing sessionId: {value}")
            }
            AcpSessionError::Unsupported(message) => {
                write!(f, "ACP session/load unsupported: {message}")
            }
        }
    }
}

impl std::error::Error for AcpSessionError {}

impl From<AcpCodecError> for AcpSessionError {
    fn from(error: AcpCodecError) -> Self {
        AcpSessionError::Codec(error)
    }
}

impl From<AcpResultError> for AcpSessionError {
    fn from(error: AcpResultError) -> Self {
        match error {
            AcpResultError::Rpc(error) => Self::Rpc {
                code: error.code,
                message: error.message,
            },
            AcpResultError::MissingResult => Self::Codec(AcpCodecError::BadEnvelope {
                line: String::new(),
                message: "ACP response missing both result and error".into(),
            }),
        }
    }
}

impl AcpSessionError {
    /// Stable code for [`CliRuntimeError`]. The supervisor and the
    /// qualification probes match on these strings.
    pub fn code(&self) -> &'static str {
        match self {
            AcpSessionError::Codec(_) => "acp_session_codec",
            AcpSessionError::Rpc { .. } => "acp_session_rpc",
            AcpSessionError::MissingSessionId(_) => "acp_session_missing_id",
            AcpSessionError::Unsupported(_) => "acp_session_unsupported",
        }
    }
}

/// Serialize a [`CliMcpServerSpec`] into the JSON-RPC payload the
/// ACP server expects under `mcpServers`. We preserve the `name`
/// verbatim so the server can tag incoming tool calls with the
/// originating server identity — the permission broker relies on
/// this field.
pub fn mcp_server_to_json(spec: &CliMcpServerSpec) -> Value {
    let env: Vec<Value> = spec
        .env
        .iter()
        .map(|(name, value)| {
            json!({
                "name": name,
                "value": value,
            })
        })
        .collect();
    json!({
        "name": spec.name,
        "command": spec.command.to_string_lossy(),
        "args": spec.args,
        "env": env,
    })
}

/// Validate the one-MCP-server rule. Returns the single entry when
/// the launch spec carries exactly one; otherwise returns an
/// [`AcpSessionError::Codec`] mapped to a stable `code` so the
/// supervisor can refuse the launch.
pub fn validate_single_mcp_server(
    spec: Option<&CliMcpServerSpec>,
) -> Result<&CliMcpServerSpec, AcpSessionError> {
    match spec {
        Some(spec) => Ok(spec),
        None => Err(AcpSessionError::Codec(AcpCodecError::Encode {
            method: "session/new".into(),
            message:
                "launch spec carries no Agent MCP server; the one-MCP rule forbids zero entries"
                    .into(),
        })),
    }
}

/// The session-lifecycle half of the ACP transport. Constructed by
/// [`AcpSessions::new`] and stored on the [`super::AcpTransport`].
#[derive(Debug, Default)]
pub struct AcpSessions {
    _private: (),
}

impl AcpSessions {
    pub fn new() -> Self {
        Self { _private: () }
    }

    /// Issue `session/new`. The transport already validated the
    /// launch spec; this method only encodes the request, parses the
    /// server's reply, and assembles the
    /// [`VendorSessionHandle`] the executor stores on the binding.
    #[allow(clippy::too_many_arguments)]
    pub async fn create(
        provider_id: &CliProviderId,
        process: &StartedCliProcess,
        request: &CreateVendorSession,
        codec: &AcpCodec,
        cwd: &str,
        binding_id: &str,
        policy_hash: &str,
        session_id: &str,
        agent_id: &str,
    ) -> Result<VendorSessionHandle, CliRuntimeError> {
        let mcp_spec = validate_single_mcp_server(request.mcp_server.as_ref())
            .map_err(session_error_to_runtime)?;
        let mcp_value = mcp_server_to_json(mcp_spec);
        let line = codec
            .encode_session_new(
                cwd,
                &mcp_value,
                policy_hash,
                binding_id,
                session_id,
                agent_id,
            )
            .map_err(|error| session_error_to_runtime(error.into()))?;
        send_request_blocking(process, &line)?;
        let response = read_response_blocking(process).map_err(session_error_to_runtime)?;
        let result = decode_result::<Value>(&response)
            .map_err(|error| session_error_to_runtime(error.into()))?;
        let result = AcpSessionNewResult::from_value(&result).map_err(session_error_to_runtime)?;
        Ok(VendorSessionHandle {
            provider_id: provider_id.clone(),
            transport: CliTransportKind::Acp,
            vendor_session_id: result.session_id,
            model_id: result.model_id,
            process: clone_process_handle(process),
        })
    }

    /// Issue `session/load`. Some providers advertise resume but
    /// raise an error on load; we surface that as
    /// [`AcpSessionError::Unsupported`] so the executor can fall
    /// back to a fresh session.
    pub async fn load(
        provider_id: &CliProviderId,
        process: &StartedCliProcess,
        request: &LoadVendorSession,
        codec: &AcpCodec,
        cwd: &str,
        binding_id: &str,
    ) -> Result<VendorSessionHandle, CliRuntimeError> {
        let mcp_spec = validate_single_mcp_server(request.mcp_server.as_ref())
            .map_err(session_error_to_runtime)?;
        let mcp_value = mcp_server_to_json(mcp_spec);
        let line = codec
            .encode_session_load(&request.vendor_session_id, cwd, &mcp_value, binding_id)
            .map_err(|error| session_error_to_runtime(error.into()))?;
        send_request_blocking(process, &line)?;
        let response = read_response_blocking(process).map_err(session_error_to_runtime)?;
        if let Some(error) = &response.error {
            return Err(CliRuntimeError::new(
                "acp_session_load_unsupported",
                format!("session/load rejected: {}", error.message),
            ));
        }
        let result: Value =
            decode_result(&response).map_err(|error| session_error_to_runtime(error.into()))?;
        let result = AcpSessionNewResult::from_value(&result).map_err(session_error_to_runtime)?;
        Ok(VendorSessionHandle {
            provider_id: provider_id.clone(),
            transport: CliTransportKind::Acp,
            vendor_session_id: result.session_id,
            model_id: result.model_id,
            process: clone_process_handle(process),
        })
    }
}

/// `decode_result` returns a transport-internal `AcpResultError`,
/// but the session layer reports through [`AcpSessionError`]. The
/// helper unwraps it.
fn session_error_to_runtime(error: AcpSessionError) -> CliRuntimeError {
    CliRuntimeError::new(error.code(), error.to_string())
}

/// Write a single JSON-RPC frame to the child process stdin.
/// Returns [`CliRuntimeError`] with a stable `code` so the
/// supervisor can map I/O failures to recovery actions.
fn send_request_blocking(process: &StartedCliProcess, line: &str) -> Result<(), CliRuntimeError> {
    process.inner.write_line(line).map_err(|error| {
        CliRuntimeError::new(
            "acp_session_write",
            format!("failed to write to ACP child stdin: {}", error.message),
        )
    })
}

/// Read one JSON-RPC response from the child process stdout. The
/// transport only cares about the *first* frame after a request;
/// notifications are drained by the streaming layer in
/// [`super::client`].
fn read_response_blocking(process: &StartedCliProcess) -> Result<AcpResponse, AcpSessionError> {
    loop {
        let frame = process
            .inner
            .read_event()
            .map_err(|error| AcpSessionError::Rpc {
                code: -32000,
                message: format!("ACP child read failed: {}", error.message),
            })?;
        let Some(value) = frame else {
            return Err(AcpSessionError::Rpc {
                code: -32001,
                message: "ACP child closed stdin before responding".into(),
            });
        };
        let line = value.to_string();
        match decode_message(&line)? {
            AcpInbound::Response(response) => return Ok(response),
            // Notifications stream past while we wait for a response;
            // the higher-level streaming pipeline drains them. We
            // ignore them here so the session layer stays narrow.
            AcpInbound::Notification(_) => continue,
            AcpInbound::ProtocolError(error) => {
                return Err(AcpSessionError::Rpc {
                    code: error.code,
                    message: error.message,
                });
            }
        }
    }
}

/// Clone a `StartedCliProcess` so the session handle can outlive the
/// transport's borrow. Process handles are cheap: the heavy I/O
/// state is shared through `Arc` inside the implementation.
fn clone_process_handle(process: &StartedCliProcess) -> StartedCliProcess {
    StartedCliProcess {
        pid: process.pid,
        generation: process.generation,
        inner: process.inner.clone_box(),
    }
}

/// Convenience helper for tests and for adapters that want to assert
/// the request id format before issuing it. Wraps
/// [`AcpRequest::new`] and forwards errors.
pub fn build_request(
    codec: &AcpCodec,
    method: impl Into<String>,
    params: Value,
) -> Result<AcpRequest, AcpCodecError> {
    let line = codec.encode_request(method, params)?;
    serde_json::from_str(&line).map_err(|error| AcpCodecError::Encode {
        method: "round_trip".into(),
        message: format!("built request failed to parse: {error}"),
    })
}
