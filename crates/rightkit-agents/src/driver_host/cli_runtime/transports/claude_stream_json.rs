//! Claude stream-JSON transport.
//!
//! PR 1 of the architecture plan declared the marker type; PR 6
//! implements [`crate::driver_host::cli_runtime::adapter::CliTransport`] for
//! `ClaudeStreamJsonTransport`. The transport exposes argv
//! inspection (`build_argv`) and event parsing (`parse_line`) so
//! the supervisor and the executor can reason about the Claude
//! wire shape without depending on the legacy modules.
//!
//! In PR 6 the adapter (see
//! [`crate::driver_host::cli_runtime::providers::claude::ClaudeAdapter`]) is
//! responsible for building the [`CliLaunchSpec::argv`] vector by
//! delegating to the legacy
//! [`crate::driver_host::ClaudeLaunchOptions::launch_spec`] entry point. The
//! transport itself is provider-agnostic: it forwards whatever
//! argv the adapter produced. This keeps the byte-equivalence
//! guarantee ("the launched argv is exactly what the legacy code
//! produced") local to the adapter, and keeps the transport
//! responsibility narrow (argv introspection + parse line +
//! supervision hooks that land with the supervisor PR).
//!
//! Process supervision (spawning the vendor binary, owning stdin /
//! stdout, signal handling, lifecycle cancellation) lands with the
//! supervisor PR; this PR exposes the argv and parsing surface so
//! the supervisor can be wired in a follow-up without re-touching
//! the adapter layer.

use async_trait::async_trait;

use crate::driver_host::cli_runtime::adapter::{
    CliLaunchSpec, CliPrompt, CliRuntimeError, CliTransport, CreateVendorSession,
    LoadVendorSession, StartedCliProcess, VendorSessionHandle, VendorTurnHandle,
};
use crate::driver_host::cli_runtime::ids::CliTransportKind;

/// The Claude stream-JSON transport. Owns argv introspection and
/// event parsing for one Claude Code launch; delegates argv
/// construction to the adapter (which delegates to the legacy
/// [`crate::driver_host::ClaudeLaunchOptions`]) so byte-equivalence is preserved.
pub struct ClaudeStreamJsonTransport {
    _private: (),
}

impl ClaudeStreamJsonTransport {
    pub fn new() -> Self {
        Self { _private: () }
    }

    /// Return the argv the supervisor will execute for this
    /// [`CliLaunchSpec`]. PR 6 passes through the adapter-produced
    /// argv verbatim; future PRs may augment it (for example to
    /// attach process supervision flags) but every augmentation
    /// must be reflected in the byte-equivalence snapshot in
    /// `super::argv_snapshot`.
    pub fn build_argv(launch: &CliLaunchSpec) -> Result<Vec<String>, CliRuntimeError> {
        Ok(launch.argv.clone())
    }

    /// Parse one NDJSON line off the Claude stdout stream. Returns
    /// `Ok(None)` for events the legacy parser treats as
    /// presentation bookkeeping (heartbeats, control acks, etc.).
    /// Returns `Err` only for malformed JSON or events the legacy
    /// parser treats as required-but-unknown — the supervisor must
    /// fail closed in both cases.
    pub fn parse_line(
        line: &str,
        source_offset: u64,
    ) -> Result<Option<crate::driver_host::NormalizedEvent>, CliRuntimeError> {
        crate::driver_host::parse_claude_stream_line(line, source_offset)
            .map_err(cli_runtime_error_from_parse_error)
    }
}

impl Default for ClaudeStreamJsonTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ClaudeStreamJsonTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("claude_stream_json")
    }
}

/// What kind of transport this is. Re-exported for adapter code
/// that wants to assert the matching kind without importing
/// [`crate::driver_host::cli_runtime::ids::CliTransportKind`] directly.
pub const KIND: CliTransportKind = CliTransportKind::ClaudeStreamJson;

/// Translate a legacy [`crate::driver_host::DriverHostError`] (from the parsing
/// pipeline) into the open [`CliRuntimeError`] shape used by the
/// transport trait.
fn cli_runtime_error_from_parse_error(
    error: crate::driver_host::DriverHostError,
) -> CliRuntimeError {
    match error {
        crate::driver_host::DriverHostError::InvalidMessage { message, .. } => {
            CliRuntimeError::new("claude_parse_invalid", message)
        }
        crate::driver_host::DriverHostError::UnknownRequiredEvent { method, .. } => {
            CliRuntimeError::new("claude_parse_unknown_event", method)
        }
    }
}

#[async_trait]
impl CliTransport for ClaudeStreamJsonTransport {
    fn kind(&self) -> CliTransportKind {
        KIND
    }

    async fn start(&self, launch: &CliLaunchSpec) -> Result<StartedCliProcess, CliRuntimeError> {
        if launch.transport != KIND {
            return Err(CliRuntimeError::new(
                "claude_transport_kind",
                "launch transport is not Claude stream JSON",
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
        Ok(handle_for(
            process,
            format!("claude-process-{}", process.pid),
            request.model_id,
        ))
    }

    async fn load_session(
        &self,
        process: &StartedCliProcess,
        request: LoadVendorSession,
    ) -> Result<VendorSessionHandle, CliRuntimeError> {
        if request.vendor_session_id.is_empty() {
            return Err(CliRuntimeError::new(
                "claude_session_id",
                "cannot resume an empty Claude session id",
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
        if prompt.text.trim().is_empty() {
            return Err(CliRuntimeError::new(
                "claude_prompt_empty",
                "prompt text must not be empty",
            ));
        }
        let frame = crate::claude::user_turn_frame(&prompt.text);
        session.process.inner.write_line(frame.trim_end())?;
        Ok(VendorTurnHandle {
            vendor_turn_id: None,
            session: handle_for(
                &session.process,
                session.vendor_session_id.clone(),
                session.model_id.clone(),
            ),
        })
    }

    async fn interrupt(&self, turn: &VendorTurnHandle) -> Result<(), CliRuntimeError> {
        let frame = crate::claude::interrupt_frame(&crate::claude::next_request_id("interrupt"));
        turn.session.process.inner.write_line(frame.trim_end())
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
        provider_id: crate::driver_host::cli_runtime::ids::CliProviderId::new("claude"),
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
