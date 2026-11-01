//! Resumable NDJSON transport for CLIs that emit structured
//! headless events and support native session resume (Command Code
//! primarily, Cursor fallback when ACP is not qualified).
//!
//! PR 1 of the architecture plan declared the dialect trait and the
//! transport kind marker. PR 11 implements
//! [`crate::driver_host::cli_runtime::adapter::CliTransport`] for
//! `ResumableNdjsonTransport` and ships the Command Code dialect that
//! the Command Code adapter plugs in. The transport owns the argv
//! introspection (`build_argv`) and the NDJSON event parser
//! (`parse_line`) so the supervisor can reason about the wire shape
//! without depending on per-vendor code.
//!
//! Process supervision (spawning the vendor binary, owning stdin /
//! stdout, signal handling, lifecycle cancellation) lands with the
//! supervisor owns persistent binding metadata; this transport owns the
//! concrete stdio child lifecycle and fail-closed event framing.
//!
//! The Command Code dialect:
//!
//! - argv for the first turn: `commandcode --managed-home <home> --mcp-config <path> --disable-skills --disable-updates --hooks-config <hooks> --print-events --session-id <id>`;
//! - argv for subsequent turns: same with `--resume <vendor_session_id>` replacing `--session-id <id>`;
//! - NDJSON events: `{ "type": "<kind>", ... }` over stdout; the
//!   transport maps each well-known kind to a
//!   [`crate::driver_host::contracts::NormalizedEvent`];
//! - terminal session id: surfaced on `session.created` and
//!   `turn.started` events as the `session_id` field.

use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use crate::driver_host::cli_runtime::adapter::{
    CliLaunchSpec, CliPrompt, CliRuntimeError, CliTransport, CreateVendorSession,
    LoadVendorSession, StartedCliProcess, VendorSessionHandle, VendorTurnHandle,
};
use crate::driver_host::cli_runtime::ids::CliTransportKind;
use crate::driver_host::contracts::{DriverKind, NormalizedEvent, NormalizedEventKind};

/// One dialect of resumable NDJSON. Each vendor's CLI has its own
/// arg shape, line format, and session-id location. Adapters
/// implement this trait to plug their vendor into the shared
/// transport.
pub trait ResumableNdjsonDialect: Send + Sync {
    /// argv for the first turn of a session.
    fn first_turn_args(
        &self,
        request: &ResumableNdjsonRequest,
        launch: &CliLaunchSpec,
    ) -> Vec<String>;
    /// argv for every subsequent turn, including the vendor session
    /// identifier that re-attaches to the existing conversation.
    fn resumed_turn_args(
        &self,
        vendor_session_id: &str,
        request: &ResumableNdjsonRequest,
        launch: &CliLaunchSpec,
    ) -> Vec<String>;
    /// Parse one NDJSON line into a transport event. Returns
    /// `Ok(None)` when the line carries no event (e.g. a heartbeat).
    fn parse_line(
        &self,
        line: &str,
        source_offset: u64,
    ) -> Result<Option<NormalizedEvent>, CliRuntimeError>;
    /// Extract the terminal session id from a transport event when
    /// one is available. Used to detect a missing or mismatched
    /// vendor session id, which is a hard failure.
    fn terminal_session_id(&self, event: &serde_json::Value) -> Option<String>;
    /// Whether the dialect is qualified for launch. The default
    /// implementation returns `true`; dialects that gate on a
    /// runtime canary (for example the Cursor NDJSON fallback)
    /// override this to consult their gate. The transport's
    /// `build_argv` checks this method before invoking the argv
    /// builders so a closed-gate dialect never produces argv.
    fn is_qualified(&self) -> bool {
        true
    }
}

/// Per-request payload a dialect needs to build its argv. Carries
/// the unified launch request fields the open adapter contract
/// exchanges plus the session_id seed the supervisor decides is
/// authoritative. Lives next to the dialect trait so the dialect
/// does not have to depend on adapter internals.
#[derive(Debug, Clone)]
pub struct ResumableNdjsonRequest {
    pub session_id_seed: String,
    pub resume: bool,
    pub model_id: Option<String>,
    pub managed_home: std::path::PathBuf,
    pub mcp_config_path: std::path::PathBuf,
    pub hooks_config_path: std::path::PathBuf,
}

pub const KIND: CliTransportKind = CliTransportKind::ResumableNdjson;

/// The ResumableNDJSON transport. Owns argv introspection and event
/// parsing for one launch; delegates argv construction to the
/// dialect and the dialect to the per-vendor adapter. The transport
/// itself is provider-agnostic: it forwards whatever argv the
/// dialect produced and applies the dialect's parser to every line
/// the supervisor pulls off stdout.
pub struct ResumableNdjsonTransport {
    dialect: Box<dyn ResumableNdjsonDialect>,
}

impl ResumableNdjsonTransport {
    pub fn new(dialect: Box<dyn ResumableNdjsonDialect>) -> Self {
        Self { dialect }
    }

    /// Return the argv the supervisor will execute for this
    /// [`CliLaunchSpec`]. PR 11 passes through the adapter-supplied
    /// argv verbatim; future PRs may augment it (for example to
    /// attach process supervision flags) but every augmentation
    /// must be reflected in the dialect's argv constructor.
    ///
    /// The transport refuses to build argv when the dialect's
    /// qualification gate is closed. Dialects that gate on a
    /// runtime canary (the Cursor NDJSON fallback) override
    /// [`ResumableNdjsonDialect::is_qualified`] to consult their
    /// gate; until the canary flips the gate the transport
    /// surfaces `resumable_ndjson_dialect_not_qualified` so the
    /// supervisor can refuse the launch instead of accidentally
    /// driving a stub.
    pub fn build_argv(
        &self,
        launch: &CliLaunchSpec,
        request: &ResumableNdjsonRequest,
    ) -> Result<Vec<String>, CliRuntimeError> {
        if !self.dialect.is_qualified() {
            return Err(CliRuntimeError::new(
                "resumable_ndjson_dialect_not_qualified",
                "the dialect's qualification gate is closed; the fallback is disabled until the \
                 full-harness canary passes",
            ));
        }
        Ok(if request.resume {
            // For resume, argv[0] is still the vendor binary;
            // dialect is responsible for substituting `--resume`
            // for `--session-id`.
            self.dialect
                .resumed_turn_args(&request.session_id_seed, request, launch)
        } else {
            self.dialect.first_turn_args(request, launch)
        })
    }

    /// Parse one NDJSON line off the vendor stdout stream. Returns
    /// `Ok(None)` for events the dialect treats as presentation
    /// bookkeeping (heartbeats, control acks, etc.). Returns `Err`
    /// for malformed JSON or events the dialect treats as
    /// required-but-unknown — the supervisor must fail closed in
    /// both cases.
    pub fn parse_line(
        &self,
        line: &str,
        source_offset: u64,
    ) -> Result<Option<NormalizedEvent>, CliRuntimeError> {
        self.dialect.parse_line(line, source_offset)
    }
}

impl std::fmt::Display for ResumableNdjsonTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("resumable_ndjson")
    }
}

/// The Command Code dialect. Owns the vendor-specific argv shape
/// and the NDJSON event mapping.
///
/// Wire shape (per architecture plan §10):
/// - NDJSON line: `{ "type": "<kind>", "session_id": "...", ... }`;
/// - well-known kinds:
///   - `session.created` → `SessionStarted`;
///   - `turn.started` → `TurnStarted`;
///   - `assistant.delta` → `AssistantDelta`;
///   - `assistant.completed` → `AssistantCompleted`;
///   - `tool_call.started` → `ToolCallStarted`;
///   - `tool_call.result` → `ToolResult`;
///   - `approval.requested` → `ApprovalRequested`;
///   - `turn.completed` → `TurnCompleted`;
///   - `turn.interrupted` → `TurnInterrupted`;
///   - `turn.failed` → `TurnFailed`;
///   - `gateway.ready` → `GatewayReady`.
pub struct CommandCodeDialect;

impl CommandCodeDialect {
    pub const fn new() -> Self {
        Self
    }
}

impl Default for CommandCodeDialect {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandCodeDialect {
    /// Core argv shared by both first-turn and resumed-turn paths.
    /// `--print-events` keeps Command Code on the NDJSON wire, the
    /// hook config pins the deny rules, and the flags disable the
    /// two surfaces that would otherwise let the vendor escape the
    /// gateway (skills/updates).
    fn shared_argv(&self, request: &ResumableNdjsonRequest, launch: &CliLaunchSpec) -> Vec<String> {
        let mut argv: Vec<String> = Vec::with_capacity(16);
        argv.push(launch.executable.display().to_string());
        argv.push("--managed-home".into());
        argv.push(request.managed_home.display().to_string());
        argv.push("--mcp-config".into());
        argv.push(request.mcp_config_path.display().to_string());
        argv.push("--hooks-config".into());
        argv.push(request.hooks_config_path.display().to_string());
        argv.push("--disable-skills".into());
        argv.push("--disable-updates".into());
        argv.push("--print-events".into());
        if let Some(model) = &request.model_id {
            argv.push("--model".into());
            argv.push(model.clone());
        }
        argv
    }
}

impl ResumableNdjsonDialect for CommandCodeDialect {
    fn first_turn_args(
        &self,
        request: &ResumableNdjsonRequest,
        launch: &CliLaunchSpec,
    ) -> Vec<String> {
        let mut argv = self.shared_argv(request, launch);
        argv.push("--session-id".into());
        argv.push(request.session_id_seed.clone());
        argv
    }

    fn resumed_turn_args(
        &self,
        vendor_session_id: &str,
        request: &ResumableNdjsonRequest,
        launch: &CliLaunchSpec,
    ) -> Vec<String> {
        let mut argv = self.shared_argv(request, launch);
        argv.push("--resume".into());
        argv.push(vendor_session_id.to_string());
        argv
    }

    fn parse_line(
        &self,
        line: &str,
        source_offset: u64,
    ) -> Result<Option<NormalizedEvent>, CliRuntimeError> {
        let value: serde_json::Value = serde_json::from_str(line).map_err(|error| {
            CliRuntimeError::new(
                "commandcode_ndjson_invalid",
                format!("invalid NDJSON line at offset {source_offset}: {error}"),
            )
        })?;
        let Some(kind_str) = value.get("type").and_then(serde_json::Value::as_str) else {
            return Err(CliRuntimeError::new(
                "commandcode_ndjson_missing_type",
                format!("NDJSON line missing `type` field at offset {source_offset}"),
            ));
        };
        // Surface heartbeats as None; the rest are required events.
        let kind = match kind_str {
            "heartbeat" | "ping" => return Ok(None),
            "session.created" => NormalizedEventKind::SessionStarted {
                vendor_session_id: value
                    .get("session_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                init: None,
            },
            "turn.started" => NormalizedEventKind::TurnStarted {
                vendor_turn_id: value
                    .get("turn_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            },
            "assistant.delta" => NormalizedEventKind::AssistantDelta {
                text: value
                    .get("delta")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            },
            "assistant.completed" => NormalizedEventKind::AssistantCompleted {
                text: value
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            },
            "tool_call.started" => NormalizedEventKind::ToolCallStarted {
                call_id: value
                    .get("call_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                tool_name: value
                    .get("tool_name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                arguments: value
                    .get("arguments")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            },
            "tool_call.result" => NormalizedEventKind::ToolResult {
                call_id: value
                    .get("call_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                is_error: value
                    .get("is_error")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                content: value
                    .get("content")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            },
            "approval.requested" => NormalizedEventKind::ApprovalRequested {
                request_id: value
                    .get("request_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                action: value
                    .get("action")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                details: value
                    .get("details")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            },
            "turn.completed" => NormalizedEventKind::TurnCompleted,
            "turn.interrupted" => NormalizedEventKind::TurnInterrupted,
            "turn.failed" => NormalizedEventKind::TurnFailed {
                message: value
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("turn failed")
                    .to_string(),
            },
            "gateway.ready" => NormalizedEventKind::GatewayReady,
            // Required-but-unknown events fail closed. The
            // supervisor surfaces the error and refuses to keep
            // reading from this process.
            other => {
                return Err(CliRuntimeError::new(
                    "commandcode_ndjson_unknown_event",
                    format!("unknown Command Code event '{other}' at offset {source_offset}"),
                ));
            }
        };
        Ok(Some(NormalizedEvent {
            event_key: crate::driver_host::contracts::event_key(
                DriverKind::Claude,
                kind_str,
                source_offset,
            ),
            driver: DriverKind::Claude,
            source_offset,
            vendor_method: kind_str.to_string(),
            kind,
        }))
    }

    fn terminal_session_id(&self, event: &serde_json::Value) -> Option<String> {
        event
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    }
}

/// Runtime qualification gate for the Cursor NDJSON fallback. Per
/// architecture plan §14.5 the fallback is **disabled by default**
/// and only becomes launchable once the full-harness canary
/// (`set_cursor_ndjson_qualified`) flips the gate. The gate lives
/// behind a [`OnceLock`] so the supervisor, the qualification
/// runner, and tests can all observe the same state.
fn cursor_ndjson_gate() -> &'static Arc<AtomicBool> {
    static GATE: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    GATE.get_or_init(|| Arc::new(AtomicBool::new(false)))
}

/// Whether the Cursor NDJSON fallback is qualified for launch.
/// Always `false` until [`set_cursor_ndjson_qualified`] flips the
/// gate.
pub fn cursor_ndjson_qualified() -> bool {
    cursor_ndjson_gate().load(Ordering::Acquire)
}

/// Flip the runtime qualification gate for the Cursor NDJSON
/// fallback. The full-harness canary is the only legitimate
/// caller; the gate starts closed so a misconfigured harness
/// cannot silently enable the fallback.
pub fn set_cursor_ndjson_qualified(qualified: bool) {
    cursor_ndjson_gate().store(qualified, Ordering::Release);
}

/// The Cursor NDJSON fallback dialect. Implements §14.5 of the
/// architecture plan: `agent --print --output-format stream-json
/// --resume <chatId>`. The dialect is **default-off** —
/// [`ResumableNdjsonDialect::is_qualified`] returns `false` until
/// [`set_cursor_ndjson_qualified`] flips the gate.
///
/// Cursor's `--print` mode is dangerous (§14.2) because it
/// exposes all vendor tools including write and Bash. The dialect
/// therefore emits the structural-deny flags the Cursor CLI ships
/// for stripping native write, shell, browser, and cloud-handoff
/// routes; whether those flags actually exist on the host binary
/// is what the qualification canary determines. If the canary
/// fails, the gate stays closed and the transport refuses to
/// build argv — a launch never happens.
///
/// The dialect never emits `--api-key`, never references
/// `CURSOR_API_KEY`, and never surfaces a BYOK code path.
pub struct CursorNdjsonDialect;

impl CursorNdjsonDialect {
    pub const fn new() -> Self {
        Self
    }
}

impl Default for CursorNdjsonDialect {
    fn default() -> Self {
        Self::new()
    }
}

impl CursorNdjsonDialect {
    /// Core argv shared by both first-turn and resumed-turn paths.
    /// `--print --output-format stream-json` keeps the vendor on
    /// the NDJSON wire; the rest of the flags are the structural
    /// deny knobs the Cursor CLI ships. Each deny flag is named
    /// after the surface it disables so an audit can map argv to
    /// policy intent.
    fn shared_argv(&self, request: &ResumableNdjsonRequest, launch: &CliLaunchSpec) -> Vec<String> {
        let mut argv: Vec<String> = Vec::with_capacity(20);
        argv.push(launch.executable.display().to_string());
        argv.push("--print".into());
        argv.push("--output-format".into());
        argv.push("stream-json".into());
        argv.push("--managed-home".into());
        argv.push(request.managed_home.display().to_string());
        argv.push("--mcp-config".into());
        argv.push(request.mcp_config_path.display().to_string());
        argv.push("--hooks-config".into());
        argv.push(request.hooks_config_path.display().to_string());
        // Structural-deny flags. The Cursor CLI exposes these
        // (or the version-qualified equivalents) when the binary
        // honours the architecture plan §14.5 controls. If any
        // flag is rejected the launch fails; we do not silently
        // drop a flag because that would re-enable a route the
        // harness is supposed to deny.
        argv.push("--disable-native-write".into());
        argv.push("--disable-native-shell".into());
        argv.push("--disable-native-edit".into());
        argv.push("--disable-browser-tools".into());
        argv.push("--disable-cloud-handoff".into());
        argv.push("--disable-background-agents".into());
        argv.push("--disable-skills".into());
        argv.push("--disable-plugins".into());
        if let Some(model) = &request.model_id {
            argv.push("--model".into());
            argv.push(model.clone());
        }
        argv
    }
}

impl ResumableNdjsonDialect for CursorNdjsonDialect {
    fn first_turn_args(
        &self,
        request: &ResumableNdjsonRequest,
        launch: &CliLaunchSpec,
    ) -> Vec<String> {
        let mut argv = self.shared_argv(request, launch);
        argv.push("--session-id".into());
        argv.push(request.session_id_seed.clone());
        argv
    }

    fn resumed_turn_args(
        &self,
        vendor_session_id: &str,
        request: &ResumableNdjsonRequest,
        launch: &CliLaunchSpec,
    ) -> Vec<String> {
        let mut argv = self.shared_argv(request, launch);
        argv.push("--resume".into());
        argv.push(vendor_session_id.to_string());
        argv
    }

    fn parse_line(
        &self,
        line: &str,
        source_offset: u64,
    ) -> Result<Option<NormalizedEvent>, CliRuntimeError> {
        let value: serde_json::Value = serde_json::from_str(line).map_err(|error| {
            CliRuntimeError::new(
                "cursor_ndjson_invalid",
                format!("invalid NDJSON line at offset {source_offset}: {error}"),
            )
        })?;
        let Some(kind_str) = value.get("type").and_then(serde_json::Value::as_str) else {
            return Err(CliRuntimeError::new(
                "cursor_ndjson_missing_type",
                format!("NDJSON line missing `type` field at offset {source_offset}"),
            ));
        };
        // The Cursor NDJSON fallback mirrors the Command Code
        // wire shape (`{ "type": "<kind>", ... }`); the
        // translation into [`NormalizedEventKind`] is the same
        // table so downstream consumers do not have to know which
        // dialect produced the line.
        let kind = match kind_str {
            "heartbeat" | "ping" => return Ok(None),
            "session.created" => NormalizedEventKind::SessionStarted {
                vendor_session_id: value
                    .get("session_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                init: None,
            },
            "turn.started" => NormalizedEventKind::TurnStarted {
                vendor_turn_id: value
                    .get("turn_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            },
            "assistant.delta" => NormalizedEventKind::AssistantDelta {
                text: value
                    .get("delta")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            },
            "assistant.completed" => NormalizedEventKind::AssistantCompleted {
                text: value
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            },
            "tool_call.started" => NormalizedEventKind::ToolCallStarted {
                call_id: value
                    .get("call_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                tool_name: value
                    .get("tool_name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                arguments: value
                    .get("arguments")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            },
            "tool_call.result" => NormalizedEventKind::ToolResult {
                call_id: value
                    .get("call_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                is_error: value
                    .get("is_error")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                content: value
                    .get("content")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            },
            "approval.requested" => NormalizedEventKind::ApprovalRequested {
                request_id: value
                    .get("request_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                action: value
                    .get("action")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                details: value
                    .get("details")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            },
            "turn.completed" => NormalizedEventKind::TurnCompleted,
            "turn.interrupted" => NormalizedEventKind::TurnInterrupted,
            "turn.failed" => NormalizedEventKind::TurnFailed {
                message: value
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("turn failed")
                    .to_string(),
            },
            "gateway.ready" => NormalizedEventKind::GatewayReady,
            other => {
                return Err(CliRuntimeError::new(
                    "cursor_ndjson_unknown_event",
                    format!("unknown Cursor NDJSON event '{other}' at offset {source_offset}"),
                ));
            }
        };
        Ok(Some(NormalizedEvent {
            event_key: crate::driver_host::contracts::event_key(
                DriverKind::Codex,
                kind_str,
                source_offset,
            ),
            driver: DriverKind::Codex,
            source_offset,
            vendor_method: kind_str.to_string(),
            kind,
        }))
    }

    fn terminal_session_id(&self, event: &serde_json::Value) -> Option<String> {
        event
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    }

    fn is_qualified(&self) -> bool {
        cursor_ndjson_qualified()
    }
}

#[async_trait]
impl CliTransport for ResumableNdjsonTransport {
    fn kind(&self) -> CliTransportKind {
        KIND
    }

    async fn start(&self, launch: &CliLaunchSpec) -> Result<StartedCliProcess, CliRuntimeError> {
        if launch.transport != KIND {
            return Err(CliRuntimeError::new(
                "resumable_ndjson_transport_kind",
                "launch transport is not resumable NDJSON",
            ));
        }
        if !self.dialect.is_qualified() {
            return Err(CliRuntimeError::new(
                "resumable_ndjson_dialect_not_qualified",
                "provider NDJSON dialect is not harness-qualified",
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
            format!("ndjson-process-{}", process.pid),
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
                "resumable_ndjson_session_id",
                "cannot resume an empty vendor session id",
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
                "resumable_ndjson_prompt_empty",
                "prompt text must not be empty",
            ));
        }
        session
            .process
            .inner
            .write_line(&serde_json::json!({"type":"user","text":prompt.text}).to_string())?;
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
        turn.session
            .process
            .inner
            .write_line(r#"{"type":"interrupt"}"#)
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
        provider_id: crate::driver_host::cli_runtime::ids::CliProviderId::new("commandcode"),
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

#[cfg(test)]
mod cursor_tests {
    //! PR 10 acceptance: the Cursor NDJSON fallback is implemented
    //! but **disabled by default** until the full-harness canary
    //! flips the gate. Tests in this module exercise the gate,
    //! argv shape, and wire-level event mapping without ever
    //! accidentally launching the Cursor binary.

    use super::*;
    use crate::driver_host::cli_runtime::adapter::CliMcpServerSpec;
    use crate::driver_host::cli_runtime::ids::CliProviderId;
    use std::path::PathBuf;
    use std::sync::{Mutex, MutexGuard};

    static GATE_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Snapshot + restore the gate around each test so a test
    /// that flips the gate cannot leak state into the rest of the
    /// suite. The harness always sees the closed gate unless
    /// the canary explicitly opens it.
    struct GateGuard {
        previous: bool,
        _lock: MutexGuard<'static, ()>,
    }

    impl GateGuard {
        fn new() -> Self {
            let lock = GATE_TEST_LOCK
                .lock()
                .expect("cursor qualification test lock");
            let previous = cursor_ndjson_qualified();
            set_cursor_ndjson_qualified(false);
            Self {
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for GateGuard {
        fn drop(&mut self) {
            set_cursor_ndjson_qualified(self.previous);
        }
    }

    fn sample_launch_and_request(
        resume: bool,
        seed: &str,
    ) -> (CliLaunchSpec, ResumableNdjsonRequest) {
        let launch = CliLaunchSpec {
            provider_id: CliProviderId::new("cursor"),
            transport: CliTransportKind::ResumableNdjson,
            executable: PathBuf::from("/usr/local/bin/cursor-agent"),
            argv: Vec::new(),
            env_keep: Vec::new(),
            env_remove: Vec::new(),
            env: Vec::new(),
            cwd: PathBuf::from("/work"),
            mcp_server: Some(CliMcpServerSpec {
                name: "rightkit-agent".into(),
                command: PathBuf::from("/usr/local/bin/rightkit-agent-mcp"),
                args: Vec::new(),
                env: Vec::new(),
            }),
            profile_hash: "cursor-profile-hash".into(),
            session_id: seed.into(),
        };
        let request = ResumableNdjsonRequest {
            session_id_seed: seed.into(),
            resume,
            model_id: None,
            managed_home: PathBuf::from("/var/rightkit-agent/cli-profiles/cursor/default"),
            mcp_config_path: PathBuf::from(
                "/var/rightkit-agent/cli-profiles/cursor/default/mcp.json",
            ),
            hooks_config_path: PathBuf::from(
                "/var/rightkit-agent/cli-profiles/cursor/default/hooks.json",
            ),
        };
        (launch, request)
    }

    /// PR 10 acceptance: the Cursor NDJSON fallback is disabled by
    /// default. A misconfigured harness must never accidentally
    /// drive the fallback.
    #[test]
    fn cursor_ndjson_dialect_is_not_qualified_by_default() {
        let _guard = GateGuard::new();
        assert!(
            !cursor_ndjson_qualified(),
            "Cursor NDJSON fallback must be closed by default"
        );
        let dialect = CursorNdjsonDialect::new();
        assert!(
            !dialect.is_qualified(),
            "CursorNdjsonDialect must report closed gate by default"
        );
    }

    /// The dialect's qualification gate can be flipped at runtime
    /// by the full-harness canary. The flip is observable through
    /// both the global helper and the dialect's `is_qualified`
    /// method, so the supervisor and the qualification runner see
    /// the same state.
    #[test]
    fn cursor_ndjson_dialect_can_be_qualified_at_runtime() {
        let _guard = GateGuard::new();
        let dialect = CursorNdjsonDialect::new();
        set_cursor_ndjson_qualified(true);
        assert!(
            cursor_ndjson_qualified(),
            "set_cursor_ndjson_qualified(true) must flip the gate"
        );
        assert!(
            dialect.is_qualified(),
            "CursorNdjsonDialect must reflect the opened gate"
        );
        set_cursor_ndjson_qualified(false);
        assert!(
            !dialect.is_qualified(),
            "CursorNdjsonDialect must reflect the closed gate"
        );
    }

    /// The transport refuses to build argv while the Cursor NDJSON
    /// gate is closed. The error code is stable so the supervisor
    /// can render a repair message instead of silently dropping
    /// the launch request.
    #[test]
    fn transport_refuses_build_argv_when_cursor_ndjson_gate_is_closed() {
        let _guard = GateGuard::new();
        let transport = ResumableNdjsonTransport::new(Box::new(CursorNdjsonDialect::new()));
        let (launch, request) = sample_launch_and_request(false, "seed-1");
        let error = transport
            .build_argv(&launch, &request)
            .expect_err("build_argv must refuse while gate is closed");
        assert_eq!(error.code, "resumable_ndjson_dialect_not_qualified");
    }

    /// Once the gate is opened the dialect builds argv. The argv
    /// shape is `agent --print --output-format stream-json
    /// --managed-home <home> --mcp-config <path> --hooks-config
    /// <path> --disable-native-write --disable-native-shell ... --
    /// session-id <id>` for the first turn. The structural-deny
    /// flags carry the §14.6 surface-deny list verbatim.
    #[test]
    fn cursor_ndjson_first_turn_argv_carries_session_id_and_deny_flags() {
        let _guard = GateGuard::new();
        set_cursor_ndjson_qualified(true);
        let transport = ResumableNdjsonTransport::new(Box::new(CursorNdjsonDialect::new()));
        let (launch, request) = sample_launch_and_request(false, "cursor-chat-1");
        let argv = transport
            .build_argv(&launch, &request)
            .expect("build_argv must succeed when qualified");
        assert!(
            argv.windows(2)
                .any(|w| w == ["--session-id", "cursor-chat-1"]),
            "first-turn argv must carry `--session-id <chat_id>`; argv = {argv:?}"
        );
        for required in [
            "--print",
            "--output-format",
            "stream-json",
            "--managed-home",
            "--mcp-config",
            "--hooks-config",
            "--disable-native-write",
            "--disable-native-shell",
            "--disable-browser-tools",
            "--disable-cloud-handoff",
            "--disable-background-agents",
        ] {
            assert!(
                argv.contains(&required.to_string()),
                "argv must carry {required}; argv = {argv:?}"
            );
        }
        // The dialect must never emit the API-key path the plan
        // forbids.
        assert!(
            !argv
                .iter()
                .any(|a| a == "--api-key" || a.starts_with("--api-key=")),
            "Cursor NDJSON fallback must never emit --api-key; argv = {argv:?}"
        );
        assert!(
            !argv.windows(2).any(|w| w[0] == "--resume"),
            "first-turn argv must not carry --resume; argv = {argv:?}"
        );
    }

    /// The resumed-turn argv substitutes `--resume <vendor_session_id>`
    /// for `--session-id <id>` and keeps every structural-deny
    /// flag in place. Resume is the documented "chat/session ID"
    /// re-attachment from §14.7.
    #[test]
    fn cursor_ndjson_resumed_turn_argv_uses_resume_flag_with_chat_id() {
        let _guard = GateGuard::new();
        set_cursor_ndjson_qualified(true);
        let transport = ResumableNdjsonTransport::new(Box::new(CursorNdjsonDialect::new()));
        let (launch, mut request) = sample_launch_and_request(true, "seed-1");
        request.session_id_seed = "cursor-chat-abc".into();
        let argv = transport
            .build_argv(&launch, &request)
            .expect("build_argv must succeed when qualified");
        assert!(
            argv.windows(2)
                .any(|w| w == ["--resume", "cursor-chat-abc"]),
            "resume argv must carry `--resume <chat_id>`; argv = {argv:?}"
        );
        assert!(
            !argv.windows(2).any(|w| w[0] == "--session-id"),
            "resume argv must not carry --session-id; argv = {argv:?}"
        );
        // The structural-deny flags survive the resume path; the
        // gate is per-dialect, not per-launch.
        for required in [
            "--disable-native-write",
            "--disable-native-shell",
            "--disable-cloud-handoff",
            "--disable-background-agents",
        ] {
            assert!(
                argv.contains(&required.to_string()),
                "resume argv must carry {required}; argv = {argv:?}"
            );
        }
    }

    /// The Cursor NDJSON dialect must never surface a credential
    /// byte or a BYOK code path. The argv it builds is the only
    /// shape the harness forwards; if a `--api-key`-shaped token
    /// slips in, the contract is broken.
    #[test]
    fn cursor_ndjson_argv_never_carries_api_key_or_keyword() {
        let _guard = GateGuard::new();
        set_cursor_ndjson_qualified(true);
        let dialect = CursorNdjsonDialect::new();
        let (launch, request) = sample_launch_and_request(false, "cursor-chat-1");
        for argv in [
            dialect.first_turn_args(&request, &launch),
            dialect.resumed_turn_args("cursor-chat-1", &request, &launch),
        ] {
            for token in &argv {
                assert_ne!(
                    token, "--api-key",
                    "Cursor NDJSON dialect must never emit --api-key"
                );
                assert!(
                    !token.starts_with("--api-key="),
                    "Cursor NDJSON dialect must never emit --api-key=*"
                );
                assert!(
                    !token.to_ascii_lowercase().contains("cursor_api_key"),
                    "Cursor NDJSON dialect must never reference CURSOR_API_KEY; got {token:?}"
                );
            }
        }
    }

    /// The Cursor NDJSON event parser maps the canonical
    /// `assistant.delta` event into `NormalizedEvent::AssistantDelta`,
    /// matching the Command Code wire shape so downstream consumers
    /// see one event table across both NDJSON dialects.
    #[test]
    fn cursor_ndjson_parses_assistant_delta_into_normalized_event() {
        let _guard = GateGuard::new();
        let dialect = CursorNdjsonDialect::new();
        let line = r#"{"type":"assistant.delta","session_id":"sess-1","delta":"hello"}"#;
        let event = dialect
            .parse_line(line, 7)
            .expect("parse_line must succeed")
            .expect("assistant.delta must parse to Some");
        assert_eq!(
            event.kind,
            NormalizedEventKind::AssistantDelta {
                text: "hello".into()
            }
        );
        assert_eq!(event.vendor_method, "assistant.delta");
    }

    /// Heartbeat / ping events drop to `None` so the supervisor
    /// does not surface them as NormalizedEvents.
    #[test]
    fn cursor_ndjson_returns_none_for_heartbeat() {
        let _guard = GateGuard::new();
        let dialect = CursorNdjsonDialect::new();
        let event = dialect
            .parse_line(r#"{"type":"heartbeat","ts":1}"#, 1)
            .expect("heartbeat must parse cleanly");
        assert!(event.is_none(), "heartbeat must drop to None");
    }

    /// Unknown required events fail closed with a typed error so
    /// the supervisor can refuse the launch instead of silently
    /// dropping the unknown shape.
    #[test]
    fn cursor_ndjson_fails_closed_on_unknown_required_event() {
        let _guard = GateGuard::new();
        let dialect = CursorNdjsonDialect::new();
        let error = dialect
            .parse_line(r#"{"type":"cursor_subagent_spawn"}"#, 4)
            .unwrap_err();
        assert_eq!(error.code, "cursor_ndjson_unknown_event");
    }

    /// Malformed JSON fails closed with a distinct error code.
    #[test]
    fn cursor_ndjson_fails_closed_on_malformed_line() {
        let _guard = GateGuard::new();
        let dialect = CursorNdjsonDialect::new();
        let error = dialect.parse_line("not json", 1).unwrap_err();
        assert_eq!(error.code, "cursor_ndjson_invalid");
    }

    /// `session.created` is the wire event the dialect reads the
    /// terminal session id from; `terminal_session_id` must agree
    /// with what `parse_line` surfaces as `vendor_session_id`.
    #[test]
    fn cursor_ndjson_terminal_session_id_matches_session_created() {
        let _guard = GateGuard::new();
        let dialect = CursorNdjsonDialect::new();
        let raw: serde_json::Value =
            serde_json::from_str(r#"{"type":"session.created","session_id":"vendor-cursor-1"}"#)
                .unwrap();
        assert_eq!(
            dialect.terminal_session_id(&raw),
            Some("vendor-cursor-1".into())
        );
    }

    /// The default `is_qualified` on the Command Code dialect
    /// stays `true` so PR 11's existing tests and wire shape do
    /// not break when PR 10 introduces the gate.
    #[test]
    fn commandcode_dialect_qualification_default_remains_open() {
        let _guard = GateGuard::new();
        set_cursor_ndjson_qualified(false);
        let dialect = CommandCodeDialect::new();
        assert!(
            dialect.is_qualified(),
            "Command Code dialect must stay qualified by default"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver_host::cli_runtime::adapter::CliMcpServerSpec;
    use crate::driver_host::cli_runtime::ids::CliProviderId;
    use std::path::PathBuf;

    fn sample_request(
        managed_home: &str,
        mcp: &str,
        hooks: &str,
        resume: bool,
        session_id: &str,
    ) -> (CliLaunchSpec, ResumableNdjsonRequest) {
        let launch = CliLaunchSpec {
            provider_id: CliProviderId::new("commandcode"),
            transport: CliTransportKind::ResumableNdjson,
            executable: PathBuf::from("/usr/local/bin/commandcode"),
            argv: Vec::new(),
            env_keep: Vec::new(),
            env_remove: Vec::new(),
            env: Vec::new(),
            cwd: PathBuf::from("/work"),
            mcp_server: Some(CliMcpServerSpec {
                name: "rightkit-agent".into(),
                command: PathBuf::from("/usr/local/bin/rightkit-agent-mcp"),
                args: Vec::new(),
                env: Vec::new(),
            }),
            profile_hash: "profile-hash".into(),
            session_id: session_id.into(),
        };
        let request = ResumableNdjsonRequest {
            session_id_seed: session_id.into(),
            resume,
            model_id: None,
            managed_home: PathBuf::from(managed_home),
            mcp_config_path: PathBuf::from(mcp),
            hooks_config_path: PathBuf::from(hooks),
        };
        (launch, request)
    }

    /// The transport kind must be reported as `ResumableNdjson`.
    #[test]
    fn kind_is_resumable_ndjson() {
        assert_eq!(KIND, CliTransportKind::ResumableNdjson);
    }

    /// First-turn argv uses `--session-id <seed>`. The plan section
    /// 10 specifies the structured session id, so it must appear
    /// verbatim in argv.
    #[test]
    fn first_turn_argv_uses_session_id_flag() {
        let transport = ResumableNdjsonTransport::new(Box::new(CommandCodeDialect::new()));
        let (launch, request) = sample_request(
            "/var/rightkit-agent/cli/commandcode/default",
            "/var/rightkit-agent/cli/commandcode/default/mcp.json",
            "/var/rightkit-agent/cli/commandcode/default/hooks.json",
            false,
            "550e8400-e29b-41d4-a716-446655440000",
        );
        let argv = transport
            .build_argv(&launch, &request)
            .expect("build_argv must succeed");
        assert!(
            argv.windows(2)
                .any(|w| w == ["--session-id", "550e8400-e29b-41d4-a716-446655440000"]),
            "first-turn argv must carry `--session-id <seed>` pair"
        );
        // `--resume` must NOT appear on a fresh session.
        assert!(
            !argv.iter().any(|a| a == "--resume"),
            "fresh session argv must not carry --resume"
        );
    }

    /// Resume argv uses `--resume <vendor_session_id>` instead of
    /// `--session-id`. The vendor session id is the one the prior
    /// session.created event surfaced, NOT the open session_id_seed.
    #[test]
    fn resumed_turn_argv_uses_resume_flag_with_vendor_id() {
        let transport = ResumableNdjsonTransport::new(Box::new(CommandCodeDialect::new()));
        let (launch, mut request) = sample_request(
            "/var/rightkit-agent/cli/commandcode/default",
            "/var/rightkit-agent/cli/commandcode/default/mcp.json",
            "/var/rightkit-agent/cli/commandcode/default/hooks.json",
            true,
            "550e8400-e29b-41d4-a716-446655440000",
        );
        request.session_id_seed = "vendor-session-abc".into();
        let argv = transport
            .build_argv(&launch, &request)
            .expect("build_argv must succeed");
        assert!(
            argv.windows(2)
                .any(|w| w == ["--resume", "vendor-session-abc"]),
            "resume argv must carry `--resume <vendor_session_id>` pair"
        );
        assert!(
            !argv.windows(2).any(|w| w[0] == "--session-id"),
            "resume argv must not carry --session-id"
        );
    }

    /// `--managed-home` and `--mcp-config` must appear on every
    /// argv the dialect emits — the harness and the host app MCP wiring
    /// depend on both flags.
    #[test]
    fn argv_carries_managed_home_and_mcp_config() {
        let transport = ResumableNdjsonTransport::new(Box::new(CommandCodeDialect::new()));
        let (launch, request) = sample_request(
            "/var/rightkit-agent/cli/commandcode/default",
            "/var/rightkit-agent/cli/commandcode/default/mcp.json",
            "/var/rightkit-agent/cli/commandcode/default/hooks.json",
            false,
            "550e8400-e29b-41d4-a716-446655440000",
        );
        let argv = transport
            .build_argv(&launch, &request)
            .expect("build_argv must succeed");
        assert!(
            argv.windows(2).any(|w| w[0] == "--managed-home"
                && w[1] == "/var/rightkit-agent/cli/commandcode/default"),
            "argv must carry --managed-home <managed_home>"
        );
        assert!(
            argv.windows(2).any(|w| w[0] == "--mcp-config"
                && w[1] == "/var/rightkit-agent/cli/commandcode/default/mcp.json"),
            "argv must carry --mcp-config <path>"
        );
    }

    /// Skills and updates are explicitly disabled by flag. Drift
    /// here is a security-relevant diff because the plan keeps both
    /// surfaces off until activation acceptance passes.
    #[test]
    fn argv_disables_skills_and_updates() {
        let transport = ResumableNdjsonTransport::new(Box::new(CommandCodeDialect::new()));
        let (launch, request) = sample_request(
            "/var/rightkit-agent/cli/commandcode/default",
            "/var/rightkit-agent/cli/commandcode/default/mcp.json",
            "/var/rightkit-agent/cli/commandcode/default/hooks.json",
            false,
            "550e8400-e29b-41d4-a716-446655440000",
        );
        let argv = transport
            .build_argv(&launch, &request)
            .expect("build_argv must succeed");
        assert!(
            argv.iter().any(|a| a == "--disable-skills"),
            "argv must disable native skills"
        );
        assert!(
            argv.iter().any(|a| a == "--disable-updates"),
            "argv must disable native updates"
        );
    }

    /// NDJSON transport parses the canonical `assistant.delta`
    /// event into a `NormalizedEvent::AssistantDelta`.
    #[test]
    fn ndjson_parses_assistant_delta_into_normalized_event() {
        let transport = ResumableNdjsonTransport::new(Box::new(CommandCodeDialect::new()));
        let line = r#"{"type":"assistant.delta","session_id":"sess-1","delta":"hello"}"#;
        let event = transport
            .parse_line(line, 7)
            .unwrap()
            .expect("assistant.delta must parse to Some");
        assert_eq!(
            event.kind,
            NormalizedEventKind::AssistantDelta {
                text: "hello".into()
            }
        );
        assert_eq!(event.vendor_method, "assistant.delta");
    }

    /// NDJSON transport parses `session.created` and exposes the
    /// vendor session id. The supervisor uses this to detect a
    /// missing or mismatched vendor session id, which is a hard
    /// failure.
    #[test]
    fn ndjson_parses_session_created_with_vendor_session_id() {
        let transport = ResumableNdjsonTransport::new(Box::new(CommandCodeDialect::new()));
        let line = r#"{"type":"session.created","session_id":"vendor-sess-1"}"#;
        let event = transport
            .parse_line(line, 1)
            .unwrap()
            .expect("session.created must parse to Some");
        match event.kind {
            NormalizedEventKind::SessionStarted {
                vendor_session_id, ..
            } => {
                assert_eq!(vendor_session_id, "vendor-sess-1");
            }
            other => panic!("expected SessionStarted, got {other:?}"),
        }
        let raw: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(
            CommandCodeDialect.terminal_session_id(&raw),
            Some("vendor-sess-1".into())
        );
    }

    /// NDJSON transport parses `tool_call.started` and `tool_call.result`.
    #[test]
    fn ndjson_parses_tool_call_started_and_result() {
        let transport = ResumableNdjsonTransport::new(Box::new(CommandCodeDialect::new()));
        let line = r#"{"type":"tool_call.started","call_id":"c1","tool_name":"fs.read","arguments":{"path":"/etc/hosts"}}"#;
        let event = transport
            .parse_line(line, 2)
            .unwrap()
            .expect("tool_call.started must parse to Some");
        match event.kind {
            NormalizedEventKind::ToolCallStarted {
                call_id, tool_name, ..
            } => {
                assert_eq!(call_id, "c1");
                assert_eq!(tool_name, "fs.read");
            }
            other => panic!("expected ToolCallStarted, got {other:?}"),
        }
        let line =
            r#"{"type":"tool_call.result","call_id":"c1","is_error":false,"content":{"ok":true}}"#;
        let event = transport
            .parse_line(line, 3)
            .unwrap()
            .expect("tool_call.result must parse to Some");
        match event.kind {
            NormalizedEventKind::ToolResult {
                call_id, is_error, ..
            } => {
                assert_eq!(call_id, "c1");
                assert!(!is_error);
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    /// NDJSON transport parses `turn.completed`, `turn.interrupted`,
    /// `turn.failed`, and `gateway.ready` into the canonical kinds.
    #[test]
    fn ndjson_parses_turn_lifecycle_events() {
        let transport = ResumableNdjsonTransport::new(Box::new(CommandCodeDialect::new()));
        for (line, expected_kind) in [
            (r#"{"type":"turn.completed"}"#, "TurnCompleted"),
            (r#"{"type":"turn.interrupted"}"#, "TurnInterrupted"),
            (
                r#"{"type":"turn.failed","message":"policy denied"}"#,
                "TurnFailed",
            ),
            (r#"{"type":"gateway.ready"}"#, "GatewayReady"),
        ] {
            let event = transport
                .parse_line(line, 1)
                .unwrap()
                .unwrap_or_else(|| panic!("event must parse to Some: {line}"));
            let got = match event.kind {
                NormalizedEventKind::TurnCompleted
                | NormalizedEventKind::TurnCompletedWithUsage { .. } => "TurnCompleted",
                NormalizedEventKind::TurnInterrupted => "TurnInterrupted",
                NormalizedEventKind::TurnFailed { .. } => "TurnFailed",
                NormalizedEventKind::GatewayReady => "GatewayReady",
                other => panic!("unexpected kind: {other:?}"),
            };
            assert_eq!(got, expected_kind, "for line {line}");
        }
    }

    /// Heartbeat / ping lines are presentation bookkeeping. The
    /// transport must return `Ok(None)` so the supervisor does not
    /// log them as NormalizedEvents.
    #[test]
    fn ndjson_returns_none_for_heartbeat_and_ping() {
        let transport = ResumableNdjsonTransport::new(Box::new(CommandCodeDialect::new()));
        for line in [
            r#"{"type":"heartbeat","ts":1}"#,
            r#"{"type":"ping","ts":2}"#,
        ] {
            let event = transport
                .parse_line(line, 1)
                .expect("heartbeat must parse cleanly");
            assert!(event.is_none(), "heartbeat must drop to None: {line}");
        }
    }

    /// Unknown required events fail closed. The transport must
    /// surface them as `CliRuntimeError`, not swallow them.
    #[test]
    fn ndjson_fails_closed_on_unknown_required_event() {
        let transport = ResumableNdjsonTransport::new(Box::new(CommandCodeDialect::new()));
        let line = r#"{"type":"subagent.spawn","team":"research"}"#;
        let error = transport.parse_line(line, 4).unwrap_err();
        assert_eq!(error.code, "commandcode_ndjson_unknown_event");
    }

    /// Malformed JSON fails closed with a distinct error code.
    #[test]
    fn ndjson_fails_closed_on_malformed_line() {
        let transport = ResumableNdjsonTransport::new(Box::new(CommandCodeDialect::new()));
        let error = transport.parse_line("not json", 1).unwrap_err();
        assert_eq!(error.code, "commandcode_ndjson_invalid");
    }

    /// NDJSON lines missing the `type` field fail closed. A line
    /// without a kind cannot be routed to a NormalizedEvent.
    #[test]
    fn ndjson_fails_closed_on_missing_type_field() {
        let transport = ResumableNdjsonTransport::new(Box::new(CommandCodeDialect::new()));
        let error = transport
            .parse_line(r#"{"session_id":"sess-1"}"#, 1)
            .unwrap_err();
        assert_eq!(error.code, "commandcode_ndjson_missing_type");
    }

    #[test]
    fn transport_label_matches_id_module_kind() {
        assert_eq!(KIND, CliTransportKind::ResumableNdjson);
        let transport = ResumableNdjsonTransport::new(Box::new(CommandCodeDialect::new()));
        assert_eq!(transport.to_string(), "resumable_ndjson");
    }
}
