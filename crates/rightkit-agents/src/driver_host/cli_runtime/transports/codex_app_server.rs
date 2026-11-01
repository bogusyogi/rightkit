//! Codex app-server transport.
//!
//! PR 1 of the architecture plan declared the marker type; PR 6
//! implements [`crate::driver_host::cli_runtime::adapter::CliTransport`] for
//! `CodexAppServerTransport`. The transport exposes argv
//! introspection (`build_argv`) and event parsing (`parse_line`) so
//! the supervisor and the executor can reason about the Codex
//! wire shape without depending on the legacy modules.
//!
//! In PR 6 the adapter (see
//! [`crate::driver_host::cli_runtime::providers::codex::CodexAdapter`]) is
//! responsible for building the [`CliLaunchSpec::argv`] vector by
//! delegating to the legacy
//! [`crate::driver_host::CodexLaunchOptions::launch_spec`] entry point. The
//! transport itself is provider-agnostic: it forwards whatever
//! argv the adapter produced. This keeps the byte-equivalence
//! guarantee ("the launched argv is exactly what the legacy code
//! produced") local to the adapter, and keeps the transport
//! responsibility narrow (argv introspection + parse line +
//! supervision hooks that land with the supervisor PR).
//!
//! Process supervision (spawning the vendor binary, owning the
//! JSON-RPC stdio bridge, lifecycle cancellation) lands with the
//! supervisor PR; this PR exposes the argv and parsing surface so
//! the supervisor can be wired in a follow-up without re-touching
//! the adapter layer.

use async_trait::async_trait;

use crate::driver_host::cli_runtime::adapter::{
    CliLaunchSpec, CliPrompt, CliRuntimeError, CliTransport, CreateVendorSession,
    LoadVendorSession, StartedCliProcess, VendorSessionHandle, VendorTurnHandle,
};
use crate::driver_host::cli_runtime::ids::CliTransportKind;
use crate::driver_host::CodexMessage;

/// The Codex app-server transport. Owns argv introspection and
/// event parsing for one Codex launch; delegates argv construction
/// to the adapter (which delegates to the legacy
/// [`crate::driver_host::CodexLaunchOptions`]) so byte-equivalence is preserved.
pub struct CodexAppServerTransport {
    _private: (),
}

impl CodexAppServerTransport {
    pub fn new() -> Self {
        Self { _private: () }
    }

    /// Return the argv the supervisor will execute for this
    /// [`CliLaunchSpec`]. PR 6 passes through the adapter-produced
    /// argv verbatim; future PRs may augment it (for example to
    /// attach process supervision flags) but every augmentation
    /// must be reflected in the byte-equivalence snapshot in
    /// [`super::argv_snapshot`].
    pub fn build_argv(launch: &CliLaunchSpec) -> Result<Vec<String>, CliRuntimeError> {
        Ok(launch.argv.clone())
    }

    /// Parse one JSON-RPC line off the Codex stdio stream. Returns
    /// the tagged [`CodexMessage`] the legacy parser produces:
    /// responses carry a request id and an optional `result` /
    /// `error`; server requests carry a `method` and `params`;
    /// notifications carry a [`crate::driver_host::NormalizedEvent`] that the
    /// executor already knows how to fold.
    ///
    /// `Err` is reserved for malformed JSON, lines that lack both
    /// `id` and `method`, and notifications the legacy parser
    /// treats as required-but-unknown. The supervisor must fail
    /// closed in all three cases.
    pub fn parse_line(
        line: &str,
        source_offset: u64,
    ) -> Result<Option<CodexMessage>, CliRuntimeError> {
        crate::driver_host::parse_codex_jsonrpc_line(line, source_offset)
            .map_err(cli_runtime_error_from_parse_error)
    }
}

impl Default for CodexAppServerTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for CodexAppServerTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("codex_app_server")
    }
}

pub const KIND: CliTransportKind = CliTransportKind::CodexAppServer;

/// Translate a legacy [`crate::driver_host::DriverHostError`] (from the parsing
/// pipeline) into the open [`CliRuntimeError`] shape used by the
/// transport trait.
fn cli_runtime_error_from_parse_error(
    error: crate::driver_host::DriverHostError,
) -> CliRuntimeError {
    match error {
        crate::driver_host::DriverHostError::InvalidMessage { message, .. } => {
            CliRuntimeError::new("codex_parse_invalid", message)
        }
        crate::driver_host::DriverHostError::UnknownRequiredEvent { method, .. } => {
            CliRuntimeError::new("codex_parse_unknown_event", method)
        }
    }
}

#[async_trait]
impl CliTransport for CodexAppServerTransport {
    fn kind(&self) -> CliTransportKind {
        KIND
    }

    async fn start(&self, launch: &CliLaunchSpec) -> Result<StartedCliProcess, CliRuntimeError> {
        if launch.transport != KIND {
            return Err(CliRuntimeError::new(
                "codex_transport_kind",
                "launch transport is not Codex app server",
            ));
        }
        crate::driver_host::cli_runtime::transports::process::StdioProcessIo::spawn_with_native_adapter(
            launch, 1,
        )
    }

    async fn create_session(
        &self,
        process: &StartedCliProcess,
        request: CreateVendorSession,
    ) -> Result<VendorSessionHandle, CliRuntimeError> {
        let request_id = 1;
        process.inner.write_line(&serde_json::json!({"jsonrpc":"2.0","id":request_id,"method":"thread/start","params":{"model":request.model_id}}).to_string())?;
        let value = read_response(process, request_id)?;
        let session_id = value
            .pointer("/thread/id")
            .or_else(|| value.get("threadId"))
            .or_else(|| value.get("id"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                CliRuntimeError::new(
                    "codex_session_start",
                    "Codex thread/start response omitted thread id",
                )
            })?
            .to_owned();
        Ok(handle_for(process, session_id, request.model_id))
    }

    async fn load_session(
        &self,
        process: &StartedCliProcess,
        request: LoadVendorSession,
    ) -> Result<VendorSessionHandle, CliRuntimeError> {
        if request.vendor_session_id.is_empty() {
            return Err(CliRuntimeError::new(
                "codex_session_id",
                "cannot resume an empty Codex thread id",
            ));
        }
        Ok(handle_for(
            process,
            request.vendor_session_id,
            request.model_id,
        ))
    }

    async fn prompt(
        &self,
        session: &VendorSessionHandle,
        prompt: CliPrompt,
    ) -> Result<VendorTurnHandle, CliRuntimeError> {
        let request_id = 2;
        session.process.inner.write_line(&serde_json::json!({"jsonrpc":"2.0","id":request_id,"method":"turn/start","params":{"threadId":session.vendor_session_id,"input":[{"type":"text","text":prompt.text}]}}).to_string())?;
        let value = read_response(&session.process, request_id)?;
        let turn_id = value
            .get("turnId")
            .or_else(|| value.pointer("/turn/id"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        Ok(VendorTurnHandle {
            vendor_turn_id: turn_id,
            session: handle_for(
                &session.process,
                session.vendor_session_id.clone(),
                session.model_id.clone(),
            ),
        })
    }

    async fn interrupt(&self, turn: &VendorTurnHandle) -> Result<(), CliRuntimeError> {
        turn.session.process.inner.write_line(&serde_json::json!({"jsonrpc":"2.0","id":3,"method":"turn/interrupt","params":{"threadId":turn.session.vendor_session_id,"turnId":turn.vendor_turn_id}}).to_string())
    }

    async fn close(&self, session: &VendorSessionHandle) -> Result<(), CliRuntimeError> {
        session.process.inner.shutdown()
    }
}

fn handle_for(
    process: &StartedCliProcess,
    vendor_session_id: String,
    model_id: Option<String>,
) -> VendorSessionHandle {
    VendorSessionHandle {
        provider_id: crate::driver_host::cli_runtime::ids::CliProviderId::new("codex"),
        transport: KIND,
        vendor_session_id,
        model_id,
        process: StartedCliProcess {
            pid: process.pid,
            generation: process.generation,
            inner: process.inner.clone_box(),
        },
    }
}

fn read_response(
    process: &StartedCliProcess,
    request_id: u64,
) -> Result<serde_json::Value, CliRuntimeError> {
    loop {
        let Some(value) = process.inner.read_event()? else {
            return Err(CliRuntimeError::new(
                "codex_stream_ended",
                "Codex app server closed stdout before response",
            ));
        };
        if value.get("id").and_then(serde_json::Value::as_u64) != Some(request_id) {
            continue;
        }
        if let Some(error) = value.get("error") {
            return Err(CliRuntimeError::new("codex_rpc", error.to_string()));
        }
        return Ok(value
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver_host::cli_runtime::ids::CliProviderId;
    use crate::driver_host::contracts::NormalizedEventKind;
    use crate::driver_host::CodexMessage;

    /// The baseline argv is byte-equivalent to the snapshot.
    #[test]
    fn build_argv_matches_frozen_snapshot_for_baseline_launch() {
        let argv: Vec<String> = super::super::argv_snapshot::CODEX_BASELINE_ARGV
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let launch = CliLaunchSpec {
            provider_id: CliProviderId::new("codex"),
            transport: CliTransportKind::CodexAppServer,
            executable: "/usr/local/bin/codex".into(),
            argv,
            env_keep: Vec::new(),
            env_remove: Vec::new(),
            env: Vec::new(),
            cwd: "/work".into(),
            mcp_server: None,
            profile_hash: "profile-hash".into(),
            session_id: "550e8400-e29b-41d4-a716-446655440000".into(),
        };
        let produced = CodexAppServerTransport::build_argv(&launch).unwrap();
        let produced_refs: Vec<&str> = produced.iter().map(String::as_str).collect();
        assert_eq!(
            produced_refs,
            super::super::argv_snapshot::CODEX_BASELINE_ARGV,
            "Codex transport argv must match the frozen snapshot byte-for-byte"
        );
    }

    /// The model-override argv is byte-equivalent to the snapshot.
    #[test]
    fn build_argv_matches_frozen_snapshot_for_model_launch() {
        let argv: Vec<String> = super::super::argv_snapshot::CODEX_WITH_MODEL_ARGV
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let launch = CliLaunchSpec {
            provider_id: CliProviderId::new("codex"),
            transport: CliTransportKind::CodexAppServer,
            executable: "/usr/local/bin/codex".into(),
            argv,
            env_keep: Vec::new(),
            env_remove: Vec::new(),
            env: Vec::new(),
            cwd: "/work".into(),
            mcp_server: None,
            profile_hash: "profile-hash".into(),
            session_id: "550e8400-e29b-41d4-a716-446655440000".into(),
        };
        let produced = CodexAppServerTransport::build_argv(&launch).unwrap();
        let produced_refs: Vec<&str> = produced.iter().map(String::as_str).collect();
        assert_eq!(
            produced_refs,
            super::super::argv_snapshot::CODEX_WITH_MODEL_ARGV,
            "Codex model-override argv must match the frozen snapshot byte-for-byte"
        );
    }

    /// `item/agentMessage/delta` notifications round-trip through
    /// the transport parser into a `CodexMessage::Notification`
    /// carrying the same `NormalizedEvent` kind the legacy parser
    /// produced. PR 6 acceptance gate: existing event fixtures
    /// parse into the same `NormalizedEvent`.
    #[test]
    fn parse_line_matches_legacy_for_agent_message_delta() {
        let line = r#"{"method":"item/agentMessage/delta","params":{"delta":"hello"}}"#;
        let message = CodexAppServerTransport::parse_line(line, 9)
            .unwrap()
            .expect("notification must parse to Some");
        let CodexMessage::Notification(event) = message else {
            panic!("expected CodexMessage::Notification");
        };
        assert_eq!(
            event.kind,
            NormalizedEventKind::AssistantDelta {
                text: "hello".into()
            }
        );
    }

    /// The startup-status notification that marks gateway readiness
    /// must surface as a `GatewayReady` event through the transport
    /// parser.
    #[test]
    fn parse_line_emits_gateway_ready_for_startup_status() {
        let line = r#"{"method":"mcpServer/startupStatus/updated","params":{"server":"host","status":"connected"}}"#;
        let message = CodexAppServerTransport::parse_line(line, 1)
            .unwrap()
            .expect("startup status must parse to Some");
        let CodexMessage::Notification(event) = message else {
            panic!("expected CodexMessage::Notification");
        };
        assert_eq!(event.kind, NormalizedEventKind::GatewayReady);
    }

    /// Unknown notifications fail closed through the transport.
    #[test]
    fn parse_line_fails_closed_on_unknown_notification() {
        let error = CodexAppServerTransport::parse_line(
            r#"{"method":"item/newMutationKind","params":{}}"#,
            1,
        )
        .unwrap_err();
        assert_eq!(error.code, "codex_parse_unknown_event");
    }

    /// `serverRequest/resolved` is a known lifecycle notification
    /// the legacy parser drops (`Ok(None)`). The transport must
    /// preserve that.
    #[test]
    fn parse_line_returns_none_for_resolved_server_request() {
        let line = r#"{"method":"serverRequest/resolved","params":{"requestId":"7"}}"#;
        let message = CodexAppServerTransport::parse_line(line, 1).unwrap();
        assert!(
            message.is_none(),
            "resolved server request must drop to None"
        );
    }

    /// User-message items are known presentation events that the
    /// legacy parser drops. The transport must preserve that.
    #[test]
    fn parse_line_returns_none_for_user_message_items() {
        for method in ["item/started", "item/completed"] {
            let line = format!(
                r#"{{"method":"{method}","params":{{"item":{{"type":"userMessage","id":"user-1","content":[]}}}}}}"#
            );
            let message = CodexAppServerTransport::parse_line(&line, 1).unwrap();
            assert!(
                message.is_none(),
                "user-message item {method} must drop to None"
            );
        }
    }

    #[test]
    fn transport_label_matches_id_module_kind() {
        assert_eq!(KIND, CliTransportKind::CodexAppServer);
        assert_eq!(
            CodexAppServerTransport::new().to_string(),
            "codex_app_server"
        );
    }
}
