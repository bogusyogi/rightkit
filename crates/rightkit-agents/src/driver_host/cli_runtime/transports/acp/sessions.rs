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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver_host::cli_runtime::adapter::{
        CliMcpServerSpec, CreateVendorSession, LoadVendorSession,
    };
    use crate::driver_host::cli_runtime::ids::CliProviderId;
    use crate::driver_host::cli_runtime::transports::acp::codec::AcpCodec;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    /// In-memory process I/O that captures the lines the transport
    /// writes and feeds pre-canned responses back to the session
    /// layer. Used by both the contract suite and the unit tests
    /// below.
    #[derive(Clone)]
    struct FakeProcess {
        pid: u32,
        generation: u64,
        written: Arc<Mutex<Vec<String>>>,
        scripted: Arc<Mutex<Vec<String>>>,
    }

    impl FakeProcess {
        fn new(pid: u32, scripted: Vec<String>) -> Self {
            Self {
                pid,
                generation: 1,
                written: Arc::new(Mutex::new(Vec::new())),
                scripted: Arc::new(Mutex::new(scripted)),
            }
        }

        fn written_lines(&self) -> Vec<String> {
            self.written.lock().unwrap().clone()
        }
    }

    impl crate::driver_host::cli_runtime::adapter::CliProcessIo for FakeProcess {
        fn write_line(
            &self,
            line: &str,
        ) -> Result<(), crate::driver_host::cli_runtime::adapter::CliRuntimeError> {
            self.written.lock().unwrap().push(line.to_string());
            Ok(())
        }
        fn read_event(
            &self,
        ) -> Result<Option<Value>, crate::driver_host::cli_runtime::adapter::CliRuntimeError>
        {
            let mut queue = self.scripted.lock().unwrap();
            if queue.is_empty() {
                Ok(None)
            } else {
                serde_json::from_str(&queue.remove(0))
                    .map(Some)
                    .map_err(|error| {
                        crate::driver_host::cli_runtime::adapter::CliRuntimeError::new(
                            "fake_acp_json",
                            error.to_string(),
                        )
                    })
            }
        }
        fn shutdown(
            &self,
        ) -> Result<(), crate::driver_host::cli_runtime::adapter::CliRuntimeError> {
            Ok(())
        }
        fn clone_box(&self) -> Box<dyn crate::driver_host::cli_runtime::adapter::CliProcessIo> {
            Box::new(self.clone())
        }
    }

    fn fake_process(scripted: Vec<&str>) -> StartedCliProcess {
        let fake = FakeProcess::new(4242, scripted.into_iter().map(str::to_string).collect());
        StartedCliProcess {
            pid: fake.pid,
            generation: fake.generation,
            inner: Box::new(fake),
        }
    }

    fn fake_process_with(fake: FakeProcess) -> StartedCliProcess {
        StartedCliProcess {
            pid: fake.pid,
            generation: fake.generation,
            inner: Box::new(fake),
        }
    }

    fn sample_mcp() -> CliMcpServerSpec {
        CliMcpServerSpec {
            name: "rightkit-agent".into(),
            command: PathBuf::from("/usr/local/bin/rightkit-agent"),
            args: vec!["mcp-server".into(), "--session-id".into(), "s-1".into()],
            env: vec![("RIGHTKIT_GATEWAY_TOKEN".into(), "lease-token".into())],
        }
    }

    /// The `mcpServers` payload must keep the `name`, `command`,
    /// `args`, and `env` entries so the server can spawn the right
    /// MCP binary.
    #[test]
    fn mcp_server_to_json_preserves_identity_fields() {
        let value = mcp_server_to_json(&sample_mcp());
        assert_eq!(value["name"], "rightkit-agent");
        assert_eq!(value["command"], "/usr/local/bin/rightkit-agent");
        assert_eq!(value["args"][1], "--session-id");
        assert_eq!(value["env"][0]["name"], "RIGHTKIT_GATEWAY_TOKEN");
        assert_eq!(value["env"][0]["value"], "lease-token");
    }

    /// The one-MCP rule must reject a launch that omits the server
    /// entirely.
    #[test]
    fn validate_single_mcp_server_rejects_zero_entries() {
        let err = validate_single_mcp_server(None).unwrap_err();
        assert!(matches!(err, AcpSessionError::Codec(_)));
    }

    /// `from_value` must accept both envelope shapes.
    #[test]
    fn session_new_result_accepts_both_wire_shapes() {
        let flat = json!({"sessionId": "abc", "modelId": "gpt-x"});
        let nested = json!({"session": {"sessionId": "def", "modelId": "gpt-y"}});
        assert_eq!(
            AcpSessionNewResult::from_value(&flat).unwrap().session_id,
            "abc"
        );
        assert_eq!(
            AcpSessionNewResult::from_value(&nested).unwrap().session_id,
            "def"
        );
    }

    /// `from_value` must reject a payload with no recognizable id
    /// so the transport never invents a session id.
    #[test]
    fn session_new_result_rejects_payload_without_id() {
        let payload = json!({"model": "x"});
        let err = AcpSessionNewResult::from_value(&payload).unwrap_err();
        assert!(matches!(err, AcpSessionError::MissingSessionId(_)));
    }

    /// A scripted `session/new` happy-path round-trips: the
    /// transport writes the right envelope, parses the server's
    /// response, and returns a handle carrying the vendor session
    /// id.
    #[test]
    fn create_session_round_trips_through_fake_process() {
        let scripted =
            vec![r#"{"jsonrpc":"2.0","id":1,"result":{"sessionId":"v-1","modelId":"gpt-x"}}"#];
        let fake = FakeProcess::new(7, scripted.into_iter().map(str::to_string).collect());
        let process = fake_process_with(fake.clone());

        let codec = AcpCodec::new();
        let handle = futures_lite_smoke::block_on(AcpSessions::create(
            &CliProviderId::new("grok"),
            &process,
            &CreateVendorSession {
                model_id: Some("gpt-x".into()),
                mcp_server: Some(sample_mcp()),
                system_policy: "policy-hash".into(),
            },
            &codec,
            "/work",
            "binding-1",
            "policy-hash",
            "session-1",
            "agent-1",
        ))
        .unwrap();

        assert_eq!(handle.provider_id, CliProviderId::new("grok"));
        assert_eq!(handle.transport, CliTransportKind::Acp);
        assert_eq!(handle.vendor_session_id, "v-1");
        assert_eq!(handle.model_id.as_deref(), Some("gpt-x"));

        let written = fake.written_lines();
        assert_eq!(written.len(), 1);
        let parsed: Value = serde_json::from_str(&written[0]).unwrap();
        assert_eq!(parsed["method"], "session/new");
        assert_eq!(parsed["params"]["mcpServers"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["params"]["_meta"]["rightkitBindingId"], "binding-1");
        assert_eq!(
            parsed["params"]["_meta"]["rightkitPolicyHash"],
            "policy-hash"
        );
    }

    /// `session/load` happy-path round-trips the vendor session id
    /// from the server's reply.
    #[test]
    fn load_session_round_trips_through_fake_process() {
        let scripted =
            vec![r#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"v-2","modelId":"gpt-x"}}"#];
        let fake = FakeProcess::new(9, scripted.into_iter().map(str::to_string).collect());
        let process = fake_process_with(fake.clone());

        let codec = AcpCodec::new();
        let handle = futures_lite_smoke::block_on(AcpSessions::load(
            &CliProviderId::new("cursor"),
            &process,
            &LoadVendorSession {
                vendor_session_id: "v-existing".into(),
                model_id: Some("gpt-x".into()),
                mcp_server: Some(sample_mcp()),
            },
            &codec,
            "/work",
            "binding-1",
        ))
        .unwrap();

        assert_eq!(handle.vendor_session_id, "v-2");
        let written = fake.written_lines();
        let parsed: Value = serde_json::from_str(&written[0]).unwrap();
        assert_eq!(parsed["method"], "session/load");
        assert_eq!(parsed["params"]["sessionId"], "v-existing");
        assert_eq!(parsed["params"]["mcpServers"].as_array().unwrap().len(), 1);
    }

    /// A `session/new` whose server reply lacks a session id must
    /// raise [`AcpSessionError::MissingSessionId`].
    #[test]
    fn create_session_rejects_response_without_id() {
        let scripted = vec![r#"{"jsonrpc":"2.0","id":1,"result":{"model":"gpt-x"}}"#];
        let process = fake_process(scripted);
        let codec = AcpCodec::new();
        let err = futures_lite_smoke::block_on(AcpSessions::create(
            &CliProviderId::new("grok"),
            &process,
            &CreateVendorSession {
                model_id: Some("gpt-x".into()),
                mcp_server: Some(sample_mcp()),
                system_policy: "policy".into(),
            },
            &codec,
            "/work",
            "binding-1",
            "policy-hash",
            "session-1",
            "agent-1",
        ))
        .unwrap_err();
        assert_eq!(err.code, "acp_session_missing_id");
    }

    /// A server-side JSON-RPC error on `session/new` must surface
    /// as a stable runtime error code.
    #[test]
    fn create_session_surfaces_rpc_error() {
        let scripted = vec![
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"method not found"}}"#,
        ];
        let process = fake_process(scripted);
        let codec = AcpCodec::new();
        let err = futures_lite_smoke::block_on(AcpSessions::create(
            &CliProviderId::new("grok"),
            &process,
            &CreateVendorSession {
                model_id: Some("gpt-x".into()),
                mcp_server: Some(sample_mcp()),
                system_policy: "policy".into(),
            },
            &codec,
            "/work",
            "binding-1",
            "policy-hash",
            "session-1",
            "agent-1",
        ))
        .unwrap_err();
        assert_eq!(err.code, "acp_session_rpc");
        assert!(err.message.contains("method not found"));
    }

    /// Notifications streamed between request and response must be
    /// drained and not break the round-trip.
    #[test]
    fn create_session_drains_interleaved_notifications() {
        let scripted = vec![
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"v-1","update":{"type":"agent_message_chunk","text":"hello"}}}"#,
            r#"{"jsonrpc":"2.0","id":1,"result":{"sessionId":"v-1"}}"#,
        ];
        let process = fake_process(scripted);
        let codec = AcpCodec::new();
        let handle = futures_lite_smoke::block_on(AcpSessions::create(
            &CliProviderId::new("grok"),
            &process,
            &CreateVendorSession {
                model_id: None,
                mcp_server: Some(sample_mcp()),
                system_policy: "policy".into(),
            },
            &codec,
            "/work",
            "binding-1",
            "policy-hash",
            "session-1",
            "agent-1",
        ))
        .unwrap();
        assert_eq!(handle.vendor_session_id, "v-1");
    }

    /// A launch that omits the MCP server must be refused at the
    /// session layer so the transport fails closed.
    #[test]
    fn create_session_refuses_zero_mcp_entries() {
        let process = fake_process(Vec::new());
        let codec = AcpCodec::new();
        let err = futures_lite_smoke::block_on(AcpSessions::create(
            &CliProviderId::new("grok"),
            &process,
            &CreateVendorSession {
                model_id: None,
                mcp_server: None,
                system_policy: "policy".into(),
            },
            &codec,
            "/work",
            "binding-1",
            "policy-hash",
            "session-1",
            "agent-1",
        ))
        .unwrap_err();
        assert_eq!(err.code, "acp_session_codec");
    }
}

// `futures_lite_smoke` is a tiny `block_on` shim so the session
// tests can drive the async API without pulling `tokio` into this
// crate's `[dev-dependencies]`. The implementation is intentionally
// minimal — it only needs to drive the in-process fake transport.
#[cfg(test)]
mod futures_lite_smoke {
    pub fn block_on<F: std::future::Future>(future: F) -> F::Output {
        // Build a tiny waker registry so the future can register
        // wake-ups. The session API never awaits anything other than
        // itself, so the registry never fires.
        use std::sync::{Arc, Condvar, Mutex};
        use std::task::{Context, Poll, Wake, Waker};

        struct Park {
            flag: Mutex<bool>,
            cv: Condvar,
        }
        impl Wake for Park {
            fn wake(self: Arc<Self>) {
                *self.flag.lock().unwrap() = true;
                self.cv.notify_all();
            }
            fn wake_by_ref(self: &Arc<Self>) {
                self.clone().wake();
            }
        }

        let park = Arc::new(Park {
            flag: Mutex::new(false),
            cv: Condvar::new(),
        });
        let waker = Waker::from(park.clone());
        let mut cx = Context::from_waker(&waker);
        let mut future = Box::pin(future);
        loop {
            match future.as_mut().poll(&mut cx) {
                Poll::Ready(value) => return value,
                Poll::Pending => {
                    let mut flag = park.flag.lock().unwrap();
                    while !*flag {
                        flag = park.cv.wait(flag).unwrap();
                    }
                    *flag = false;
                }
            }
        }
    }
}
