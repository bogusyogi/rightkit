//! Generic ACP transport.
//!
//! PR 1 of the architecture plan declared the marker; PR 7 wires
//! the full [`AcpTransport`] implementation behind
//! [`crate::driver_host::cli_runtime::adapter::CliTransport`]. The transport
//! composes:
//!
//! - [`AcpCodec`] for JSON-RPC 2.0 framing and per-process request
//!   ids;
//! - [`AcpSessions`] for `session/new` and `session/load`;
//! - [`AcpClient`] for `session/prompt`, the streaming loop, and
//!   `session/cancel`;
//! - [`AcpPermissionBroker`] for the deny-by-default tool
//!   broker described in §9.3 of the architecture plan.
//!
//! ## One MCP server rule
//!
//! The transport refuses to start a launch that does not carry
//! exactly one the host app MCP server entry. The rule lives in
//! [`AcpTransport::start`] and is repeated at
//! `AcpSessions::validate_single_mcp_server` so the
//! session-layer cannot be called with an empty list either.

use std::sync::Arc;

use async_trait::async_trait;

use crate::driver_host::cli_runtime::adapter::{
    CliLaunchSpec, CliMcpServerSpec, CliPrompt, CliRuntimeError, CliTransport, CreateVendorSession,
    LoadVendorSession, StartedCliProcess, VendorSessionHandle, VendorTurnHandle,
};
use crate::driver_host::cli_runtime::ids::CliTransportKind;

pub mod capabilities;
pub mod client;
pub mod codec;
pub mod permissions;
pub mod sessions;

pub use client::{AcpClient, AcpClientError, AcpPromptResult, AcpStreamEvent};
pub use codec::{
    decode_message, decode_result, parse_session_update, AcpCodec, AcpCodecError, AcpInbound,
    AcpNotification, AcpRequest, AcpRequestIdGenerator, AcpResponse, AcpResultError, AcpRpcError,
    AcpSessionUpdate, AcpUpdateBody, ACP_PROTOCOL_VERSION, JSONRPC_VERSION,
};
pub use permissions::{
    AcpPermissionBroker, AcpPermissionContext, AcpPermissionDecision, AcpToolCall,
};
pub use sessions::{
    build_request, mcp_server_to_json, validate_single_mcp_server, AcpSessionError,
    AcpSessionLoadResult, AcpSessionNewResult, AcpSessions,
};

/// What kind of transport this is. Re-exported for adapter code
/// that wants to assert the matching kind without importing
/// [`crate::driver_host::cli_runtime::ids::CliTransportKind`] directly.
pub const KIND: CliTransportKind = CliTransportKind::Acp;

/// The one MCP server name the harness uses. The transport compares
/// every `server_name` it sees on an incoming `ToolCall` against
/// this string; mismatches are denied before any executor is
/// reached.
pub const HOST_MCP_SERVER_NAME: &str = "rightkit-agent";

/// The full ACP transport. The transport owns a per-process
/// [`AcpCodec`] (because the request id generator must be
/// per-process) and the three collaborator modules that share it.
#[derive(Debug, Clone)]
pub struct AcpTransport {
    inner: Arc<AcpTransportInner>,
}

#[derive(Debug)]
struct AcpTransportInner {
    #[allow(dead_code)]
    sessions: AcpSessions,
    client: AcpClient,
    permissions: AcpPermissionBroker,
    codec: AcpCodec,
}

impl AcpTransport {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(AcpTransportInner {
                sessions: AcpSessions::new(),
                client: AcpClient::new(),
                permissions: AcpPermissionBroker::new(),
                codec: AcpCodec::new(),
            }),
        }
    }

    /// Borrow the codec. Tests use this to assert the request
    /// envelope shape; production code reaches it through the
    /// [`CliTransport`] impl.
    pub fn codec(&self) -> &AcpCodec {
        &self.inner.codec
    }

    /// Borrow the permission broker. Tests use this to assert
    /// grant/deny paths against a hand-built
    /// [`AcpPermissionContext`].
    pub fn permissions(&self) -> &AcpPermissionBroker {
        &self.inner.permissions
    }

    /// Validate the one-MCP-server rule on a launch spec. Returns
    /// the single entry on success; raises a typed error otherwise.
    pub fn enforce_one_mcp_rule(
        spec: &CliLaunchSpec,
    ) -> Result<&CliMcpServerSpec, CliRuntimeError> {
        match spec.mcp_server.as_ref() {
            Some(server) if server.name == HOST_MCP_SERVER_NAME => Ok(server),
            Some(other) => Err(CliRuntimeError::new(
                "acp_one_mcp_rule",
                format!(
                    "launch spec carries MCP server {:?}; \
                     the one-MCP rule requires exactly {:?}",
                    other.name, HOST_MCP_SERVER_NAME
                ),
            )),
            None => Err(CliRuntimeError::new(
                "acp_one_mcp_rule",
                "launch spec carries no MCP server entry; \
                 the one-MCP rule requires exactly one Host MCP server",
            )),
        }
    }
}

impl Default for AcpTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for AcpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("acp")
    }
}

#[async_trait]
impl CliTransport for AcpTransport {
    fn kind(&self) -> CliTransportKind {
        KIND
    }

    async fn start(&self, launch: &CliLaunchSpec) -> Result<StartedCliProcess, CliRuntimeError> {
        let _ = Self::enforce_one_mcp_rule(launch)?;
        crate::driver_host::cli_runtime::transports::process::StdioProcessIo::spawn_with_native_adapter(
            launch, 1,
        )
    }

    async fn create_session(
        &self,
        process: &StartedCliProcess,
        request: CreateVendorSession,
    ) -> Result<VendorSessionHandle, CliRuntimeError> {
        AcpSessions::create(
            &launch_provider_id(process),
            process,
            &request,
            &self.inner.codec,
            ".",
            "binding-stub",
            "policy-stub",
            "session-stub",
            "agent-stub",
        )
        .await
    }

    async fn load_session(
        &self,
        process: &StartedCliProcess,
        request: LoadVendorSession,
    ) -> Result<VendorSessionHandle, CliRuntimeError> {
        AcpSessions::load(
            &launch_provider_id(process),
            process,
            &request,
            &self.inner.codec,
            ".",
            "binding-stub",
        )
        .await
    }

    async fn prompt(
        &self,
        session: &VendorSessionHandle,
        prompt: CliPrompt,
    ) -> Result<VendorTurnHandle, CliRuntimeError> {
        let result = self
            .inner
            .client
            .prompt(session, &prompt, &self.inner.codec)
            .await?;
        Ok(result.turn)
    }

    async fn interrupt(&self, turn: &VendorTurnHandle) -> Result<(), CliRuntimeError> {
        self.inner
            .client
            .cancel(&turn.session, &self.inner.codec)
            .await
    }

    async fn close(&self, session: &VendorSessionHandle) -> Result<(), CliRuntimeError> {
        session.process.inner.shutdown().map_err(|error| {
            CliRuntimeError::new(
                "acp_transport_shutdown",
                format!("failed to shut down ACP child: {}", error.message),
            )
        })
    }
}

/// Best-effort recovery of the provider id off a [`StartedCliProcess`].
/// The supervisor PR owns the canonical mapping; for now we fall back
/// to a generic `unknown` provider so the session layer can be
/// exercised in tests.
fn launch_provider_id(
    _process: &StartedCliProcess,
) -> crate::driver_host::cli_runtime::ids::CliProviderId {
    // ACP's session protocol does not repeat the provider identity. The
    // executor owns its persisted binding; this fallback is only metadata on
    // a transient handle and never used for routing or authorization.
    crate::driver_host::cli_runtime::ids::CliProviderId::new("acp")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver_host::cli_runtime::ids::CliProviderId;
    use std::path::PathBuf;

    fn sample_spec(mcp: Option<CliMcpServerSpec>) -> CliLaunchSpec {
        CliLaunchSpec {
            provider_id: CliProviderId::new("grok"),
            transport: CliTransportKind::Acp,
            executable: PathBuf::from("/usr/local/bin/grok"),
            argv: vec!["--acp".into()],
            env_keep: Vec::new(),
            env_remove: Vec::new(),
            env: Vec::new(),
            cwd: PathBuf::from("/work"),
            mcp_server: mcp,
            profile_hash: "profile".into(),
            session_id: "session-1".into(),
        }
    }

    fn mcp_server(name: &str) -> CliMcpServerSpec {
        CliMcpServerSpec {
            name: name.into(),
            command: PathBuf::from("/usr/local/bin/rightkit-agent"),
            args: vec!["mcp-server".into()],
            env: Vec::new(),
        }
    }

    /// The one-MCP rule must accept a launch with exactly one
    /// the host app MCP server entry.
    #[test]
    fn one_mcp_rule_accepts_single_host_entry() {
        let spec = sample_spec(Some(mcp_server("rightkit-agent")));
        let server = AcpTransport::enforce_one_mcp_rule(&spec).unwrap();
        assert_eq!(server.name, "rightkit-agent");
    }

    /// The one-MCP rule must reject a launch whose MCP server name
    /// is not `host app`. The harness refuses to bind to a
    /// non-attested server.
    #[test]
    fn one_mcp_rule_rejects_non_host_server() {
        let spec = sample_spec(Some(mcp_server("third-party")));
        let err = AcpTransport::enforce_one_mcp_rule(&spec).unwrap_err();
        assert_eq!(err.code, "acp_one_mcp_rule");
        assert!(err.message.contains("third-party"));
    }

    /// The one-MCP rule must reject a launch that omits the MCP
    /// server entirely.
    #[test]
    fn one_mcp_rule_rejects_missing_server() {
        let spec = sample_spec(None);
        let err = AcpTransport::enforce_one_mcp_rule(&spec).unwrap_err();
        assert_eq!(err.code, "acp_one_mcp_rule");
        assert!(err.message.contains("no MCP server"));
    }

    /// The transport's display label must match its kind.
    #[test]
    fn transport_label_matches_id_module_kind() {
        assert_eq!(KIND, CliTransportKind::Acp);
        assert_eq!(AcpTransport::new().to_string(), "acp");
        assert_eq!(AcpTransport::new().kind(), CliTransportKind::Acp);
    }

    /// `start` validates the one-MCP rule before it can spawn a child.
    #[test]
    fn start_enforces_one_mcp_rule_before_refusing() {
        let transport = AcpTransport::new();
        let bad = sample_spec(Some(mcp_server("third-party")));
        let err = futures_lite_smoke::block_on(transport.start(&bad)).unwrap_err();
        assert_eq!(err.code, "acp_one_mcp_rule");
    }

    /// The transport owns the codec, sessions, client, and broker
    /// as collaborators; cloning the transport shares them through
    /// an `Arc` so multiple in-flight bindings do not duplicate the
    /// request id space accidentally.
    #[test]
    fn transport_clone_shares_codec_state() {
        let t = AcpTransport::new();
        let t2 = t.clone();
        assert_eq!(
            t.codec().id_generator().peek(),
            t2.codec().id_generator().peek()
        );
    }
}

#[cfg(test)]
mod futures_lite_smoke {
    pub fn block_on<F: std::future::Future>(future: F) -> F::Output {
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
