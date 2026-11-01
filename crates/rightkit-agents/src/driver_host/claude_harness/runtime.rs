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
        let io = self.supervisor.io()?;
        io.write_line(&interrupt_frame())?;
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
    /// (donor: `managed_home` is mandatory and
    /// `begin_native_login` pins exactly those two variables), never
    /// the ambient host profile.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver_host::claude_harness::binding::InMemoryBindingMetadataStore;
    use crate::driver_host::claude_harness::test_support::{
        claude_launch_for_workspace, ScriptedInner, ScriptedProcessIo, ScriptedSpawner,
    };
    use crate::driver_host::claude_harness::workspace::TrustBoundary;
    use std::path::PathBuf;

    const VENDOR_SESSION_ID: &str = "vendor-session-1";

    fn init_event() -> String {
        format!(r#"{{"type":"system","subtype":"init","session_id":"{VENDOR_SESSION_ID}"}}"#)
    }

    fn init_event_with_uuid(uuid: &str) -> String {
        format!(
            r#"{{"type":"system","subtype":"init","session_id":"{VENDOR_SESSION_ID}","uuid":"{uuid}"}}"#
        )
    }

    struct HarnessFixture {
        _dir_guard: tempfile::TempDir,
        workspace: PathBuf,
        scripted: Arc<ScriptedInner>,
        spawner: ScriptedSpawner,
        runtime: Arc<ClaudeRootRuntime>,
        progress: mpsc::UnboundedReceiver<HarnessProgressEvent>,
        store: Arc<InMemoryBindingMetadataStore>,
    }

    fn open_fixture(lines: Vec<String>) -> HarnessFixture {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("work");
        let trust = TrustBoundary::grant(dir.path(), 100).unwrap();
        let store = Arc::new(InMemoryBindingMetadataStore::new());
        let scripted = ScriptedProcessIo::new(lines);
        let inner = scripted.inner();
        let spawner = ScriptedSpawner::new(inner.clone());
        let (runtime, progress) = ClaudeRootRuntime::open(
            "binding-root",
            "session-1",
            "agent-root",
            trust,
            "default",
            dir.path().join("profiles").join("claude").join("default"),
            store.clone(),
            spawner.harness_spawner(),
        )
        .unwrap();
        HarnessFixture {
            _dir_guard: dir,
            workspace,
            scripted: inner,
            spawner,
            runtime,
            progress,
            store,
        }
    }

    fn drain_progress(fixture: &mut HarnessFixture) -> Vec<HarnessProgressKind> {
        let mut kinds = Vec::new();
        while let Ok(event) = fixture.progress.try_recv() {
            kinds.push(event.kind);
        }
        kinds
    }

    #[test]
    fn start_binds_vendor_session_from_init_and_persists_binding() {
        let mut fixture = open_fixture(vec![init_event()]);
        let launch = claude_launch_for_workspace(&fixture.workspace);
        let info = fixture.runtime.start(&launch).unwrap();
        assert_eq!(info.generation, 0);
        assert_eq!(
            fixture.runtime.binding_snapshot().status,
            HarnessBindingStatus::Ready,
            "start moves Starting -> Ready"
        );

        let event = fixture
            .runtime
            .next_event()
            .unwrap()
            .expect("init event must normalize to Some");
        assert!(matches!(
            event.kind,
            NormalizedEventKind::SessionStarted { .. }
        ));

        let binding = fixture.runtime.binding_snapshot();
        assert_eq!(
            binding.vendor_session_id.as_deref(),
            Some(VENDOR_SESSION_ID),
            "vendor session id must bind from system/init"
        );
        assert_eq!(binding.status, HarnessBindingStatus::Running);
        assert_eq!(binding.workspace_root, fixture.workspace);
        assert_eq!(
            binding.cursor.last_event_key.as_deref(),
            Some(event.event_key.as_str())
        );

        // The persisted record matches the in-memory binding.
        let persisted = fixture.store.load("binding-root").unwrap().unwrap();
        assert_eq!(persisted, binding);

        // The progress feed carried the session binding.
        let kinds = drain_progress(&mut fixture);
        assert!(kinds.contains(&HarnessProgressKind::SessionBound {
            vendor_session_id: VENDOR_SESSION_ID.to_string()
        }));
    }

    #[test]
    fn start_refuses_untrusted_workspace_before_spawning() {
        let mut fixture = open_fixture(vec![]);
        let mut launch = claude_launch_for_workspace(&fixture.workspace);
        launch.cwd = PathBuf::from("/somewhere-else-entirely");
        let error = fixture.runtime.start(&launch).unwrap_err();
        assert!(matches!(error, HarnessError::Trust(_)));
        // No process was spawned and the binding stays Starting.
        assert_eq!(
            fixture.runtime.binding_snapshot().status,
            HarnessBindingStatus::Starting
        );
        assert!(fixture.spawner.launched().is_empty());
    }

    #[test]
    fn start_refuses_launch_spec_that_breaks_the_delegation_policy() {
        let mut fixture = open_fixture(vec![]);
        let mut launch = claude_launch_for_workspace(&fixture.workspace);
        let index = launch
            .argv
            .iter()
            .position(|arg| arg == "--allowedTools")
            .unwrap();
        launch.argv[index + 1] = "Bash".to_string();
        let error = fixture.runtime.start(&launch).unwrap_err();
        assert!(
            error.to_string().contains("allowed tools"),
            "policy failure must name the violation: {error}"
        );
        assert_eq!(
            fixture.runtime.binding_snapshot().status,
            HarnessBindingStatus::Starting
        );
        assert!(fixture.spawner.launched().is_empty());
    }

    /// The runtime finalizes the launch before spawning: the process
    /// runs in the trusted session cwd and the vendor profile is
    /// pinned to the managed home via HOME/CLAUDE_CONFIG_DIR.
    #[test]
    fn start_pins_the_managed_vendor_home_env() {
        let mut fixture = open_fixture(vec![]);
        let launch = claude_launch_for_workspace(&fixture.workspace);
        fixture.runtime.start(&launch).unwrap();

        let launched = fixture.spawner.launched();
        assert_eq!(launched.len(), 1);
        let spec = &launched[0];
        assert_eq!(spec.cwd, fixture.workspace);
        let managed_home = fixture.runtime.binding_snapshot().managed_home;
        let expected = managed_home.display().to_string();
        for name in ["HOME", "CLAUDE_CONFIG_DIR"] {
            let pinned = spec
                .env
                .iter()
                .find(|(key, _)| key.as_str() == name)
                .expect("managed home variable must be pinned");
            assert_eq!(pinned.1, expected, "{name} must point at the managed home");
        }
    }

    #[test]
    fn steer_writes_donor_sdk_user_message_frame() {
        let mut fixture = open_fixture(vec![init_event()]);
        fixture
            .runtime
            .start(&claude_launch_for_workspace(&fixture.workspace))
            .unwrap();
        fixture.runtime.next_event().unwrap().unwrap();

        fixture.runtime.steer("focus on the parser").unwrap();
        let written = fixture.scripted.written();
        assert_eq!(written.len(), 1);
        let frame: Value = serde_json::from_str(&written[0]).unwrap();
        assert_eq!(frame["type"], "user");
        assert_eq!(frame["session_id"], "");
        assert_eq!(frame["message"]["role"], "user");
        assert_eq!(frame["message"]["content"], "focus on the parser");
        assert!(frame["parent_tool_use_id"].is_null());

        let kinds = drain_progress(&mut fixture);
        assert!(kinds.contains(&HarnessProgressKind::SteerAccepted));
    }

    #[test]
    fn steer_requires_running_session() {
        let mut fixture = open_fixture(vec![]);
        // Not started: steering is rejected, nothing is queued.
        let error = fixture.runtime.steer("too early").unwrap_err();
        assert!(matches!(error, HarnessError::SteerRejected(_)));
        assert!(fixture.scripted.written().is_empty());

        // Empty steer text is rejected too.
        fixture
            .runtime
            .start(&claude_launch_for_workspace(&fixture.workspace))
            .unwrap();
        let error = fixture.runtime.steer("   ").unwrap_err();
        assert!(matches!(error, HarnessError::SteerRejected(_)));
    }

    #[test]
    fn interrupt_writes_donor_control_request_frame() {
        let mut fixture = open_fixture(vec![init_event()]);
        fixture
            .runtime
            .start(&claude_launch_for_workspace(&fixture.workspace))
            .unwrap();
        fixture.runtime.next_event().unwrap().unwrap();

        fixture.runtime.interrupt().unwrap();
        let written = fixture.scripted.written();
        assert_eq!(written.len(), 1);
        let frame: Value = serde_json::from_str(&written[0]).unwrap();
        assert_eq!(frame["type"], "control_request");
        assert_eq!(frame["request"]["subtype"], "interrupt");
        let kinds = drain_progress(&mut fixture);
        assert!(kinds.contains(&HarnessProgressKind::InterruptRequested));

        // The vendor acknowledges with a result/interrupted event.
        fixture
            .scripted
            .lines
            .lock()
            .unwrap()
            .push_back(r#"{"type":"result","subtype":"interrupted"}"#.to_string());
        let event = fixture.runtime.next_event().unwrap().unwrap();
        assert!(matches!(event.kind, NormalizedEventKind::TurnInterrupted));
        assert_eq!(
            fixture.runtime.binding_snapshot().status,
            HarnessBindingStatus::Interrupted
        );
    }

    #[test]
    fn stop_shuts_down_and_persists_terminal_status() {
        let mut fixture = open_fixture(vec![init_event()]);
        fixture
            .runtime
            .start(&claude_launch_for_workspace(&fixture.workspace))
            .unwrap();
        fixture.runtime.next_event().unwrap().unwrap();

        fixture.runtime.stop().unwrap();
        assert_eq!(fixture.scripted.shutdown_count(), 1);
        assert_eq!(
            fixture.runtime.binding_snapshot().status,
            HarnessBindingStatus::Completed
        );
        let persisted = fixture.store.load("binding-root").unwrap().unwrap();
        assert_eq!(persisted.status, HarnessBindingStatus::Completed);
        // Idempotent.
        fixture.runtime.stop().unwrap();
        assert_eq!(fixture.scripted.shutdown_count(), 1);
        let kinds = drain_progress(&mut fixture);
        assert!(kinds.contains(&HarnessProgressKind::Stopped));
        // Steering after stop is rejected.
        assert!(matches!(
            fixture.runtime.steer("again").unwrap_err(),
            HarnessError::SteerRejected(_)
        ));
    }

    #[test]
    fn resume_relaunches_with_resume_flag_and_bumps_process_generation() {
        let mut fixture = open_fixture(vec![init_event()]);
        let launch = claude_launch_for_workspace(&fixture.workspace);
        fixture.runtime.start(&launch).unwrap();
        fixture.runtime.next_event().unwrap().unwrap();
        fixture.runtime.stop().unwrap();

        // A resume launch must carry the --resume pair for the
        // recorded vendor session.
        let mut resume_launch = claude_launch_for_workspace(&fixture.workspace);
        let pos = resume_launch
            .argv
            .iter()
            .position(|arg| arg == "--session-id")
            .unwrap();
        resume_launch.argv[pos] = "--resume".to_string();
        // The flag's value must be the *vendor* session id, not the launch's
        // own session uuid — that pair is what `resume` validates.
        resume_launch.argv[pos + 1] = VENDOR_SESSION_ID.to_string();

        let info = fixture
            .runtime
            .resume(&resume_launch, VENDOR_SESSION_ID)
            .unwrap();
        assert_eq!(info.generation, 1, "resume advances the process generation");
        assert_eq!(
            fixture.runtime.binding_snapshot().process_generation,
            1,
            "binding generation stays in lock-step with the supervisor"
        );
        assert_eq!(
            fixture.runtime.binding_snapshot().status,
            HarnessBindingStatus::Resuming
        );

        // The resumed init carries a fresh uuid (the vendor replays
        // system/init after a restart); it re-binds the session and
        // moves the binding back to Running.
        fixture
            .scripted
            .lines
            .lock()
            .unwrap()
            .push_back(init_event_with_uuid("evt-resume"));
        fixture.runtime.next_event().unwrap().unwrap();
        let binding = fixture.runtime.binding_snapshot();
        assert_eq!(
            binding.vendor_session_id.as_deref(),
            Some(VENDOR_SESSION_ID)
        );
        assert_eq!(binding.status, HarnessBindingStatus::Running);

        // A resume launch without the flag pair is refused.
        fixture.runtime.stop().unwrap();
        let plain = claude_launch_for_workspace(&fixture.workspace);
        let error = fixture
            .runtime
            .resume(&plain, VENDOR_SESSION_ID)
            .unwrap_err();
        assert!(error.to_string().contains("--resume"));
    }

    #[test]
    fn progress_events_are_normalized_from_parsed_vendor_events() {
        let mut fixture = open_fixture(vec![
            init_event(),
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"hello"}}}"#.to_string(),
            // Presentation bookkeeping is consumed without projection.
            r#"{"type":"system","subtype":"hook_started","session_id":"ignored"}"#.to_string(),
            r#"{"type":"result","subtype":"success"}"#.to_string(),
        ]);
        fixture
            .runtime
            .start(&claude_launch_for_workspace(&fixture.workspace))
            .unwrap();

        let first = fixture.runtime.next_event().unwrap().unwrap();
        assert!(matches!(
            first.kind,
            NormalizedEventKind::SessionStarted { .. }
        ));
        let second = fixture.runtime.next_event().unwrap().unwrap();
        assert!(matches!(
            second.kind,
            NormalizedEventKind::AssistantDelta { .. }
        ));
        // The hook_started frame is consumed inside the same call, so
        // the third call lands directly on the terminal result.
        let third = fixture.runtime.next_event().unwrap().unwrap();
        assert!(matches!(third.kind, NormalizedEventKind::TurnCompleted));

        let kinds = drain_progress(&mut fixture);
        assert!(kinds.contains(&HarnessProgressKind::SessionBound {
            vendor_session_id: VENDOR_SESSION_ID.to_string()
        }));
        assert!(kinds.contains(&HarnessProgressKind::AssistantDelta {
            text: "hello".to_string()
        }));
        assert!(kinds.contains(&HarnessProgressKind::TurnCompleted));
    }

    #[test]
    fn duplicate_vendor_events_are_admitted_exactly_once() {
        let line = init_event_with_uuid("evt-1");
        let mut fixture = open_fixture(vec![
            line.clone(),
            line,
            r#"{"type":"result","subtype":"success"}"#.to_string(),
        ]);
        fixture
            .runtime
            .start(&claude_launch_for_workspace(&fixture.workspace))
            .unwrap();

        let first = fixture.runtime.next_event().unwrap().unwrap();
        assert!(matches!(
            first.kind,
            NormalizedEventKind::SessionStarted { .. }
        ));
        // The replayed init is admitted exactly once: the next event
        // is the terminal result, not a second SessionStarted.
        let second = fixture.runtime.next_event().unwrap().unwrap();
        assert!(matches!(second.kind, NormalizedEventKind::TurnCompleted));
        let binding = fixture.runtime.binding_snapshot();
        assert_eq!(binding.duplicate_events_dropped, 1);
        assert_eq!(binding.status, HarnessBindingStatus::Running);
        let persisted = fixture.store.load("binding-root").unwrap().unwrap();
        assert_eq!(persisted.duplicate_events_dropped, 1);
    }

    #[test]
    fn native_child_spawn_tool_call_is_counted_and_projected_as_bypass() {
        let task_call = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"call-1","name":"Task","input":{"prompt":"spawn a child"}}]}}"#;
        let mut fixture = open_fixture(vec![init_event(), task_call.to_string()]);
        fixture
            .runtime
            .start(&claude_launch_for_workspace(&fixture.workspace))
            .unwrap();
        fixture.runtime.next_event().unwrap().unwrap();

        let event = fixture.runtime.next_event().unwrap().unwrap();
        assert!(matches!(
            event.kind,
            NormalizedEventKind::ToolCallStarted { ref tool_name, .. } if tool_name == "Task"
        ));
        let binding = fixture.runtime.binding_snapshot();
        assert_eq!(
            binding.native_child_spawn_violations, 1,
            "the bypass attempt is counted on the persisted binding"
        );
        let persisted = fixture.store.load("binding-root").unwrap().unwrap();
        assert_eq!(persisted.native_child_spawn_violations, 1);
        let kinds = drain_progress(&mut fixture);
        assert!(
            kinds.contains(&HarnessProgressKind::DelegationBypassDetected {
                tool_name: "Task".to_string(),
                call_id: "call-1".to_string()
            })
        );
    }

    #[test]
    fn vendor_eof_fails_the_binding_and_projects_process_exited() {
        let mut fixture = open_fixture(vec![]);
        fixture
            .runtime
            .start(&claude_launch_for_workspace(&fixture.workspace))
            .unwrap();
        // No events queued: the first read hits EOF.
        assert!(fixture.runtime.next_event().unwrap().is_none());
        assert_eq!(
            fixture.runtime.binding_snapshot().status,
            HarnessBindingStatus::Failed
        );
        let kinds = drain_progress(&mut fixture);
        assert!(kinds.contains(&HarnessProgressKind::ProcessExited));
        // Steering now rejects (status is Failed, not Running).
        assert!(matches!(
            fixture.runtime.steer("gone").unwrap_err(),
            HarnessError::SteerRejected(_)
        ));
    }

    #[test]
    fn steer_and_interrupt_frames_match_the_donor_contract() {
        let frame: Value = serde_json::from_str(&steer_frame("hi")).unwrap();
        assert_eq!(frame["type"], "user");
        assert_eq!(frame["session_id"], "");
        assert_eq!(frame["message"]["role"], "user");
        assert!(frame["parent_tool_use_id"].is_null());
        let interrupt: Value = serde_json::from_str(&interrupt_frame()).unwrap();
        assert_eq!(interrupt["type"], "control_request");
        assert_eq!(interrupt["request"]["subtype"], "interrupt");
    }

    #[test]
    fn progress_projection_maps_every_normalized_kind() {
        let cases: Vec<(NormalizedEventKind, usize)> = vec![
            (
                NormalizedEventKind::SessionStarted {
                    vendor_session_id: "s".into(),
                    init: None,
                },
                1,
            ),
            (
                NormalizedEventKind::TurnStarted {
                    vendor_turn_id: "t".into(),
                },
                1,
            ),
            (NormalizedEventKind::AssistantDelta { text: "x".into() }, 1),
            (
                NormalizedEventKind::AssistantCompleted { text: "x".into() },
                1,
            ),
            (
                NormalizedEventKind::ToolCallStarted {
                    call_id: "c".into(),
                    tool_name: "mcp__rightkit".into(),
                    arguments: Value::Null,
                },
                1,
            ),
            (
                NormalizedEventKind::ToolCallStarted {
                    call_id: "c".into(),
                    tool_name: "Task".into(),
                    arguments: Value::Null,
                },
                2,
            ),
            (
                NormalizedEventKind::ToolResult {
                    call_id: "c".into(),
                    is_error: true,
                    content: Value::Null,
                },
                1,
            ),
            (
                NormalizedEventKind::ApprovalRequested {
                    request_id: "r".into(),
                    action: "shell".into(),
                    details: Value::Null,
                },
                1,
            ),
            (NormalizedEventKind::TurnCompleted, 1),
            (NormalizedEventKind::TurnInterrupted, 1),
            (
                NormalizedEventKind::TurnFailed {
                    message: "boom".into(),
                },
                1,
            ),
            (NormalizedEventKind::GatewayReady, 1),
        ];
        for (kind, expected) in cases {
            assert_eq!(
                project_progress_kinds(&kind).len(),
                expected,
                "projection count mismatch for {kind:?}"
            );
        }
    }
}
