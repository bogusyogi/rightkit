//! The persistent Claude root runtime (PL-005).
//!
//! Owns the four harness obligations the canon row names on top of
//! supervision and policy:
//!
//! - **Session binding** — the vendor session id observed from the
//!   `system/init` event is bound to the the host app session/agent pair
//!   on the persisted binding record (`system:init` →
//!   `SessionStarted`, donor parser).
//! - **start / resume / steer / interrupt / stop** — the persistent
//!   control surface. Steering and interruption use the Claude Code
//!   stream-JSON contract translated from the donor
//!   (`repos/cli/claude-code/src/cli/structuredIO.ts`):
//!   `prependUserMessage` writes an interleaved
//!   `{"type":"user","session_id":"","message":{"role":"user","content":…},"parent_tool_use_id":null}`
//!   frame, and interrupt writes
//!   `{"type":"control_request","request":{"subtype":"interrupt"}}`
//!   (same frame shape the existing transport uses).
//! - **Normalized progress events** — every vendor frame is parsed by
//!   the shared donor parser (`crate::driver_host::parse_claude_stream_line`),
//!   admitted exactly once (`crate::driver_host::dedupe`), and projected onto
//!   [`HarnessProgressKind`] — the single normalized feed CLI, TUI
//!   and desktop presentation subscribe to.
//! - **Binding-metadata persistence** — every state change is written
//!   through the [`BindingMetadataStore`] before it is visible.
//!
//! The runtime is transport-agnostic: the daemon injects the spawner
//! (the shared `StdioProcessIo`-based transport) and the store.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::driver_host::cli_runtime::adapter::CliLaunchSpec;
use crate::driver_host::contracts::{NormalizedEvent, NormalizedEventKind};
use crate::driver_host::{
    DedupAdmission, EventDeduplicator, InMemoryEventDeduplicator, RUN_SCOPED_POLICY,
};

use super::binding::{
    transition_is_valid, BindingMetadataStore, ClaudeHarnessBindingRecord, HarnessBindingStatus,
};
use super::delegation::{
    enforce_delegation_launch_policy, inspect_tool_use_for_native_child_spawn, policy_digest,
};
use super::supervisor::{ClaudeHarnessSupervisor, HarnessSpawner, SupervisedProcessInfo};
use super::workspace::{TrustBoundary, WorkspaceTrustVerdict};
use super::{now_unix_ms, HarnessError};

/// One normalized progress event projected for CLI/TUI/desktop
/// presentation. `seq` is a per-runtime monotonic sequence so
/// consumers can detect drops without trusting vendor offsets;
/// `event_key` is the exact-once identity of the underlying vendor
/// event (or a `host:` synthetic for host-originated actions).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessProgressEvent {
    pub binding_id: String,
    pub host_session_id: String,
    pub seq: u64,
    pub event_key: String,
    pub kind: HarnessProgressKind,
}

/// Normalized progress kinds. Mapped 1:1 from the shared
/// `NormalizedEventKind` vocabulary, plus the harness-specific
/// delegation and lifecycle kinds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HarnessProgressKind {
    SessionBound {
        vendor_session_id: String,
    },
    TurnStarted {
        vendor_turn_id: String,
    },
    AssistantDelta {
        text: String,
    },
    AssistantCompleted {
        text: String,
    },
    ToolCallStarted {
        call_id: String,
        tool_name: String,
        arguments: Value,
    },
    ToolResult {
        call_id: String,
        is_error: bool,
        content: Value,
    },
    ApprovalRequested {
        request_id: String,
        action: String,
        details: Value,
    },
    /// A provider-native child-spawn attempt was observed and must be
    /// denied — child work may only flow through the host app delegation.
    DelegationBypassDetected {
        tool_name: String,
        call_id: String,
    },
    SteerAccepted,
    InterruptRequested,
    TurnCompleted,
    TurnInterrupted,
    TurnFailed {
        message: String,
    },
    GatewayReady,
    /// Vendor rate-limit telemetry; the account is stamped by the host.
    RateLimit {
        status: String,
        rate_limit_type: Option<String>,
        resets_at: Option<u64>,
        utilization: Option<f64>,
    },
    /// The vendor process exited on its own (EOF, not a stop).
    ProcessExited,
    /// A graceful stop completed.
    Stopped,
}

/// Project a normalized vendor event onto zero or more progress kinds.
/// Tool calls may project two kinds when the call is a delegation
/// bypass (the call itself plus the typed bypass detection).
pub fn project_progress_kinds(kind: &NormalizedEventKind) -> Vec<HarnessProgressKind> {
    match kind {
        NormalizedEventKind::SessionStarted {
            vendor_session_id, ..
        } => {
            vec![HarnessProgressKind::SessionBound {
                vendor_session_id: vendor_session_id.clone(),
            }]
        }
        NormalizedEventKind::TurnStarted { vendor_turn_id } => {
            vec![HarnessProgressKind::TurnStarted {
                vendor_turn_id: vendor_turn_id.clone(),
            }]
        }
        NormalizedEventKind::AssistantDelta { text } => {
            vec![HarnessProgressKind::AssistantDelta { text: text.clone() }]
        }
        NormalizedEventKind::AssistantCompleted { text } => {
            vec![HarnessProgressKind::AssistantCompleted { text: text.clone() }]
        }
        NormalizedEventKind::ToolCallStarted {
            call_id,
            tool_name,
            arguments,
        } => {
            let mut kinds = vec![HarnessProgressKind::ToolCallStarted {
                call_id: call_id.clone(),
                tool_name: tool_name.clone(),
                arguments: arguments.clone(),
            }];
            if inspect_tool_use_for_native_child_spawn(tool_name).is_some() {
                kinds.push(HarnessProgressKind::DelegationBypassDetected {
                    tool_name: tool_name.clone(),
                    call_id: call_id.clone(),
                });
            }
            kinds
        }
        NormalizedEventKind::ToolResult {
            call_id,
            is_error,
            content,
        } => vec![HarnessProgressKind::ToolResult {
            call_id: call_id.clone(),
            is_error: *is_error,
            content: content.clone(),
        }],
        NormalizedEventKind::ApprovalRequested {
            request_id,
            action,
            details,
        } => vec![HarnessProgressKind::ApprovalRequested {
            request_id: request_id.clone(),
            action: action.clone(),
            details: details.clone(),
        }],
        NormalizedEventKind::TurnCompleted | NormalizedEventKind::TurnCompletedWithUsage { .. } => {
            vec![HarnessProgressKind::TurnCompleted]
        }
        NormalizedEventKind::TurnInterrupted => vec![HarnessProgressKind::TurnInterrupted],
        NormalizedEventKind::TurnFailed { message } => vec![HarnessProgressKind::TurnFailed {
            message: message.clone(),
        }],
        NormalizedEventKind::GatewayReady => vec![HarnessProgressKind::GatewayReady],
        NormalizedEventKind::RateLimit {
            status,
            rate_limit_type,
            resets_at,
            utilization,
            ..
        } => vec![HarnessProgressKind::RateLimit {
            status: status.clone(),
            rate_limit_type: rate_limit_type.clone(),
            resets_at: *resets_at,
            utilization: *utilization,
        }],
        // Cost is attributed per account by the daemon reader, not the harness.
        NormalizedEventKind::TurnCostReported { .. } => Vec::new(),
    }
}

/// The steering frame from the Claude Code stream-JSON contract
/// (donor `StructuredIO::prependUserMessage`): an interleaved user
/// message with an empty session id and `parent_tool_use_id: null`.
pub fn steer_frame(text: &str) -> String {
    serde_json::json!({
        "type": "user",
        "session_id": "",
        "message": {"role": "user", "content": text},
        "parent_tool_use_id": Value::Null,
    })
    .to_string()
}

/// The interrupt control frame (`structuredIO.ts` control protocol), built by RightKit
/// (`crate::claude`). The Agent SDK control protocol correlates `control_response` by
/// `request_id`; the owner supplies a process-unique one. The write adds the line break.
pub fn interrupt_frame() -> String {
    let request_id = crate::claude::next_request_id("interrupt");
    crate::claude::interrupt_frame(&request_id)
        .trim_end()
        .to_string()
}

struct SteeringState {
    steers_sent: u64,
    interrupt_requested: bool,
}

/// The control request a `request_id` was issued for (issue #337).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlKind {
    Initialize,
    Interrupt,
    SetModel,
    SetPermissionMode,
}

/// A control request the CLI answered with an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlRejection {
    pub request_id: String,
    pub kind: ControlKind,
    pub message: String,
}

/// What the CLI has told the host over the control channel (issue #337): the capabilities it
/// advertised in `system/init`, the typed `initialize` answer (account info, models), the last
/// interrupt receipt and every refused control request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ControlState {
    /// `capabilities` of `system/init`, such as `interrupt_receipt_v1`.
    pub capabilities: Vec<String>,
    pub initialize: Option<crate::claude::InitializeResponse>,
    pub interrupt_receipt: Option<crate::claude::InterruptReceipt>,
    pub rejected: Vec<ControlRejection>,
    /// Successful `set_model` / `set_permission_mode` answers, in order.
    pub acknowledged: Vec<(String, ControlKind)>,
}

/// Refuse a durable binding whose authority-bearing identity differs
/// from this open request. A binding id is not an authorization token:
/// it remains bound to its original the host app session, agent,
/// workspace grant, managed profile & policy.
fn validate_reopened_binding(
    record: &ClaudeHarnessBindingRecord,
    initial: &ClaudeHarnessBindingRecord,
    trust: &TrustBoundary,
) -> Result<(), HarnessError> {
    if record.engine != initial.engine
        || record.host_session_id != initial.host_session_id
        || record.coderight_agent_id != initial.coderight_agent_id
        || record.managed_profile_label != initial.managed_profile_label
        || record.managed_home != initial.managed_home
        || record.delegation_policy_digest != initial.delegation_policy_digest
    {
        return Err(HarnessError::InvalidState(format!(
            "binding {} cannot be reopened under a different harness identity or policy",
            record.binding_id
        )));
    }
    match &record.trust {
        WorkspaceTrustVerdict::Trusted { root, .. } if root == trust.root() => Ok(()),
        _ => Err(HarnessError::Trust(format!(
            "binding {} cannot be reopened outside its original trusted workspace",
            record.binding_id
        ))),
    }
}

/// One persistent Claude Code root runtime: a supervised vendor
/// process bound to a the host app session, with validated start/resume,
/// donor-contract steering/interrupt, exact-once normalized progress
/// projection and durable binding metadata.
pub struct ClaudeRootRuntime {
    binding: Mutex<ClaudeHarnessBindingRecord>,
    store: Arc<dyn BindingMetadataStore>,
    supervisor: ClaudeHarnessSupervisor,
    trust: TrustBoundary,
    steering: Mutex<SteeringState>,
    progress_tx: mpsc::UnboundedSender<HarnessProgressEvent>,
    progress_seq: AtomicU64,
    dedupe: Mutex<InMemoryEventDeduplicator>,
    read_offset: AtomicU64,
    /// Sibling `tool_use` events of a parallel-tool-call assistant line, served before the next
    /// frame is read.
    pending_events: Mutex<std::collections::VecDeque<NormalizedEvent>>,
    /// Control requests written and not yet answered: `request_id` to what was asked.
    pending_controls: Mutex<std::collections::HashMap<String, ControlKind>>,
    control_state: Mutex<ControlState>,
}

impl ClaudeRootRuntime {
    /// Open (create or reload) a persistent Claude root runtime.
    ///
    /// A new binding is persisted with status `Starting` before any
    /// process exists. Reopening preserves its durable cursor,
    /// observed vendor identity & audit counters; it refuses an
    /// attempt to attach that binding to another the host app identity,
    /// workspace grant, profile or run policy.
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        binding_id: &str,
        host_session_id: &str,
        coderight_agent_id: &str,
        trust: TrustBoundary,
        managed_profile_label: &str,
        managed_home: std::path::PathBuf,
        store: Arc<dyn BindingMetadataStore>,
        spawner: HarnessSpawner,
    ) -> Result<(Arc<Self>, mpsc::UnboundedReceiver<HarnessProgressEvent>), HarnessError> {
        let now = now_unix_ms();
        let initial_record = ClaudeHarnessBindingRecord::new(
            binding_id,
            host_session_id,
            coderight_agent_id,
            trust.verdict(now),
            managed_profile_label,
            managed_home,
            policy_digest(RUN_SCOPED_POLICY),
            now,
        );
        let record = match store.load(binding_id)? {
            Some(record) => {
                validate_reopened_binding(&record, &initial_record, &trust)?;
                record
            }
            None => {
                store.save(&initial_record)?;
                initial_record
            }
        };
        let (progress_tx, progress_rx) = mpsc::unbounded_channel();
        let runtime = Arc::new(Self {
            binding: Mutex::new(record),
            store,
            supervisor: ClaudeHarnessSupervisor::new(spawner),
            trust,
            steering: Mutex::new(SteeringState {
                steers_sent: 0,
                interrupt_requested: false,
            }),
            progress_tx,
            progress_seq: AtomicU64::new(0),
            dedupe: Mutex::new(InMemoryEventDeduplicator::default()),
            read_offset: AtomicU64::new(0),
            pending_events: Mutex::new(std::collections::VecDeque::new()),
            pending_controls: Mutex::new(std::collections::HashMap::new()),
            control_state: Mutex::new(ControlState::default()),
        });
        Ok((runtime, progress_rx))
    }

    /// The current binding record (snapshot).
    pub fn binding_snapshot(&self) -> ClaudeHarnessBindingRecord {
        self.binding
            .lock()
            .expect("claude harness binding mutex poisoned")
            .clone()
    }

    /// Steering diagnostics: `(steers accepted, interrupt requested)`.
    /// `interrupt_requested` clears when the vendor's interrupted
    /// turn completes.
    pub fn steering_state(&self) -> (u64, bool) {
        let steering = self
            .steering
            .lock()
            .expect("claude harness steering mutex poisoned");
        (steering.steers_sent, steering.interrupt_requested)
    }

    /// Start a fresh persistent session from an adapter-produced
    /// launch spec. Fail-closed order:
    ///
    /// 1. the session cwd must be inside the trusted workspace root
    ///    (the process then runs *in* the workspace);
    /// 2. the effective argv must satisfy the delegation launch
    ///    policy (native tool surface removed, the host app MCP gateway
    ///    only, run-scoped policy appended, no permission bypass, env
    ///    denial intact);
    /// 3. the managed vendor home is pinned via `HOME`/`CLAUDE_CONFIG_DIR`
    ///    (never the ambient profile).
    pub fn start(&self, launch: &CliLaunchSpec) -> Result<SupervisedProcessInfo, HarnessError> {
        // Validate the lifecycle move before touching the vendor
        // process so a rejected start never respawns anything.
        let current = self.binding_snapshot().status;
        if !transition_is_valid(current, HarnessBindingStatus::Ready) {
            return Err(HarnessError::InvalidTransition {
                from: current,
                to: HarnessBindingStatus::Ready,
            });
        }
        let spec = self.finalize_launch(launch)?;
        let info = self.supervisor.start(&spec)?;
        self.with_binding(|record| {
            record.workspace_root = spec.cwd.clone();
            record.process_generation = info.generation;
            record.status = HarnessBindingStatus::Ready;
            record.updated_at_unix_ms = now_unix_ms();
            Ok(())
        })?;
        Ok(info)
    }

    /// Resume an existing vendor session: stop the old process,
    /// advance the process generation, relaunch with the donor
    /// `--resume <id>` flag and mark the binding `Resuming`. The
    /// vendor session id re-binds from the resumed `system/init`.
    pub fn resume(
        &self,
        launch: &CliLaunchSpec,
        vendor_session_id: &str,
    ) -> Result<SupervisedProcessInfo, HarnessError> {
        if vendor_session_id.trim().is_empty() {
            return Err(HarnessError::InvalidState(
                "cannot resume an empty Claude session id".to_string(),
            ));
        }
        // Validate the lifecycle move before touching the vendor
        // process so a rejected resume never respawns anything.
        let current = self.binding_snapshot().status;
        if !transition_is_valid(current, HarnessBindingStatus::Resuming) {
            return Err(HarnessError::InvalidTransition {
                from: current,
                to: HarnessBindingStatus::Resuming,
            });
        }
        let spec = self.finalize_launch(launch)?;
        let args: Vec<&str> = spec.argv.iter().map(String::as_str).collect();
        let carries_resume = args
            .windows(2)
            .any(|pair| pair[0] == "--resume" && pair[1] == vendor_session_id);
        if !carries_resume {
            return Err(HarnessError::Delegation(format!(
                "resume launch must carry the `--resume {vendor_session_id}` flag pair"
            )));
        }
        let info = self.supervisor.restart(&spec)?;
        self.with_binding(|record| {
            record.workspace_root = spec.cwd.clone();
            record.process_generation = info.generation;
            record.vendor_session_id = Some(vendor_session_id.to_string());
            record.vendor_turn_id = None;
            record.status = HarnessBindingStatus::Resuming;
            record.updated_at_unix_ms = now_unix_ms();
            Ok(())
        })?;
        Ok(info)
    }

    /// Steer the running turn: write an interleaved user message
    /// (donor stream-JSON contract). Fail-closed: steering requires a
    /// running session; nothing is silently queued or dropped.
    pub fn steer(&self, text: &str) -> Result<(), HarnessError> {
        if text.trim().is_empty() {
            return Err(HarnessError::SteerRejected(
                "steer text must not be empty".to_string(),
            ));
        }
        let status = self.binding_snapshot().status;
        if status != HarnessBindingStatus::Running {
            return Err(HarnessError::SteerRejected(format!(
                "steering requires a running session, current status is {status:?}"
            )));
        }
        let io = self.supervisor.io()?;
        io.write_line(&steer_frame(text))?;
        {
            let mut steering = self
                .steering
                .lock()
                .map_err(|_| HarnessError::InvalidState("steering mutex poisoned".to_string()))?;
            steering.steers_sent += 1;
        }
        self.emit_host_progress("host:steer", HarnessProgressKind::SteerAccepted);
        Ok(())
    }

    /// Interrupt the in-flight turn with the donor control frame. The
    /// vendor acknowledges through a `TurnInterrupted` result event.
    pub fn interrupt(&self) -> Result<(), HarnessError> {
        let status = self.binding_snapshot().status;
        if !matches!(
            status,
            HarnessBindingStatus::Running | HarnessBindingStatus::WaitingForApproval
        ) {
            return Err(HarnessError::InvalidState(format!(
                "interrupt requires an active turn, current status is {status:?}"
            )));
        }
        let request_id = crate::claude::next_request_id("interrupt");
        let frame = crate::claude::interrupt_frame(&request_id);
        self.write_control(ControlKind::Interrupt, &request_id, frame.trim_end())?;
        {
            let mut steering = self
                .steering
                .lock()
                .map_err(|_| HarnessError::InvalidState("steering mutex poisoned".to_string()))?;
            steering.interrupt_requested = true;
        }
        self.emit_host_progress("host:interrupt", HarnessProgressKind::InterruptRequested);
        Ok(())
    }

    /// Write one control request and remember which request id asked what, so the CLI's
    /// `control_response` can be matched to it.
    fn write_control(
        &self,
        kind: ControlKind,
        request_id: &str,
        frame: &str,
    ) -> Result<(), HarnessError> {
        let io = self.supervisor.io()?;
        self.pending_controls
            .lock()
            .map_err(|_| HarnessError::InvalidState("control mutex poisoned".to_string()))?
            .insert(request_id.to_string(), kind);
        if let Err(error) = io.write_line(frame) {
            if let Ok(mut pending) = self.pending_controls.lock() {
                pending.remove(request_id);
            }
            return Err(error.into());
        }
        Ok(())
    }

    fn require_live_session(&self, what: &str) -> Result<(), HarnessError> {
        let status = self.binding_snapshot().status;
        if matches!(
            status,
            HarnessBindingStatus::Ready
                | HarnessBindingStatus::Running
                | HarnessBindingStatus::WaitingForApproval
        ) {
            Ok(())
        } else {
            Err(HarnessError::InvalidState(format!(
                "{what} requires a started session, current status is {status:?}"
            )))
        }
    }

    /// Ask the CLI for its `initialize` answer (account info and models, issue #337). Returns
    /// the request id; the typed answer lands in [`Self::control_state`] when it arrives.
    pub fn request_initialize(&self) -> Result<String, HarnessError> {
        self.require_live_session("initialize")?;
        let request_id = crate::claude::next_request_id("initialize");
        let frame = crate::claude::initialize_frame(&request_id, None);
        self.write_control(ControlKind::Initialize, &request_id, frame.trim_end())?;
        Ok(request_id)
    }

    /// Switch the running session's model (`None` selects the CLI default). The id is validated
    /// by RightKit before it reaches the wire.
    pub fn set_model(&self, model: Option<&str>) -> Result<String, HarnessError> {
        self.require_live_session("set_model")?;
        let request_id = crate::claude::next_request_id("set_model");
        let frame = crate::claude::set_model_frame(&request_id, model)
            .map_err(HarnessError::InvalidState)?;
        self.write_control(ControlKind::SetModel, &request_id, frame.trim_end())?;
        Ok(request_id)
    }

    /// Switch the running session's permission mode. `bypassPermissions` is refused.
    pub fn set_permission_mode(&self, mode: &str) -> Result<String, HarnessError> {
        self.require_live_session("set_permission_mode")?;
        let request_id = crate::claude::next_request_id("set_permission_mode");
        let frame = crate::claude::set_permission_mode_frame(&request_id, mode)
            .map_err(HarnessError::InvalidState)?;
        self.write_control(
            ControlKind::SetPermissionMode,
            &request_id,
            frame.trim_end(),
        )?;
        Ok(request_id)
    }

    /// Everything the CLI has reported over the control channel so far.
    pub fn control_state(&self) -> ControlState {
        self.control_state
            .lock()
            .expect("claude harness control mutex poisoned")
            .clone()
    }

    /// Match a `control_response` line to the request it answers. An answer to a request this
    /// runtime did not make (the `can_use_tool` replies are written elsewhere) is ignored.
    fn handle_control_response(&self, value: &Value) {
        let Some(response) = crate::claude::parse_control_response(value) else {
            return;
        };
        let Some(kind) = self
            .pending_controls
            .lock()
            .ok()
            .and_then(|mut pending| pending.remove(&response.request_id))
        else {
            return;
        };
        let Ok(mut state) = self.control_state.lock() else {
            return;
        };
        match response.outcome {
            crate::event::ControlOutcome::Success { payload } => match kind {
                ControlKind::Initialize => {
                    state.initialize = Some(crate::claude::parse_initialize_response(&payload));
                }
                ControlKind::Interrupt => {
                    state.interrupt_receipt =
                        crate::claude::parse_interrupt_receipt(&payload, &state.capabilities);
                }
                ControlKind::SetModel | ControlKind::SetPermissionMode => {
                    state.acknowledged.push((response.request_id, kind));
                }
            },
            crate::event::ControlOutcome::Error { message } => {
                state.rejected.push(ControlRejection {
                    request_id: response.request_id,
                    kind,
                    message,
                });
            }
        }
    }

    /// Graceful stop of the persistent runtime. Idempotent: stopping
    /// a stopped runtime succeeds without rewriting history.
    pub fn stop(&self) -> Result<(), HarnessError> {
        self.supervisor.stop()?;
        let current = self.binding_snapshot().status;
        if current != HarnessBindingStatus::Completed {
            self.with_binding(|record| {
                if !transition_is_valid(record.status, HarnessBindingStatus::Completed) {
                    return Err(HarnessError::InvalidTransition {
                        from: record.status,
                        to: HarnessBindingStatus::Completed,
                    });
                }
                record.status = HarnessBindingStatus::Completed;
                record.updated_at_unix_ms = now_unix_ms();
                Ok(())
            })?;
        }
        self.emit_host_progress("host:stop", HarnessProgressKind::Stopped);
        Ok(())
    }

    /// Read, normalize and project the next vendor event. Returns
    /// `Ok(None)` when the vendor process exited without a terminal
    /// event (EOF); in that case the binding is marked `Failed` and a
    /// `ProcessExited` progress event is projected. Presentation
    /// bookkeeping frames (hooks, control acks) are consumed here and
    /// produce no progress event; duplicates are admitted exactly
    /// once and counted on the binding.
    pub fn next_event(&self) -> Result<Option<NormalizedEvent>, HarnessError> {
        loop {
            let queued = self
                .pending_events
                .lock()
                .map_err(|_| {
                    HarnessError::InvalidState("pending events mutex poisoned".to_string())
                })?
                .pop_front();
            let event = match queued {
                Some(event) => event,
                None => {
                    let io = self.supervisor.io()?;
                    let value = io.read_event()?;
                    let Some(value) = value else {
                        let had_process = self.supervisor.mark_exited()?;
                        if had_process {
                            self.with_binding(|record| {
                                if record.status != HarnessBindingStatus::Completed {
                                    record.status = HarnessBindingStatus::Failed;
                                    record.updated_at_unix_ms = now_unix_ms();
                                }
                                Ok(())
                            })?;
                            self.emit_host_progress("host:eof", HarnessProgressKind::ProcessExited);
                        }
                        return Ok(None);
                    };
                    if value.get("type").and_then(Value::as_str) == Some("control_response") {
                        self.handle_control_response(&value);
                        continue;
                    }
                    // The shared donor parser consumes a `&str`; serialize the
                    // frame back exactly like `ClaudeAdapter::normalize_provider_event`
                    // does (serde_json's default map keeps this deterministic).
                    let line = serde_json::to_string(&value)
                        .map_err(|error| HarnessError::InvalidState(error.to_string()))?;
                    let offset = self.read_offset.fetch_add(1, Ordering::SeqCst) + 1;
                    let event = match crate::driver_host::parse_claude_stream_line(&line, offset) {
                        Ok(event) => event,
                        Err(error) => {
                            // Fail closed on malformed or unknown-required
                            // frames, matching the transport contract.
                            return Err(HarnessError::from(error));
                        }
                    };
                    let Some(event) = event else {
                        continue; // presentation bookkeeping: no projection
                    };
                    // Parallel tool calls: queue the second and later `tool_use` blocks of this line.
                    let siblings =
                        crate::driver_host::claude::claude_extra_tool_use_events(&line, offset)
                            .map_err(HarnessError::from)?;
                    if !siblings.is_empty() {
                        self.pending_events
                            .lock()
                            .map_err(|_| {
                                HarnessError::InvalidState(
                                    "pending events mutex poisoned".to_string(),
                                )
                            })?
                            .extend(siblings);
                    }
                    event
                }
            };
            let admission = {
                let mut dedupe = self
                    .dedupe
                    .lock()
                    .map_err(|_| HarnessError::InvalidState("dedupe mutex poisoned".to_string()))?;
                dedupe
                    .admit(&event)
                    .expect("in-memory admission is infallible")
            };
            if admission == DedupAdmission::Duplicate {
                self.with_binding(|record| {
                    record.duplicate_events_dropped =
                        record.duplicate_events_dropped.saturating_add(1);
                    Ok(())
                })?;
                continue;
            }
            self.apply_event(&event)?;
            return Ok(Some(event));
        }
    }

    /// Apply one admitted vendor event to the binding and project its
    /// progress events.
    fn apply_event(&self, event: &NormalizedEvent) -> Result<(), HarnessError> {
        let mut progress_kinds = project_progress_kinds(&event.kind);
        if let NormalizedEventKind::SessionStarted {
            init: Some(init), ..
        } = &event.kind
        {
            if let Ok(mut state) = self.control_state.lock() {
                state.capabilities = init.capabilities.clone();
            }
        }
        self.with_binding(|record| {
            record.cursor.output_offset = record.cursor.output_offset.saturating_add(1);
            record.cursor.last_event_key = Some(event.event_key.clone());
            match &event.kind {
                NormalizedEventKind::SessionStarted {
                    vendor_session_id, ..
                } => {
                    record.vendor_session_id = Some(vendor_session_id.clone());
                    if transition_is_valid(record.status, HarnessBindingStatus::Running)
                        && record.status != HarnessBindingStatus::Running
                    {
                        record.status = HarnessBindingStatus::Running;
                    }
                }
                NormalizedEventKind::TurnStarted { vendor_turn_id } => {
                    record.vendor_turn_id = Some(vendor_turn_id.clone());
                }
                NormalizedEventKind::ToolCallStarted { tool_name, .. } => {
                    if inspect_tool_use_for_native_child_spawn(tool_name).is_some() {
                        record.native_child_spawn_violations =
                            record.native_child_spawn_violations.saturating_add(1);
                    }
                }
                NormalizedEventKind::ApprovalRequested { .. } => {
                    if transition_is_valid(record.status, HarnessBindingStatus::WaitingForApproval)
                    {
                        record.status = HarnessBindingStatus::WaitingForApproval;
                    }
                }
                NormalizedEventKind::TurnInterrupted => {
                    if transition_is_valid(record.status, HarnessBindingStatus::Interrupted) {
                        record.status = HarnessBindingStatus::Interrupted;
                    }
                }
                NormalizedEventKind::TurnFailed { .. } => {
                    if transition_is_valid(record.status, HarnessBindingStatus::Failed) {
                        record.status = HarnessBindingStatus::Failed;
                    }
                }
                NormalizedEventKind::TurnCompleted
                | NormalizedEventKind::TurnCompletedWithUsage { .. } => {
                    // A completed turn does not complete the binding:
                    // the persistent runtime stays ready for the next
                    // turn (session-level status unchanged).
                    if let Some(mut steering) = self.steering.try_lock().ok() {
                        steering.interrupt_requested = false;
                    }
                    if matches!(
                        record.status,
                        HarnessBindingStatus::Interrupted
                            | HarnessBindingStatus::WaitingForApproval
                    ) {
                        record.status = HarnessBindingStatus::Running;
                    }
                }
                NormalizedEventKind::GatewayReady => {
                    if record.status == HarnessBindingStatus::Starting {
                        record.status = HarnessBindingStatus::Ready;
                    }
                }
                NormalizedEventKind::AssistantDelta { .. }
                | NormalizedEventKind::AssistantCompleted { .. }
                | NormalizedEventKind::ToolResult { .. }
                | NormalizedEventKind::RateLimit { .. }
                | NormalizedEventKind::TurnCostReported { .. } => {
                    // Presentation/telemetry content: cursor advances, no
                    // binding-status change.
                }
            }
            record.updated_at_unix_ms = now_unix_ms();
            Ok(())
        })?;
        for kind in progress_kinds.drain(..) {
            self.emit_host_progress(&event.event_key, kind);
        }
        Ok(())
    }

    /// Validate and finalize a launch: trust boundary first, then the
    /// delegation launch policy, then pin the managed vendor home.
    /// Returns the spec that will actually be spawned. The process
    /// runs in the session cwd (which must sit inside the trusted
    /// workspace root); the vendor profile is pinned to the managed
    /// home recorded at open time via `HOME`/`CLAUDE_CONFIG_DIR`
    /// (`managed_home` is mandatory), never the ambient host profile.
    fn finalize_launch(&self, launch: &CliLaunchSpec) -> Result<CliLaunchSpec, HarnessError> {
        self.trust.admit_cwd(&launch.cwd)?;
        enforce_delegation_launch_policy(launch)
            .map_err(|violation| HarnessError::Delegation(violation.to_string()))?;
        let managed_home = {
            let record = self
                .binding
                .lock()
                .map_err(|_| HarnessError::InvalidState("binding mutex poisoned".to_string()))?;
            record.managed_home.clone()
        };
        let mut spec = launch.clone();
        for name in ["HOME", "CLAUDE_CONFIG_DIR"] {
            let value = managed_home.display().to_string();
            match spec.env.iter_mut().find(|(key, _)| key.as_str() == name) {
                Some(entry) => entry.1 = value,
                None => spec.env.push((name.to_string(), value)),
            }
        }
        Ok(spec)
    }

    fn with_binding<R>(
        &self,
        mutate: impl FnOnce(&mut ClaudeHarnessBindingRecord) -> Result<R, HarnessError>,
    ) -> Result<R, HarnessError> {
        let mut record = self
            .binding
            .lock()
            .map_err(|_| HarnessError::InvalidState("binding mutex poisoned".to_string()))?;
        // Persist a fully formed candidate before publishing it to
        // in-memory readers. A failed store write must leave the live
        // binding unchanged, otherwise a later restart would silently
        // lose an already-visible cursor or lifecycle transition.
        let mut candidate = record.clone();
        let result = mutate(&mut candidate)?;
        self.store.save(&candidate)?;
        *record = candidate;
        Ok(result)
    }

    fn emit_host_progress(&self, event_key: &str, kind: HarnessProgressKind) {
        let (binding_id, host_session_id) = {
            let record = self
                .binding
                .lock()
                .expect("claude harness binding mutex poisoned");
            (record.binding_id.clone(), record.host_session_id.clone())
        };
        let seq = self.progress_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.progress_tx.send(HarnessProgressEvent {
            binding_id,
            host_session_id,
            seq,
            event_key: event_key.to_string(),
            kind,
        });
    }
}
