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
