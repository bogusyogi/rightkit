use super::*;
#[path = "engine_tool_dispatch_concurrent.rs"]
mod concurrent;
#[path = "engine_tool_dispatch_single.rs"]
mod single;
#[path = "engine_tool_dispatch_approval.rs"]
mod single_approval;
#[path = "engine_tool_dispatch_execute.rs"]
mod single_execute;
#[path = "engine_tool_dispatch_result.rs"]
mod single_result;

/// Default cap on how many tool calls `dispatch_concurrent_batch` is allowed
/// to run in flight at once for a single chunk (Lane 5 / P1-8 §8-1, mirroring
/// claude-code's `toolOrchestration.ts` bound). Overridable via
/// `RIGHTKIT_MAX_CONCURRENT_TOOL_CALLS` for operators who need a tighter or
/// looser ceiling than the default — same pattern as
/// `compactor_breaker::new_from_env_or_default`.
pub(super) const DEFAULT_MAX_CONCURRENT_TOOL_CALLS: usize = 10;
const MAX_CONCURRENT_TOOL_CALLS_ENV: &str = "RIGHTKIT_MAX_CONCURRENT_TOOL_CALLS";

#[cfg(feature = "host")]
type ProcessLaunchAuthority = (
    std::sync::Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter>,
    rightkit_sandbox::execution::ProcessLaunchContext,
);

#[cfg(feature = "host")]
fn required_process_launch_authority(
    tools: &rightkit_agent_tools::coding_tools::ToolRegistry,
    tool_name: &str,
    args: &serde_json::Value,
    workspace_root: &std::path::Path,
) -> Result<Option<ProcessLaunchAuthority>, rightkit_agent_tools::coding_tools::ToolResult> {
    let Some(tool) = tools.get(tool_name) else {
        return Ok(None);
    };
    let Some(kind) = tool.process_resource_kind_for(args) else {
        return Ok(None);
    };
    let governor = rightkit_agent_tools::coding_tools::process_resource_authority::require_process_resource_governor()
        .map_err(|error| rightkit_agent_tools::coding_tools::ToolResult::error(error.to_string()))?;
    if !governor.capability_available(kind) {
        return Err(rightkit_agent_tools::coding_tools::ToolResult::error(
            format!(
                "{}: tool '{}' requires unavailable resource {kind:?}",
                rightkit_sandbox::execution::EffectExecutionError::ResourceLimitsMissing,
                tool_name
            ),
        ));
    }
    let launch_context = rightkit_sandbox::execution::ProcessLaunchContext {
        workspace_root: workspace_root.display().to_string(),
        sandbox_profile: None,
        sandbox_required: false,
        network_required: false,
    };
    let adapter: std::sync::Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter> =
        std::sync::Arc::new(
            rightkit_agent_tools::coding_tools::process_resource_authority::GovernedProcessLaunchAdapter::new(
                rightkit_agent_tools::coding_tools::shell::NativeCommandSandboxAdapter,
                governor,
                kind,
                matches!(
                    kind,
                    rightkit_agent_tools::coding_tools::process_resource_authority::ResourceKind::ChildProcess
                ),
            ),
        );
    Ok(Some((adapter, launch_context)))
}

/// Resolve the concurrent-tool-call bound: `RIGHTKIT_MAX_CONCURRENT_TOOL_CALLS`
/// when set to a parseable value greater than zero, else
/// `DEFAULT_MAX_CONCURRENT_TOOL_CALLS`. A batch larger than this bound must be
/// chunked by the caller — `dispatch_concurrent_batch` itself runs whatever
/// slice it is given with no internal limit, so the bound is enforced at the
/// partition/call site (`engine_loop_dispatch.rs`), not inside the executor.
pub(super) fn max_concurrent_tool_calls() -> usize {
    std::env::var(MAX_CONCURRENT_TOOL_CALLS_ENV)
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_MAX_CONCURRENT_TOOL_CALLS)
}

// Interactive terminals are operator-owned sessions (`api_execution`
// spawns them with no cancel receiver); turn cancellation must never
// reach them regardless of when they were created.
const INTERACTIVE_TERMINAL_PROCESS_KIND: &str = "terminalSession";

/// Per-session baselines of background-shell ids that predate the current
/// turn, used by `propagate_cancel_to_background_shells` to keep earlier-turn
/// work (and operator terminals) out of turn-cancellation scope. Keyed by
/// session id; the stored run id is the engine's `active_run_id` at baseline
/// time, so a new turn resets the baseline to the then-current store contents.
type ProtectedShellBaseline = (String, std::collections::BTreeSet<String>);
type ProtectedShellBaselines = std::sync::Mutex<HashMap<String, ProtectedShellBaseline>>;
static PROTECTED_SHELL_BASELINES: std::sync::LazyLock<ProtectedShellBaselines> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

/// Completion ids already delivered into a session's async inbox. Several
/// tool calls can be active while one background shell exits; delivery must
/// stay exactly once even when their lifecycle watchers overlap.
static BACKGROUND_INBOX_DELIVERIES: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashSet<(String, String)>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));

/// Byte limit under which a tool result stays fully inline in the
/// transcript; at or under this limit `store_tool_output` records the
/// content verbatim with an inline envelope, over it the result is
/// externalized immediately to preview + retrievable reference. Only the
/// exempt class bypasses size decay (CR-28/CQ-07).
pub(super) const TOOL_OUTPUT_PREVIEW_LIMIT: usize = 4000;

/// Exempt-class (control-flow) results stay verbatim inline only up to this
/// many bytes. Beyond it they spill to the tool-result store like any other
/// large result, so an unbounded exempt payload cannot defeat compaction.
pub(super) const EXEMPT_TOOL_OUTPUT_INLINE_CAP: usize = 64 * 1024;

/// Per-session baselines of the run id whose tool results were last recorded,
/// used by `externalize_prior_turn_inline_outputs`: transcripts replayed from
/// sessions recorded under the pre-CQ-07 policy can still carry oversized
/// `tool_output_inline` envelopes, and the first result record of a new run
/// decays them through the regular store. Under the current policy no new
/// oversized inline envelope is ever written, so for fresh transcripts the
/// pass is a no-op — it exists to keep the decay invariant across the
/// upgrade seam. Keyed by session id; the stored value is the engine's
/// `active_run_id` at the last result record.
/// Same shape and lifetime trade-off as `PROTECTED_SHELL_BASELINES`: the
/// small per-session entry persists for the daemon's lifetime.
static EXTERNALIZATION_TURN_BASELINES: std::sync::LazyLock<
    std::sync::Mutex<HashMap<String, String>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

/// Free session-keyed dispatch bookkeeping when a session retires. In
/// particular, completed-shell de-duplication must not retain dead session
/// ids after its inbox producer has been invalidated.
pub(super) fn retire_session_async_state(session_id: &str) {
    PROTECTED_SHELL_BASELINES.lock().unwrap().remove(session_id);
    BACKGROUND_INBOX_DELIVERIES
        .lock()
        .unwrap()
        .retain(|(stored_session_id, _)| stored_session_id != session_id);
    EXTERNALIZATION_TURN_BASELINES
        .lock()
        .unwrap()
        .remove(session_id);
}

impl SessionEngine {
    /// Execute a batch of independent tool calls concurrently, then record all
    /// results in order. Callers must pre-gate the batch to calls that are safe
    /// to run in parallel — read-only calls, or auto-allowed WriteFile edits to
    /// pairwise-disjoint paths (see `parallel_write_prefix`) — and must chunk
    /// any batch larger than `max_concurrent_tool_calls()` via
    /// `dispatch_concurrent_batch_bounded` so no more than the configured
    /// ceiling of calls is ever in flight at once.
    pub(super) fn enforce_worker_tool_limit(&mut self, response: &mut crate::llm::LlmResponse) {
        if self.max_worker_tool_calls == 0 || response.tool_calls.is_empty() {
            return;
        }

        let mut seen_worker_calls = 0usize;
        let mut dropped: Vec<ToolCall> = Vec::new();
        response.tool_calls.retain(|call| {
            if !is_worker_spawning_tool_call(call) {
                return true;
            }
            seen_worker_calls += 1;
            if seen_worker_calls <= self.max_worker_tool_calls {
                true
            } else {
                dropped.push(call.clone());
                false
            }
        });

        if dropped.is_empty() {
            return;
        }

        let dropped_names = dropped
            .iter()
            .map(|call| call.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let warning = format!(
            "[Subagent limit: accepted at most {} worker-spawning tool call{} this turn; truncated {} over-limit call{}: {}.]",
            self.max_worker_tool_calls,
            if self.max_worker_tool_calls == 1 { "" } else { "s" },
            dropped.len(),
            if dropped.len() == 1 { "" } else { "s" },
            dropped_names
        );
        if response.text.trim().is_empty() {
            response.text = warning.clone();
        } else {
            response.text.push_str("\n\n");
            response.text.push_str(&warning);
        }

        let accepted_count = seen_worker_calls.saturating_sub(dropped.len());
        let mut fields = HashMap::new();
        fields.insert("limit".to_string(), self.max_worker_tool_calls.to_string());
        fields.insert(
            "totalWorkerCalls".to_string(),
            seen_worker_calls.to_string(),
        );
        fields.insert("acceptedCount".to_string(), accepted_count.to_string());
        fields.insert("droppedCount".to_string(), dropped.len().to_string());
        fields.insert(
            "droppedToolCallIds".to_string(),
            dropped
                .iter()
                .map(|call| call.id.as_str())
                .collect::<Vec<_>>()
                .join(","),
        );
        fields.insert("droppedTools".to_string(), dropped_names);
        self.emit(|seq| ProtocolEvent::OrchestrationObservationRecorded {
            session_id: self.session_id.clone(),
            seq,
            created_at: now_iso(),
            observation: rightkit_agent_engine_protocol::OrchestrationObservation {
                id: format!("worker-limit-{seq}"),
                kind: "worker_tool_limit".into(),
                subject: self.session_id.clone(),
                summary: warning,
                fields,
                created_at: now_iso(),
            },
        });
    }

    /// Ask the tool for a non-mutating preview of the change it would make, to
    /// show in the approval card. Best-effort: any failure yields no preview.
    pub(super) async fn tool_preview(&self, call: &ToolCall) -> Option<String> {
        let tool = self.tools.get(&call.name)?;
        let args: serde_json::Value = serde_json::from_str(&call.arguments_json).ok()?;
        let ctx = ToolContext::new(
            &self.session_id,
            &self.workspace_root,
            self.permission_mode,
            self.workspace_trust,
        )
        .with_content_provenance(self.content_provenance.clone())
        .with_file_service(self.file_service.clone())
        .with_mcp_channel(self.mcp_channel.clone())
        .with_tool_result_dir(self.tool_result_dir.clone())
        .with_skill_dirs(self.skill_dirs.clone())
        .with_path_deny(self.deny_globs.clone());
        tool.preview(&args, &ctx).await
    }

    /// True when any of the last few tool results carried untrusted/remote
    /// content — a signal passed to the guardian reviewer, not a verdict.
    /// Looked back over a small fixed window so the check stays cheap.
    const GUARDIAN_UNTRUSTED_LOOKBACK: usize = 8;

    pub(super) fn recent_untrusted_context(&self) -> bool {
        self.transcript
            .messages()
            .iter()
            .rev()
            .take(Self::GUARDIAN_UNTRUSTED_LOOKBACK)
            .any(|m| match m {
                rightkit_agent_engine_protocol::TranscriptMessage::Tool {
                    trust: Some(t), ..
                } => t.wraps_untrusted_content(),
                _ => false,
            })
    }

    pub(super) fn observe_tool_result_provenance(&mut self, tool_name: &str) {
        if !tool_result_carries_content(tool_name) {
            return;
        }
        self.content_provenance
            .absorb(tool_source_provenance(tool_name));
    }

    /// Approval review for one mutating tool call (FM-031), run before the
    /// normal permission/approval gate. Every outcome emits a durable
    /// `ToolApprovalReviewed` verdict carrying the call's correlation id:
    /// - `unavailable`: no reviewer is configured. Nothing is auto-approved;
    ///   the normal approval gate still asks the operator (fail-closed default).
    /// - `allow`: the reviewer echoed this exact request identity. A review
    ///   never grants authority; the normal approval gate still applies.
    /// - `block`: the reviewer blocked, errored, or answered with an
    ///   uncorrelated or mismatched verdict. The call is denied.
    ///
    /// Read-only calls never reach this (callers gate on `!tool.is_read_only()`
    /// before calling). Returns `Some(reason)` only when the call must be denied.
    pub(super) async fn guardian_review(&self, call: &ToolCall) -> Option<String> {
        let correlation =
            rightkit_agent_policy::denial_tracking::correlation_token(&self.session_id, &call.id);
        let Some(guardian) = self.guardian.as_ref() else {
            self.emit_tool_approval_reviewed(
                call,
                &correlation,
                "unavailable",
                "no approval reviewer configured; human approval required",
                false,
            );
            return None;
        };
        if self.active_run_id.trim().is_empty() {
            let reason = "guardian review unavailable: active run identity is missing".to_string();
            self.emit_tool_approval_reviewed(call, &correlation, "block", &reason, true);
            return Some(reason);
        }
        let request = GuardianRequest::from_call_with_run(
            &self.session_id,
            &self.active_run_id,
            call,
            self.recent_untrusted_context(),
        );
        let mut lifecycle = GuardianReviewLifecycle::begin(&request);
        lifecycle.reviewing();
        let (verdict, reason) = match guardian.review(&request).await {
            Ok(verdict) if request.allows(&verdict) => {
                lifecycle.allowed();
                (
                    "allow",
                    "reviewer allowed; normal approval still applies".to_string(),
                )
            }
            Ok(GuardianVerdict::Allow) => {
                lifecycle.failed();
                (
                    "block",
                    "guardian reviewer returned uncorrelated allow; denying fail-closed"
                        .to_string(),
                )
            }
            Ok(GuardianVerdict::AllowFor { .. }) => {
                lifecycle.failed();
                (
                    "block",
                    "guardian reviewer returned mismatched request identity; denying fail-closed"
                        .to_string(),
                )
            }
            Ok(GuardianVerdict::Block(reason)) => {
                lifecycle.blocked();
                ("block", reason)
            }
            Err(err) => {
                lifecycle.failed();
                tracing::warn!(
                    session_id = %self.session_id,
                    tool_call_id = %call.id,
                    tool = %call.name,
                    error = %err,
                    "guardian reviewer failed; failing closed"
                );
                ("block", format!("guardian reviewer failed: {err}"))
            }
        };
        self.emit_tool_approval_reviewed(call, &correlation, verdict, &reason, true);
        (verdict == "block").then_some(reason)
    }

    /// Emit the durable approval-review verdict for one tool call.
    fn emit_tool_approval_reviewed(
        &self,
        call: &ToolCall,
        correlation: &str,
        verdict: &str,
        reason: &str,
        reviewer_configured: bool,
    ) {
        let session_id = self.session_id.clone();
        let tool_call_id = call.id.clone();
        let tool_name = call.name.clone();
        let correlation_id = correlation.to_string();
        let verdict = verdict.to_string();
        let reason = reason.to_string();
        self.emit(|seq| ProtocolEvent::ToolApprovalReviewed {
            session_id,
            seq,
            created_at: now_iso(),
            tool_call_id,
            tool_name,
            correlation_id,
            verdict,
            reason,
            reviewer_configured,
        });
    }

    pub(super) fn can_dispatch_readonly_fast(&self, call: &&ToolCall) -> bool {
        let pre_tool_middleware = self
            .enabled_runtime_middleware(MiddlewarePhase::PreToolCall)
            .into_iter()
            .filter(|definition| {
                definition.source
                    == rightkit_agent_engine_runtime_middleware::MiddlewareSource::LocalPlugin
            })
            .collect::<Vec<_>>();
        let assurance_ok = self
            .assurance_broker
            .as_ref()
            .and_then(|broker| broker.lock().ok())
            .map(|broker| {
                broker
                    .safety_gate(&self.assurance_scope_for_call(call))
                    .allowed
            })
            .unwrap_or(true);
        self.tools
            .get(&call.name)
            .map(|t| t.is_read_only())
            .unwrap_or(false)
            && call.name != "exec"
            && self.hook_bus.is_none()
            // A repeated call is an Ask, so it never takes the no-approval fast path.
            && !self.repeated_call_ids.contains(&call.id)
            && self.permission_verdict_for(call).decision == PermissionDecision::Allow
            && !pre_tool_middleware_matches_call(&pre_tool_middleware, call)
            && assurance_ok
    }

    /// Wire cancellation through to background shell work spawned under this
    /// session (CR-15).
    ///
    /// Registration happens once per dispatched tool call, before the tool
    /// runs; the caller in `engine_tool_dispatch_execute` hands us the call's
    /// own cancel receiver. Three mechanisms cooperate:
    ///
    /// 1. **Turn baseline.** Every background-shell id already in this
    ///    session's store when a turn's first tool call dispatches is
    ///    recorded as protected (`PROTECTED_SHELL_BASELINES`, keyed by
    ///    session id and reset whenever `active_run_id` changes at a
    ///    registration, i.e. at each new turn that dispatches tools).
    ///    Shells started by *earlier* turns — an intentional cross-turn dev
    ///    server, an operator terminal — are never cancelled by turn
    ///    teardown; only work started during the current turn is.
    ///    Limitation: a turn that dispatches no tool calls never registers,
    ///    so the baseline then still reflects the last tool-dispatching
    ///    turn; the seam cannot see tool-less turns.
    /// 2. **Parked watcher.** Each registration spawns a one-shot task that
    ///    waits for either the call's own cancel token to flip `true` (a
    ///    targeted `/tool-invocations/:id/cancel`) or the session interrupt
    ///    to fire (an operator stop of the turn). On trigger it terminates
    ///    every unprotected running shell in the session's store.
    /// 3. **Group termination.** Killing goes through `BgShell::kill()`, so
    ///    the spawn supervisor terminates the owned tree (RightKit `OwnedCommand`): SIGTERM
    ///    to the whole process group (setsid-owned on Unix, Job Object on
    ///    Windows), a bounded grace wait, then SIGKILL. Never matched or
    ///    searched by name. `kill()` takes its oneshot sender, so repeated
    ///    triggers are no-ops, and the send returns without waiting for the
    ///    target to honor the signal — escalation is bounded inside the
    ///    supervisor, never on the cancel path.
    ///
    /// Known trade-offs, stated plainly: one parked task per dispatched call
    /// lives until it fires or the session's channels close, and the small
    /// per-session baseline entry persists for the daemon's lifetime. The
    /// cancellation unit is the TURN, not the individual call: a targeted
    /// `/tool-invocations/:id/cancel` therefore terminates all current-turn
    /// background shells, not only that call's own spawns. The watcher cannot
    /// emit through `&mut self`, so it publishes its terminal disposition
    /// directly onto the conduit as an orchestration observation.
    /// No shells can exist on a host that cannot spawn processes, so
    /// cancellation has nothing to reach. Keeping the signature lets the two
    /// call sites stay free of `#[cfg]`.
    #[cfg(not(feature = "host"))]
    pub(super) fn propagate_cancel_to_background_shells(
        &self,
        _tool_call_id: &str,
        _cancel_rx: tokio::sync::watch::Receiver<bool>,
    ) {
    }

    #[cfg(feature = "host")]
    pub(super) fn propagate_cancel_to_background_shells(
        &self,
        tool_call_id: &str,
        mut cancel_rx: tokio::sync::watch::Receiver<bool>,
    ) {
        // Reactor-safe: this is reached from synchronous construction paths
        // too (the mobile runtime installs its session-lifetime interrupt
        // watcher while building the engine on a plain thread), and a bare
        // `tokio::spawn` with no reactor panics -- inside the FFI's
        // `extern "C"` frame that panic cannot unwind and aborts the whole
        // process. Without a reactor no background shell can have been
        // spawned and none can be, so skipping the watchers loses nothing.
        let Ok(reactor) = tokio::runtime::Handle::try_current() else {
            tracing::debug!(
                tool_call_id,
                "no async runtime in context; skipping background-shell cancel watchers"
            );
            return;
        };
        let _entered = reactor.enter();
        let pre_call_ids: std::collections::BTreeSet<String> = self
            .background_shells
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let inbox_baseline = pre_call_ids.clone();
        {
            let mut baselines = PROTECTED_SHELL_BASELINES.lock().unwrap();
            let run_id = self.active_run_id().to_string();
            let same_run = baselines
                .get(&self.session_id)
                .is_some_and(|(stored_run, _)| stored_run == &run_id);
            // Same turn: keep the turn-start baseline so shells spawned by
            // earlier calls of THIS turn stay cancellable. A new turn (or
            // first observed call) protects everything already in the store.
            if !same_run {
                baselines.insert(self.session_id.clone(), (run_id, pre_call_ids));
            }
        }

        let session_id = self.session_id.clone();
        let call_id = tool_call_id.to_string();
        let store = self.background_shells.clone();
        let conduit = self.conduit.clone();
        let interrupt_rx = self.interrupt_rx.clone();

        // A shell launched during this call belongs to this session, but its
        // terminal state occurs outside the streaming loop. Wait for a newly
        // inserted process row, then emit its one terminal payload into the
        // session inbox. Calls that do not launch a shell leave after a short
        // discovery window; calls that do remain until that shell is terminal.
        let inbox_session_id = self.session_id.clone();
        let inbox_store = self.background_shells.clone();
        let inbox_producer = self.async_inbox_producer.clone();
        let inbox_conduit = self.conduit.clone();
        // Event-driven: subscribe before the first scan and mark the version
        // seen at the top of every pass, so a change landing between a scan
        // and the wait below is never missed and an idle call parks instead
        // of waking on a timer.
        let mut shell_events =
            rightkit_agent_tools::coding_tools::background_shell::subscribe_shell_events();
        tokio::spawn(async move {
            let mut observed_new_shell = false;
            // Calls that launch no shell leave after this discovery window.
            let discovery_deadline =
                tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                shell_events.borrow_and_update();
                let (completed, has_running_shell) = {
                    let shells = inbox_store.lock().unwrap();
                    let mut completed = Vec::new();
                    let mut has_running_shell = false;
                    for (shell_id, shell) in shells.iter() {
                        if inbox_baseline.contains(shell_id) {
                            continue;
                        }
                        observed_new_shell = true;
                        let status = match shell.status() {
                            rightkit_agent_tools::coding_tools::background_shell::BgStatus::Running => {
                                has_running_shell = true;
                                continue;
                            }
                            rightkit_agent_tools::coding_tools::background_shell::BgStatus::Exited(code) => {
                                format!("exited ({code})")
                            }
                            rightkit_agent_tools::coding_tools::background_shell::BgStatus::Killed => {
                                "cancelled".to_string()
                            }
                            rightkit_agent_tools::coding_tools::background_shell::BgStatus::SpawnFailed(error) => {
                                format!("spawn_failed: {error}")
                            }
                            rightkit_agent_tools::coding_tools::background_shell::BgStatus::Unknown => {
                                "unknown (restart reconciliation required)".to_string()
                            }
                        };
                        completed.push((
                            shell_id.clone(),
                            status,
                            shell.output_snapshot(),
                            shell.exit_status(),
                            shell.command.clone(),
                        ));
                    }
                    (completed, has_running_shell)
                };
                for (shell_id, status, output, exit, command) in completed {
                    let fresh = BACKGROUND_INBOX_DELIVERIES
                        .lock()
                        .unwrap()
                        .insert((inbox_session_id.clone(), shell_id.clone()));
                    if fresh {
                        // Typed completion notification: emitted once per shell
                        // (same dedupe as the inbox message) after the child
                        // was reaped, so consumers need not poll.
                        if let Some(exit) = exit {
                            inbox_conduit.emit(|seq| {
                                rightkit_agent_engine_protocol::ProtocolEvent::BackgroundShellExited {
                                    session_id: inbox_session_id.clone(),
                                    seq,
                                    created_at: now_iso(),
                                    shell_id: shell_id.clone(),
                                    command: command.clone(),
                                    status: exit,
                                    output_bytes: output.len() as u64,
                                }
                            });
                        }
                        if let Some(message_id) = inbox_producer.push(
                            "background_shell",
                            format!("Background shell '{shell_id}' {status}."),
                            Some(serde_json::json!({
                                "processId": shell_id.clone(),
                                "status": status.clone(),
                                "output": output,
                            })),
                        ) {
                            let seq = inbox_conduit.next_seq();
                            inbox_conduit.publish(
                                rightkit_agent_engine_protocol::ProtocolEvent::OrchestrationObservationRecorded {
                                    session_id: inbox_session_id.clone(),
                                    seq,
                                    created_at: now_iso(),
                                    observation: rightkit_agent_engine_protocol::OrchestrationObservation {
                                        id: format!("background-shell-inbox-{shell_id}"),
                                        kind: "async_inbox_message".to_string(),
                                        subject: inbox_session_id.clone(),
                                        summary: format!("Background shell '{shell_id}' {status}; inbox message queued."),
                                        fields: std::collections::HashMap::from([
                                            ("channel".to_string(), "background_shell".to_string()),
                                            ("inboxMessageId".to_string(), message_id.to_string()),
                                            ("processId".to_string(), shell_id),
                                        ]),
                                        created_at: now_iso(),
                                    },
                                },
                            );
                        }
                    }
                }
                if !observed_new_shell {
                    match tokio::time::timeout_at(discovery_deadline, shell_events.changed()).await
                    {
                        Ok(Ok(())) => continue,
                        _ => break,
                    }
                }
                if has_running_shell {
                    if shell_events.changed().await.is_err() {
                        break;
                    }
                    continue;
                }
                break;
            }
        });

        tokio::spawn(async move {
            #[derive(Debug)]
            enum CancelTrigger {
                ToolCallCancelled,
                TurnInterrupted,
            }

            let trigger = if let Some(mut rx) = interrupt_rx {
                let wait_for_call_cancel = async move {
                    if *cancel_rx.borrow() {
                        return;
                    }
                    loop {
                        match cancel_rx.changed().await {
                            Ok(()) if *cancel_rx.borrow() => return,
                            Ok(()) => {}
                            Err(_) => std::future::pending::<()>().await,
                        }
                    }
                };
                let wait_for_interrupt = async move {
                    if *rx.borrow() {
                        return;
                    }
                    loop {
                        match rx.changed().await {
                            Ok(()) if *rx.borrow() => return,
                            Ok(()) => {}
                            Err(_) => std::future::pending::<()>().await,
                        }
                    }
                };
                tokio::select! {
                    () = wait_for_call_cancel => CancelTrigger::ToolCallCancelled,
                    () = wait_for_interrupt => CancelTrigger::TurnInterrupted,
                }
            } else {
                if !*cancel_rx.borrow() {
                    loop {
                        match cancel_rx.changed().await {
                            Ok(()) if *cancel_rx.borrow() => break,
                            Ok(()) => {}
                            Err(_) => return,
                        }
                    }
                }
                CancelTrigger::ToolCallCancelled
            };

            let baseline = PROTECTED_SHELL_BASELINES
                .lock()
                .unwrap()
                .get(&session_id)
                .map(|(_, ids)| ids.clone())
                .unwrap_or_default();
            let victims: Vec<(
                String,
                Arc<rightkit_agent_tools::coding_tools::background_shell::BgShell>,
            )> = {
                let shells = store.lock().unwrap();
                shells
                    .iter()
                    .filter(|(id, shell)| {
                        !baseline.contains(id.as_str())
                            && shell.process_kind() != INTERACTIVE_TERMINAL_PROCESS_KIND
                            && shell.status()
                                == rightkit_agent_tools::coding_tools::background_shell::BgStatus::Running
                    })
                    .map(|(id, shell)| (id.clone(), shell.clone()))
                    .collect()
            };
            // Idempotent and non-blocking: each kill() takes the shell's
            // oneshot sender; the spawn supervisor escalates TERM → KILL on a
            // bounded grace period off the cancel path.
            let killed_ids: Vec<String> = victims
                .iter()
                .filter(|(_, shell)| shell.kill())
                .map(|(id, _)| id.clone())
                .collect();

            let seq = conduit.next_seq();
            let mut fields = HashMap::new();
            fields.insert("toolCallId".to_string(), call_id.clone());
            fields.insert(
                "trigger".to_string(),
                match trigger {
                    CancelTrigger::ToolCallCancelled => "toolCallCancelled".to_string(),
                    CancelTrigger::TurnInterrupted => "turnInterrupted".to_string(),
                },
            );
            fields.insert("victims".to_string(), victims.len().to_string());
            fields.insert("killedShellIds".to_string(), killed_ids.join(","));
            conduit.publish(
                rightkit_agent_engine_protocol::ProtocolEvent::OrchestrationObservationRecorded {
                    session_id: session_id.clone(),
                    seq,
                    created_at: now_iso(),
                    observation: rightkit_agent_engine_protocol::OrchestrationObservation {
                        id: format!("background-shell-cancel-{seq}"),
                        kind: "background_shell_cancel".into(),
                        subject: session_id.clone(),
                        summary: format!(
                            "Cancellation ({trigger:?}) reached {} background \
                             shell(s); terminated: {}",
                            victims.len(),
                            if killed_ids.is_empty() {
                                "none".to_string()
                            } else {
                                killed_ids.join(", ")
                            },
                        ),
                        fields,
                        created_at: now_iso(),
                    },
                },
            );
        });
    }

    pub(super) fn record_tool_error(
        &mut self,
        sid: &str,
        call: &ToolCall,
        summary: String,
        turn_results: &mut std::collections::HashMap<
            String,
            rightkit_agent_tools::coding_tools::ToolResult,
        >,
    ) {
        // Denied / middleware-blocked calls are surfaced to the engine-loop
        // observer as typed `is_error == true` results. The unbacked-
        // mutation check excludes errors by construction; we still record
        // them so the post-edit guard never carries a stale "successful"
        // entry for a denied call that failed later in the chain.
        turn_results.insert(
            call.id.clone(),
            rightkit_agent_tools::coding_tools::ToolResult::error(summary.clone()),
        );
        let (summary, mut output, _store_failed) =
            self.store_tool_output(&call.name, &summary, Some("text/plain; charset=utf-8"));
        if self.internal_composition_call_ids.contains(&call.id) {
            if let Some(output) = output.as_mut() {
                output.kind = rightkit_agent_engine_protocol::TOOL_OUTPUT_INTERNAL_KIND.into();
            }
        }
        self.transcript
            .push_tool_result_with_output(&call.id, &summary, output.clone(), true);
        let result_msg = rightkit_agent_engine_protocol::TranscriptMessage::Tool {
            id: "tr".into(),
            created_at: now_iso(),
            tool_call_id: call.id.clone(),
            summary: summary.clone(),
            output,
            trust: Some(tool_result_trust(&call.name, &summary)),
            is_error: true,
            pinned: false,
            process_id: None,
        };
        self.emit(|seq| ProtocolEvent::ToolResultAdded {
            session_id: sid.to_string(),
            seq,
            created_at: now_iso(),
            result: result_msg,
        });
    }

    /// Bounded front end for calls classified by the loop as read-only. The
    /// concurrent implementation performs governed preflight before it starts
    /// any future, then settles results through the engine-owned transcript.
    pub(super) async fn dispatch_concurrent_batch_bounded(
        &mut self,
        calls: &[&ToolCall],
        turn_results: &mut std::collections::HashMap<
            String,
            rightkit_agent_tools::coding_tools::ToolResult,
        >,
    ) {
        self.dispatch_concurrent_batch_with_bound(calls, max_concurrent_tool_calls(), turn_results)
            .await;
    }

    /// Same chunking behaviour as `dispatch_concurrent_batch_bounded`, but
    /// takes the bound as an explicit parameter instead of resolving it from
    /// the process environment. Exists so tests (and any future caller that
    /// needs a deterministic, non-global bound) can exercise the chunking
    /// logic without mutating `RIGHTKIT_MAX_CONCURRENT_TOOL_CALLS` — a
    /// process-global `std::env::set_var` in a test races every other test in
    /// the same binary. `dispatch_concurrent_batch_bounded` is the only
    /// production caller and always resolves the bound from the environment.
    pub(super) async fn dispatch_concurrent_batch_with_bound(
        &mut self,
        calls: &[&ToolCall],
        bound: usize,
        turn_results: &mut std::collections::HashMap<
            String,
            rightkit_agent_tools::coding_tools::ToolResult,
        >,
    ) {
        let bound = bound.max(1);
        for chunk in calls.chunks(bound) {
            self.dispatch_concurrent_batch(chunk, turn_results).await;
        }
    }

    /// Record a tool result into the transcript envelope, applying the single
    /// mandatory model-observation projection for built-ins and MCP (P1-6):
    ///
    /// - **Secret masking is mandatory for every result** — built-in, MCP,
    ///   or exempt — before the model ever sees it. Masking happens here,
    ///   once, via `mask_secrets`, so there is exactly one choke point and no
    ///   path can fork a second implementation.
    /// - **Exempt class** (`is_exempt_tool`): control-flow results bypass
    ///   *size decay only*, never masking. They stay verbatim/inline after
    ///   masking, because truncating a control-flow payload silently breaks it.
    /// - **Oversized results** (CQ-07): every non-exempt result over
    ///   `TOOL_OUTPUT_PREVIEW_LIMIT` is externalized at record time — the
    ///   model projection carries the bounded head+tail preview plus a
    ///   retrievable reference, while the full output lives in the result
    ///   store/disk corpus (execution data, not model projection). The
    ///   model recovers the bytes through the virtual path in the preview.
    ///   `externalize_prior_turn_inline_outputs` still runs first to decay
    ///   any oversized `tool_output_inline` envelopes replayed from
    ///   transcripts recorded under the pre-CQ-07 latest-turn policy.
    /// - **Store failure / oversize**: `tool_results.store` falls back to a
    ///   bounded head+tail inline preview when its write fails, so a failed
    ///   externalization keeps content (bounded) instead of discarding it.
    ///   The returned flag marks that typed failure so recorders set
    ///   `is_error` — a failed archive must not read as a clean observation.
    ///   No successful result silently loses unrecoverable bytes — every
    ///   truncated preview either carries a retrievable file reference (normal
    ///   bounded file-read) or an explicit not-archived marker when the safety
    ///   limit forbids storage, and the central projection guarantees the
    ///   markup is never omitted.
    ///
    /// Returns `(summary, output_envelope, store_failed)`.
    pub(super) fn store_tool_output(
        &mut self,
        tool_name: &str,
        content: &str,
        media_type: Option<&str>,
    ) -> (String, Option<OutputRef>, bool) {
        // Decay any oversized verbatim-inline envelopes replayed from a
        // pre-CQ-07 transcript before recording this result.
        self.externalize_prior_turn_inline_outputs();

        // P1-6: one mandatory masking choke point — built-ins and MCP share
        // the identical observation policy because every tool result, regardless
        // of origin, flows through this function.
        let masked = rightkit_agent_tools::coding_tools::output_compaction::mask_secrets(content);

        if crate::compaction_archive::is_exempt_tool_name(tool_name)
            && masked.len() <= EXEMPT_TOOL_OUTPUT_INLINE_CAP
        {
            return (
                masked.clone(),
                Some(verbatim_inline_output_ref(&masked, media_type)),
                false,
            );
        }
        // Exempt results only reach here when they exceed the inline cap
        // (audit M9): they spill with a larger preview than ordinary results.
        let preview_limit = if crate::compaction_archive::is_exempt_tool_name(tool_name) {
            EXEMPT_TOOL_OUTPUT_INLINE_CAP
        } else {
            TOOL_OUTPUT_PREVIEW_LIMIT
        };
        let stored = self.tool_results.store(&masked, preview_limit);
        let store_failed = matches!(
            stored.status,
            crate::tool_result_store::ToolResultStoreStatus::Failed(_)
        );
        let output = Some(output_ref_for(&masked, &stored, preview_limit, media_type));
        (stored.preview, output, store_failed)
    }

    /// Decay pass for oversized `tool_output_inline` envelopes that predate
    /// the current run (detected via `EXTERNALIZATION_TURN_BASELINES`, keyed
    /// like `PROTECTED_SHELL_BASELINES`). Under CQ-07 no new oversized inline
    /// envelope is written; this pass exists so transcripts replayed from
    /// sessions recorded under the pre-CQ-07 latest-turn policy still decay
    /// through the regular store — preview plus retrievable reference.
    /// Exempt-class results are never touched, already-externalized messages
    /// are skipped, and a store write failure keeps the bounded inline
    /// preview (`tool_result_store`'s fallback contract) — never discards
    /// content.
    fn externalize_prior_turn_inline_outputs(&mut self) {
        let run_id = self.active_run_id().to_string();
        {
            let mut baselines = EXTERNALIZATION_TURN_BASELINES.lock().unwrap();
            match baselines.get(&self.session_id) {
                // Same turn as the last recorded result: nothing to decay.
                Some(stored_run) if *stored_run == run_id => return,
                _ => {
                    baselines.insert(self.session_id.clone(), run_id);
                }
            }
        }

        let mut messages = self.transcript.messages().to_vec();
        // Map each tool_call_id to its tool name via the paired Assistant
        // message, so exempt-class results can be skipped.
        let mut tool_names: HashMap<String, String> = HashMap::new();
        for message in &messages {
            if let rightkit_agent_engine_protocol::TranscriptMessage::Assistant {
                tool_calls, ..
            } = message
            {
                for call in tool_calls {
                    tool_names.insert(call.id.clone(), call.name.clone());
                }
            }
        }
        let mut changed = false;
        for message in messages.iter_mut() {
            let rightkit_agent_engine_protocol::TranscriptMessage::Tool {
                tool_call_id,
                summary,
                output,
                is_error,
                ..
            } = message
            else {
                continue;
            };
            let exempt = tool_names
                .get(tool_call_id.as_str())
                .map(|name| crate::compaction_archive::is_exempt_tool_name(name))
                .unwrap_or(false);
            let Some(envelope) = output.as_ref() else {
                continue;
            };
            if exempt
                || envelope.retrievable
                || !is_decayable_tool_output_kind(&envelope.kind)
                || envelope.size_bytes as usize <= TOOL_OUTPUT_PREVIEW_LIMIT
            {
                continue;
            }
            // The full content lives inline in `summary` — exactly the
            // verbatim shape recorded for the turn that produced it.
            let full = summary.clone();
            let media_type = envelope.media_type.clone();
            let stored = self.tool_results.store(&full, TOOL_OUTPUT_PREVIEW_LIMIT);
            *summary = stored.preview.clone();
            // A bounded preview produced after a safety-quota or durable-write
            // failure is diagnostic only. Preserve that typed failure on the
            // transcript message so it cannot be consumed as a successful tool
            // observation (or mistaken for an honest retrievable reference).
            if matches!(
                stored.status,
                crate::tool_result_store::ToolResultStoreStatus::Failed(_)
            ) {
                *is_error = true;
            }
            let internal =
                envelope.kind == rightkit_agent_engine_protocol::TOOL_OUTPUT_INTERNAL_KIND;
            let mut rewritten =
                output_ref_for(&full, &stored, TOOL_OUTPUT_PREVIEW_LIMIT, Some(&media_type));
            if internal {
                rewritten.kind = rightkit_agent_engine_protocol::TOOL_OUTPUT_INTERNAL_KIND.into();
            }
            *output = Some(rewritten);
            changed = true;
        }
        if changed {
            self.transcript.replace(messages);
        }
    }
}

fn is_decayable_tool_output_kind(kind: &str) -> bool {
    kind == "tool_output_inline"
        || kind == rightkit_agent_engine_protocol::TOOL_OUTPUT_INTERNAL_KIND
}

/// Output envelope for content that stays fully inline and unmodified
/// (exempt-class results — CQ-07 externalizes oversized non-exempt results
/// at record time): nothing truncated, nothing retrievable elsewhere.
fn verbatim_inline_output_ref(content: &str, declared_media_type: Option<&str>) -> OutputRef {
    let hash = crate::tool_result_store::content_hash(content);
    OutputRef {
        id: format!("sha256:{hash}"),
        kind: "tool_output_inline".into(),
        size_bytes: content.len() as i64,
        content_hash: Some(hash),
        preview: Some(content.to_string()),
        media_type: resolved_output_media_type(content, declared_media_type),
        truncated: false,
        retrievable: false,
    }
}

/// The declared MIME type, or the shared sniff: JSON parses →
/// application/json, else plain text. One definition shared by both
/// envelope builders so they cannot drift.
fn resolved_output_media_type(content: &str, declared_media_type: Option<&str>) -> String {
    declared_media_type
        .unwrap_or_else(|| {
            if serde_json::from_str::<serde_json::Value>(content).is_ok() {
                "application/json"
            } else {
                "text/plain; charset=utf-8"
            }
        })
        .to_string()
}

fn output_ref_for(
    content: &str,
    stored: &crate::tool_result_store::StoredToolResult,
    preview_limit: usize,
    declared_media_type: Option<&str>,
) -> OutputRef {
    let hash = crate::tool_result_store::content_hash(content);
    let failed = matches!(
        stored.status,
        crate::tool_result_store::ToolResultStoreStatus::Failed(_)
    );
    let retrievable = !failed && stored.virtual_path.is_some();
    let truncated = failed || stored.full_len > preview_limit;
    let media_type = resolved_output_media_type(content, declared_media_type);
    OutputRef {
        id: if retrievable {
            stored.ref_id.clone()
        } else {
            format!("sha256:{hash}")
        },
        kind: if failed {
            // This is a typed error envelope, not a successful preview. The
            // hash is an identity only; `retrievable: false` is authoritative.
            "tool_output_error"
        } else if retrievable {
            "tool_output"
        } else if truncated {
            "tool_output_preview"
        } else {
            "tool_output_inline"
        }
        .into(),
        size_bytes: stored.full_len as i64,
        content_hash: Some(hash),
        preview: Some(stored.preview.clone()),
        media_type,
        truncated,
        retrievable,
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod output_envelope_tests {
    use super::*;

    #[test]
    fn inline_output_has_typed_nonretrievable_envelope() {
        let mut store = ToolResultStore::new();
        let stored = store.store(r#"{"ok":true}"#, 4_000);
        let output = output_ref_for(r#"{"ok":true}"#, &stored, 4_000, None);
        assert_eq!(output.kind, "tool_output_inline");
        assert_eq!(output.media_type, "application/json");
        assert!(!output.truncated);
        assert!(!output.retrievable);
        assert!(output.id.starts_with("sha256:"));
    }

    #[test]
    fn externalized_output_is_marked_truncated_and_retrievable() {
        let content = "x".repeat(4_001);
        let mut store = ToolResultStore::new();
        let stored = store.store(&content, 4_000);
        let output = output_ref_for(&content, &stored, 4_000, Some("text/x-log"));
        assert_eq!(output.kind, "tool_output");
        assert!(output.truncated);
        assert!(output.retrievable);
        assert_eq!(output.media_type, "text/x-log");
    }

    #[test]
    fn failed_externalization_is_typed_error_without_retrieval_ref() {
        let content = "x".repeat(4_001);
        let stored = crate::tool_result_store::StoredToolResult {
            ref_id: String::new(),
            virtual_path: None,
            preview: "[Large tool output could not be archived]\n--- head ---\nx".into(),
            full_len: content.len(),
            status: crate::tool_result_store::ToolResultStoreStatus::Failed(
                crate::tool_result_store::ToolResultStoreFailure::SafetyLimit,
            ),
        };

        let output = output_ref_for(&content, &stored, 4_000, None);

        assert_eq!(output.kind, "tool_output_error");
        assert!(output.truncated);
        assert!(!output.retrievable);
        assert!(output.id.starts_with("sha256:"));
        assert_eq!(output.preview, Some(stored.preview));
    }
}

/// True when any of `definitions` (already filtered to enabled, local-plugin
/// `PreToolCall` middleware) matches `call`. Reuses the exact same matcher
/// `apply_pre_tool_middleware` applies on the sequential path, so a call
/// judged "unaffected" here really is unaffected there — no separate
/// heuristic to drift out of sync with the real gate.
fn pre_tool_middleware_matches_call(definitions: &[MiddlewareDefinition], call: &ToolCall) -> bool {
    if definitions.is_empty() {
        return false;
    }
    let args = serde_json::from_str::<serde_json::Value>(&call.arguments_json)
        .unwrap_or(serde_json::Value::Null);
    definitions
        .iter()
        .any(|definition| middleware_matches_call(definition, call, &args))
}

/// `tool_search` only lists the tool catalog the session already offers; its
/// result carries no repository, web or MCP content, so loading a deferred
/// tool must not move later durable writes (`memory_save`, contract rules 16
/// and 45) out of the user's own session origin.
pub(super) fn tool_result_carries_content(tool_name: &str) -> bool {
    let name = tool_name.strip_prefix("tool:").unwrap_or(tool_name);
    !name.eq_ignore_ascii_case("tool_search")
}

pub(super) fn tool_source_provenance(source: &str) -> rightkit_agent_policy::ContentProvenance {
    use rightkit_agent_policy::{ContentOrigin, ContentProvenance};

    let source = source.strip_prefix("tool:").unwrap_or(source);
    let normalized = source.to_ascii_lowercase();
    let origin = if normalized == "mcp_call"
        || normalized.starts_with("mcp_")
        || normalized.starts_with("list_mcp_")
        || normalized.starts_with("read_mcp_")
        || normalized.starts_with("mcp__")
    {
        ContentOrigin::McpDerived
    } else if normalized == "web_search"
        || normalized == "browser_fetch"
        || normalized == "recursive_fetch"
        || normalized.starts_with("browser_")
        || normalized == "github"
        || normalized == "context7"
    {
        ContentOrigin::WebDerived
    } else {
        ContentOrigin::ToolDerived
    };
    ContentProvenance::new(origin, source)
}

/// Lane 5 / P1-8 tests -- concurrency-safe batching and bounded, ordered
/// concurrent dispatch. New tests only; nothing here edits an existing test.
///
/// Prior-art note: the book's cited paths (repos/cli/claude-code/...,
/// repos/cli/codex/...) do not exist in this workspace checkout -- repos/cli
/// is absent -- so this design (fail-safe-serial default via is_read_only(),
/// partition into a concurrent read batch / disjoint-write batch / sequential
/// remainder, bounded+chunked concurrent execution preserving call order) was
/// inferred from the book's prose description of that prior art, not read
/// from the source files directly.
#[cfg(test)]
mod concurrency_safe_batching_tests {
    use super::*;
    use crate::llm::{LlmResponse, ScriptedLlm};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn bypass_engine_with_tools(tools: ToolRegistry, responses: Vec<LlmResponse>) -> SessionEngine {
        let llm = Arc::new(ScriptedLlm::new(responses));
        let conduit = Arc::new(Conduit::new(256));
        let cfg = SessionEngineConfig {
            permission_mode: PermissionMode::Bypass,
            ..Default::default()
        };
        SessionEngine::new(llm, Arc::new(tools), conduit, cfg)
    }

    fn bypass_engine(responses: Vec<LlmResponse>) -> SessionEngine {
        bypass_engine_with_tools(ToolRegistry::build_default(), responses)
    }

    fn sleep_call(id: &str, ms: u64) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: "sleep".into(),
            arguments_json: serde_json::json!({ "milliseconds": ms }).to_string(),
        }
    }

    /// DoD #1: unknown/unparseable-by-name tools default to unsafe/serial --
    /// can_dispatch_readonly_fast (the read-batch gate) must return false
    /// for a tool name that is not in the registry, and true for a real
    /// declared-read-only tool under a permissive mode. This is the exact
    /// primitive dispatch_response_tools_and_post_checks partitions on.
    #[tokio::test]
    async fn unregistered_tool_name_is_not_dispatched_concurrently() {
        let engine = bypass_engine(vec![]);
        let unknown = ToolCall {
            id: "tc-unknown".into(),
            name: "definitely_not_a_registered_tool".into(),
            arguments_json: "{}".into(),
        };
        let unknown_ref = &unknown;
        assert!(
            !engine.can_dispatch_readonly_fast(&unknown_ref),
            "an unregistered tool must default to serial, never to the concurrent read batch"
        );

        let known = sleep_call("tc-known", 0);
        let known_ref = &known;
        assert!(
            engine.can_dispatch_readonly_fast(&known_ref),
            "a declared read-only tool must be eligible for the concurrent read batch"
        );
    }

    /// CR-07 (Invariant X): mutating tools never execute concurrently, even
    /// when their target paths are pairwise-disjoint. Path-prefix
    /// disjointness alone is not a sufficient transaction boundary (symlinks,
    /// case-folding, generated files, shared indexes, hooks, process-global
    /// state, and multi-file invariants can still overlap), so the former
    /// `parallel_write_prefix` leading-batch optimization is removed and
    /// every non-concurrency-safe call runs strictly one at a time.
    ///
    /// This test instruments actual entry/exit of two disjoint-path writes
    /// (not just their final file contents) and asserts the recorded
    /// intervals never overlap -- a structural proof, not a timing guess: if
    /// the calls ever again ran concurrently, both writers would be
    /// in-flight past the barrier at once and `peak` would exceed 1.
    struct MutationEntryExitProbe {
        peak: Arc<AtomicUsize>,
        in_flight: Arc<AtomicUsize>,
        order: Arc<std::sync::Mutex<Vec<(String, &'static str)>>>,
    }

    #[async_trait::async_trait]
    impl rightkit_agent_tools::coding_tools::Tool for MutationEntryExitProbe {
        fn name(&self) -> &str {
            "mutation_entry_exit_probe"
        }
        fn description(&self) -> &str {
            "test-only mutating concurrency probe"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        fn is_read_only(&self) -> bool {
            false
        }
        fn capability_class(&self) -> Option<rightkit_agent_policy::CapabilityClass> {
            Some(rightkit_agent_policy::CapabilityClass::WriteFile)
        }
        async fn execute(
            &self,
            args: serde_json::Value,
            _ctx: &ToolContext,
        ) -> rightkit_agent_tools::coding_tools::ToolResult {
            let id = args["id"].as_str().unwrap_or("?").to_string();
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            self.order.lock().unwrap().push((id.clone(), "enter"));
            tokio::time::sleep(std::time::Duration::from_millis(15)).await;
            self.order.lock().unwrap().push((id.clone(), "exit"));
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            rightkit_agent_tools::coding_tools::ToolResult::ok("probed")
        }
    }

    #[tokio::test]
    async fn disjoint_path_mutations_never_overlap_in_time() {
        let peak = Arc::new(AtomicUsize::new(0));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut tools = ToolRegistry::build_default();
        tools.register(Box::new(MutationEntryExitProbe {
            peak: peak.clone(),
            in_flight,
            order: order.clone(),
        }));
        let call_a = ToolCall {
            id: "tc-mut-a".into(),
            name: "mutation_entry_exit_probe".into(),
            arguments_json: serde_json::json!({"id": "a", "path": "a.txt"}).to_string(),
        };
        let call_b = ToolCall {
            id: "tc-mut-b".into(),
            name: "mutation_entry_exit_probe".into(),
            arguments_json: serde_json::json!({"id": "b", "path": "b.txt"}).to_string(),
        };
        let response = LlmResponse::default()
            .with_tool_call(call_a)
            .with_tool_call(call_b);
        let llm = Arc::new(ScriptedLlm::new(vec![response, LlmResponse::text("done")]));
        let conduit = Arc::new(Conduit::new(128));
        let cfg = SessionEngineConfig {
            permission_mode: PermissionMode::Bypass,
            ..Default::default()
        };
        let mut engine = SessionEngine::new(llm, Arc::new(tools), conduit, cfg);

        let out = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            engine.submit_user_message("run two disjoint mutations"),
        )
        .await
        .expect("turn should complete")
        .unwrap();
        assert_eq!(out, "done");

        assert_eq!(
            peak.load(Ordering::SeqCst),
            1,
            "two mutating calls to disjoint paths must never be in-flight at the same time"
        );
        let recorded = order.lock().unwrap().clone();
        assert_eq!(
            recorded,
            vec![
                ("a".to_string(), "enter"),
                ("a".to_string(), "exit"),
                ("b".to_string(), "enter"),
                ("b".to_string(), "exit"),
            ],
            "entry/exit trace must show call A fully complete before call B enters"
        );
    }

    /// A read-only fixture tool that tracks how many calls are executing at
    /// once via a shared AtomicUsize, and records the peak observed
    /// concurrency. Deterministic structural evidence of "ran concurrently"
    /// instead of a wall-clock assertion: a coordination handshake
    /// (tokio::sync::Barrier, released only once every call in the chunk has
    /// entered execute) makes the overlap assertion exact rather than
    /// timing-dependent, so it cannot be flaky under a loaded/slow machine --
    /// if concurrency ever regresses to serial, the barrier simply never
    /// releases and the outer timeout fails the test instead of a timing
    /// window silently passing.
    struct ConcurrencyProbeTool {
        peak: Arc<AtomicUsize>,
        in_flight: Arc<AtomicUsize>,
        entered: Arc<tokio::sync::Barrier>,
    }

    #[async_trait::async_trait]
    impl rightkit_agent_tools::coding_tools::Tool for ConcurrencyProbeTool {
        fn name(&self) -> &str {
            "concurrency_probe"
        }
        fn description(&self) -> &str {
            "test-only concurrency probe"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        fn is_read_only(&self) -> bool {
            true
        }
        async fn execute(
            &self,
            _args: serde_json::Value,
            _ctx: &ToolContext,
        ) -> rightkit_agent_tools::coding_tools::ToolResult {
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            self.entered.wait().await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            rightkit_agent_tools::coding_tools::ToolResult::ok("probed")
        }
    }

    /// DoD #2: for a chunk of N read-only calls with bound >= N, all N are in
    /// flight inside execute at once -- proved by every call reaching a
    /// Barrier sized exactly N. Uses dispatch_concurrent_batch_with_bound
    /// directly with an explicit bound -- no env var, no process-global
    /// mutation, no timing assertion.
    #[tokio::test]
    async fn calls_within_one_chunk_are_genuinely_concurrent() {
        let peak = Arc::new(AtomicUsize::new(0));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(tokio::sync::Barrier::new(4));
        let mut tools = ToolRegistry::new();
        tools.register(Box::new(ConcurrencyProbeTool {
            peak: peak.clone(),
            in_flight,
            entered,
        }));
        let mut engine = bypass_engine_with_tools(tools, vec![]);

        let calls: Vec<ToolCall> = (0..4)
            .map(|i| ToolCall {
                id: format!("tc-probe{i}"),
                name: "concurrency_probe".into(),
                arguments_json: "{}".into(),
            })
            .collect();
        let call_refs: Vec<&ToolCall> = calls.iter().collect();
        let mut turn_results = std::collections::HashMap::new();

        let fut = engine.dispatch_concurrent_batch_with_bound(&call_refs, 4, &mut turn_results);
        tokio::time::timeout(std::time::Duration::from_secs(5), fut)
            .await
            .expect(
                "all 4 calls must reach the barrier together within one bounded chunk \
                 (bound=4 >= 4 calls); a hang here means the chunk ran serially",
            );

        assert_eq!(
            peak.load(Ordering::SeqCst),
            4,
            "peak in-flight concurrency within one chunk must equal the chunk size"
        );
        assert_eq!(turn_results.len(), 4);
    }

    /// DoD #2 (bound): dispatch_concurrent_batch_with_bound with bound=2 over
    /// 6 calls must split into exactly 3 chunks of 2 -- asserted directly via
    /// slice::chunks, the same primitive the dispatcher uses, so this is a
    /// structural/deterministic assertion, not a timing one.
    #[test]
    fn bound_chunks_a_batch_into_fixed_size_groups() {
        let calls: Vec<ToolCall> = (0..6).map(|i| sleep_call(&format!("tc-s{i}"), 0)).collect();
        let refs: Vec<&ToolCall> = calls.iter().collect();
        let chunk_sizes: Vec<usize> = refs.chunks(2).map(|c| c.len()).collect();
        assert_eq!(chunk_sizes, vec![2, 2, 2]);

        // bound=4 over 6 calls => chunks of 4 then 2, never one chunk of 6.
        let chunk_sizes_4: Vec<usize> = refs.chunks(4).map(|c| c.len()).collect();
        assert_eq!(chunk_sizes_4, vec![4, 2]);
    }

    /// Regression test for the reorder defect found by cross-check against
    /// claude-code's partitionToolCalls (toolOrchestration.ts:91-116): a turn
    /// issuing [write A, read B, write C] must execute and record results in
    /// ISSUE order -- write A, then read B, then write C -- never reads-first
    /// (a prior version globally partitioned into read_calls/mut_calls and
    /// ran every read before every write, silently reordering the turn; a
    /// write-then-read-back-in-the-same-turn verification pattern would then
    /// read pre-write state with no error). write_file and read_file both
    /// touch the real filesystem here specifically so a reorder would be
    /// observable as wrong file content, not just wrong event order.
    #[tokio::test]
    async fn interleaved_write_read_write_executes_in_issue_order() {
        let tmp = tempfile::tempdir().unwrap();
        let write_a = ToolCall {
            id: "tc-a-write".into(),
            name: "write_file".into(),
            arguments_json: serde_json::json!({"path": "a.txt", "content": "first"}).to_string(),
        };
        let read_b = ToolCall {
            id: "tc-b-read".into(),
            name: "read_file".into(),
            arguments_json: serde_json::json!({"path": "a.txt"}).to_string(),
        };
        let write_c = ToolCall {
            id: "tc-c-write".into(),
            name: "write_file".into(),
            arguments_json: serde_json::json!({"path": "c.txt", "content": "third"}).to_string(),
        };
        let response = LlmResponse::default()
            .with_tool_call(write_a)
            .with_tool_call(read_b)
            .with_tool_call(write_c);

        let llm = Arc::new(ScriptedLlm::new(vec![response, LlmResponse::text("done")]));
        let tools = Arc::new(ToolRegistry::build_default());
        let conduit = Arc::new(Conduit::new(256));
        let cfg = SessionEngineConfig {
            workspace_root: tmp.path().to_path_buf(),
            permission_mode: PermissionMode::Bypass,
            // This fixture isolates dispatch ordering without an assurance broker.
            // Receipt-bound verification has separate production-path coverage.
            auto_post_edit_diagnostics: false,
            ..Default::default()
        };
        let mut engine = SessionEngine::new(llm, tools, conduit, cfg);

        let out = engine
            .submit_user_message("write a, read a back, write c")
            .await
            .unwrap();
        assert_eq!(out, "done");

        let tool_call_ids: Vec<String> = engine
            .transcript()
            .messages()
            .iter()
            .filter_map(|m| match m {
                rightkit_agent_engine_protocol::TranscriptMessage::Tool {
                    tool_call_id, ..
                } => Some(tool_call_id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            tool_call_ids,
            vec![
                "tc-a-write".to_string(),
                "tc-b-read".to_string(),
                "tc-c-write".to_string(),
            ],
            "results must be recorded in issue order: write A, read B, write C -- \
             never all reads before all writes"
        );

        // Behavioural proof, not just event order: the read must see the
        // write that preceded it in the same turn, not pre-write state.
        let read_result_summary = engine
            .transcript()
            .messages()
            .iter()
            .find_map(|m| match m {
                rightkit_agent_engine_protocol::TranscriptMessage::Tool {
                    tool_call_id,
                    summary,
                    ..
                } if tool_call_id == "tc-b-read" => Some(summary.clone()),
                _ => None,
            })
            .expect("read_file result");
        assert!(
            read_result_summary.contains("first"),
            "read B (issued after write A) must observe write A's content, got: {read_result_summary}"
        );
    }

    /// DoD #4: result ordering matches call order regardless of completion
    /// order. The first call sleeps far longer than the second; ordering is
    /// asserted on the recorded transcript, not on timing -- a fast/slow
    /// machine cannot make this assertion pass or fail differently, only
    /// whether the wait itself completes within the generous outer timeout,
    /// which is not itself asserted on.
    #[tokio::test]
    async fn concurrent_batch_result_order_matches_call_order() {
        let slow_first = sleep_call("tc-slow", 120);
        let fast_second = sleep_call("tc-fast", 1);
        let response = LlmResponse::default()
            .with_tool_call(slow_first)
            .with_tool_call(fast_second);
        let mut engine = bypass_engine(vec![response, LlmResponse::text("done")]);

        let out = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            engine.submit_user_message("run two sleeps"),
        )
        .await
        .expect("turn should complete well within the outer timeout")
        .unwrap();
        assert_eq!(out, "done");

        let tool_call_ids: Vec<String> = engine
            .transcript()
            .messages()
            .iter()
            .filter_map(|m| match m {
                rightkit_agent_engine_protocol::TranscriptMessage::Tool {
                    tool_call_id, ..
                } => Some(tool_call_id.clone()),
                _ => None,
            })
            .collect();

        assert_eq!(
            tool_call_ids,
            vec!["tc-slow".to_string(), "tc-fast".to_string()],
            "results must be recorded in call order, not completion order"
        );
    }
}

/// Lane 5b stream-dispatch proofs. These use owned futures from the real
/// concurrent executor, while keeping existing Lane 5a tests unchanged.
#[cfg(test)]
mod lane_5b_stream_dispatch_tests {
    use super::*;
    use crate::llm::{LlmResponse, ScriptedLlm};
    use futures::{stream::FuturesOrdered, StreamExt};
    use std::sync::Arc;

    fn engine() -> SessionEngine {
        let llm = Arc::new(ScriptedLlm::new(vec![LlmResponse::text("done")]));
        SessionEngine::new(
            llm,
            Arc::new(ToolRegistry::build_default()),
            Arc::new(Conduit::new(128)),
            SessionEngineConfig {
                permission_mode: PermissionMode::Bypass,
                ..Default::default()
            },
        )
    }

    fn call(id: &str, name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments_json: args.to_string(),
        }
    }

    #[tokio::test]
    async fn safe_call_is_running_before_stream_end() {
        let engine = engine();
        let mut futures = FuturesOrdered::new();
        futures.push_back(engine.streaming_concurrent_tool_future(call(
            "sleep-1",
            "sleep",
            serde_json::json!({"milliseconds": 80}),
        )));
        let stream_end = tokio::time::sleep(std::time::Duration::from_millis(10));
        tokio::pin!(stream_end);
        tokio::select! {
            completed = futures.next() => panic!("tool completed before stream end: {completed:?}"),
            _ = &mut stream_end => assert!(!futures.is_empty()),
        }
    }

    #[tokio::test]
    async fn ordered_queue_records_adversarial_completion_in_call_order() {
        let mut engine = engine();
        engine.transcript.push_assistant("", None, Vec::new());
        let mut futures = FuturesOrdered::new();
        futures.push_back(engine.streaming_concurrent_tool_future(call(
            "slow",
            "sleep",
            serde_json::json!({"milliseconds": 60}),
        )));
        futures.push_back(engine.streaming_concurrent_tool_future(call(
            "fast",
            "sleep",
            serde_json::json!({"milliseconds": 1}),
        )));
        let mut results = std::collections::HashMap::new();
        while let Some((call, result)) = futures.next().await {
            engine
                .record_streaming_concurrent_result(call, result, &mut results)
                .await;
        }
        let ids = engine
            .transcript()
            .messages()
            .iter()
            .filter_map(|message| match message {
                TranscriptMessage::Tool { tool_call_id, .. } => Some(tool_call_id.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["slow", "fast"]);
    }

    #[test]
    fn mutation_unknown_and_unparseable_calls_are_serial_barriers() {
        let engine = engine();
        let mutation = call(
            "write",
            "write_file",
            serde_json::json!({"path":"x","content":"y"}),
        );
        let unknown = call("unknown", "not_registered", serde_json::json!({}));
        let malformed = ToolCall {
            id: "malformed".into(),
            name: "sleep".into(),
            arguments_json: "{".into(),
        };
        assert!(!engine.streaming_tool_is_concurrent_safe(&mutation));
        assert!(!engine.streaming_tool_is_concurrent_safe(&unknown));
        assert!(!engine.streaming_tool_is_concurrent_safe(&malformed));
    }

    #[tokio::test]
    async fn dropping_stream_queue_cancels_pending_work() {
        let engine = engine();
        let mut futures = FuturesOrdered::new();
        futures.push_back(engine.streaming_concurrent_tool_future(call(
            "cancelled",
            "sleep",
            serde_json::json!({"milliseconds": 10_000}),
        )));
        let pending =
            tokio::time::timeout(std::time::Duration::from_millis(10), futures.next()).await;
        assert!(pending.is_err());
        drop(futures);
    }

    #[tokio::test]
    async fn approval_gated_call_waits_before_serial_execution() {
        let llm = Arc::new(ScriptedLlm::new(vec![]));
        let conduit = Arc::new(Conduit::new(128));
        // A human client must be attached, or the approval is denied as unattended.
        let _human = conduit.subscribe_human();
        let mut engine = SessionEngine::new(
            llm,
            Arc::new(ToolRegistry::build_default()),
            conduit,
            SessionEngineConfig {
                permission_mode: PermissionMode::Ask,
                ..Default::default()
            },
        );
        let shell = call("shell", "shell", serde_json::json!({"command":"echo hi"}));
        assert!(!engine.streaming_tool_is_concurrent_safe(&shell));
        let approvals = engine.pending_approvals.clone();
        let task = tokio::spawn(async move {
            let mut results = std::collections::HashMap::new();
            engine
                .dispatch_streaming_tool_call(&shell, &mut results)
                .await;
            results
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if approvals.lock().unwrap().contains_key("shell") {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("serial approval request was not published");
        let sender = approvals.lock().unwrap().remove("shell");
        sender.expect("pending approval sender").send(true).unwrap();
        let results = task.await.expect("approval task");
        assert!(results.contains_key("shell"));
    }
}

/// Lane R4 / CR-15: cancellation must actually reach background shell work.
/// These tests drive the real dispatch path — a scripted turn that runs
/// `shell` with `run_in_background: true` — then cancel through the same
/// channels the daemon uses (session interrupt / per-call cancel token) and
/// observe the spawned processes, not internal call flags.
#[cfg(all(test, feature = "host"))]
mod background_shell_cancel_propagation_tests {
    use super::*;
    use crate::llm::{LlmResponse, ScriptedLlm};
    use rightkit_agent_tools::coding_tools::background_shell::BgStatus;
    use std::time::Duration;

    static PROCESS_LIFECYCLE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn long_sleep_command() -> String {
        if cfg!(windows) {
            "Start-Sleep -Seconds 30".to_string()
        } else {
            "sleep 30".to_string()
        }
    }

    /// Build a bypass-mode engine whose first turn spawns one background shell
    /// running `command`, with an interrupt channel wired exactly like the
    /// daemon does (`set_interrupt_receiver`).
    fn bg_spawn_engine(
        session_id: &str,
        workspace_root: &std::path::Path,
        command: &str,
    ) -> (SessionEngine, tokio::sync::watch::Sender<bool>) {
        let _ = rightkit_agent_tools::coding_tools::process_resource_authority::install_process_resource_profile(
            rightkit_agent_tools::coding_tools::process_resource_authority::ResourceProfile::Expanded,
        );
        let spawn_call = ToolCall {
            id: "tc-bg-spawn".into(),
            name: "shell".into(),
            arguments_json: serde_json::json!({
                "command": command,
                "run_in_background": true,
            })
            .to_string(),
        };
        let mut second_spawn_call = spawn_call.clone();
        second_spawn_call.id = "tc-bg-spawn-2".into();
        let llm = Arc::new(ScriptedLlm::new(vec![
            LlmResponse::default().with_tool_call(spawn_call),
            LlmResponse::text("done"),
            LlmResponse::default().with_tool_call(second_spawn_call),
            LlmResponse::text("done"),
        ]));
        let conduit = Arc::new(Conduit::new(64));
        let cfg = SessionEngineConfig {
            session_id: session_id.into(),
            workspace_root: workspace_root.to_path_buf(),
            permission_mode: PermissionMode::Bypass,
            ..Default::default()
        };
        let mut engine =
            SessionEngine::new(llm, Arc::new(ToolRegistry::build_default()), conduit, cfg);
        let (interrupt_tx, interrupt_rx) = tokio::sync::watch::channel(false);
        engine.set_interrupt_receiver(interrupt_rx);
        (engine, interrupt_tx)
    }

    /// Run the spawn turn and return the background shell id the tool
    /// reported in its real transcript result.
    ///
    /// Scoped to messages appended by THIS turn only: the id is found by
    /// scanning only the transcript tail added after `submit_user_message`
    /// runs, never by falling back to an earlier turn's still-present
    /// message. If this turn appended no message containing a quoted
    /// shell id, that is a real spawn/admission failure and the helper
    /// fails loudly with the turn's own tool-result text, rather than
    /// silently returning a stale id from a previous turn.
    async fn run_spawn_turn(engine: &mut SessionEngine) -> String {
        let baseline = engine.transcript().messages().len();
        let out = tokio::time::timeout(
            Duration::from_secs(15),
            engine.submit_user_message("start a long background job"),
        )
        .await
        .expect("spawn turn should finish")
        .expect("spawn turn should succeed");
        assert_eq!(out, "done");
        let messages = engine.transcript().messages();
        let new_messages = &messages[baseline..];
        if let Some(id) = new_messages.iter().rev().find_map(|message| match message {
            TranscriptMessage::Tool { summary, .. } => {
                let start = summary.find('\'')?;
                let rest = &summary[start + 1..];
                let end = rest.find('\'')?;
                Some(rest[..end].to_string())
            }
            _ => None,
        }) {
            return id;
        }
        let mut detail = String::new();
        for message in new_messages {
            if let TranscriptMessage::Tool { summary, .. } = message {
                detail.push_str("\n  tool result: ");
                detail.push_str(summary);
            }
        }
        panic!(
            "spawn turn appended no message with a quoted background shell id; \
             this turn added {} message(s) after baseline {baseline} with no spawned-shell \
             marker, indicating a real spawn/admission failure rather than an id collision.\
             {detail}",
            new_messages.len()
        );
    }

    /// Poll the store until `id` reaches `want`, observing live status.
    async fn wait_for_status(
        store: &rightkit_agent_tools::coding_tools::background_shell::BgStore,
        id: &str,
        want: BgStatus,
        context: &str,
    ) -> BgStatus {
        for _ in 0..200 {
            {
                let shells = store.lock().unwrap();
                let shell = shells
                    .get(id)
                    .unwrap_or_else(|| panic!("missing background shell {id}"));
                let status = shell.status();
                if status == want {
                    return status;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let actual = store.lock().unwrap().get(id).map(|shell| shell.status());
        panic!(
            "background shell {id} never reached {want:?} ({context}); final status: {actual:?}"
        );
    }

    /// Wait until cleanup has reaped the process. A kill can race with a
    /// natural exit, so either terminal status is valid for teardown.
    async fn wait_for_terminal_status(
        store: &rightkit_agent_tools::coding_tools::background_shell::BgStore,
        id: &str,
        context: &str,
    ) -> BgStatus {
        for _ in 0..200 {
            {
                let shells = store.lock().unwrap();
                let shell = shells
                    .get(id)
                    .unwrap_or_else(|| panic!("missing background shell {id}"));
                let status = shell.status();
                if matches!(status, BgStatus::Killed | BgStatus::Exited(_)) {
                    return status;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("background shell {id} never reached a terminal status ({context})");
    }

    /// DoD: a test starts a long-running child, cancels, and proves the child
    /// is gone. The `Killed` status is written by the spawn supervisor only
    /// after `terminate_tree` has reaped the real child, so this observes the
    /// process lifecycle end to end. Also proves double-cancel safety: a
    /// second interrupt is a no-op and leaves the terminal state intact.
    #[tokio::test]
    async fn interrupting_the_turn_kills_the_background_child_it_started() {
        let _test_guard = PROCESS_LIFECYCLE_TEST_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let (mut engine, interrupt_tx) =
            bg_spawn_engine("r4-cancel-portable", dir.path(), &long_sleep_command());
        let store = engine.background_shells.clone();

        let shell_id = run_spawn_turn(&mut engine).await;
        assert!(
            matches!(
                store.lock().unwrap().get(&shell_id).unwrap().status(),
                BgStatus::Running
            ),
            "background child must be alive right after the spawn turn"
        );

        interrupt_tx.send(true).expect("interrupt send");
        let status = wait_for_status(&store, &shell_id, BgStatus::Killed, "interrupt").await;
        assert!(matches!(status, BgStatus::Killed));

        // Double-cancel is safe: second interrupt changes nothing.
        let _ = interrupt_tx.send(true);
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(matches!(
            store.lock().unwrap().get(&shell_id).unwrap().status(),
            BgStatus::Killed
        ));
    }

    /// Group-termination proof: the background shell's *grandchild* (a sleep
    /// detached inside the shell) must be dead after cancellation — observed
    /// via `kill -0` on the recorded pid, not via any in-process flag.
    #[cfg(unix)]
    #[tokio::test]
    async fn interrupt_kills_descendants_of_the_background_shell_too() {
        let _test_guard = PROCESS_LIFECYCLE_TEST_LOCK.lock().await;
        use rightkit_child_process::AdoptedProcess;
        use std::num::NonZeroU32;

        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("grandchild.pid");
        let command = format!("sleep 30 & echo $! > {}; wait", pid_file.display());
        let (mut engine, interrupt_tx) = bg_spawn_engine("r4-cancel-tree", dir.path(), &command);
        let store = engine.background_shells.clone();

        let _shell_id = run_spawn_turn(&mut engine).await;

        let mut grandchild_pid: Option<i32> = None;
        for _ in 0..100 {
            if let Ok(text) = std::fs::read_to_string(&pid_file) {
                if let Ok(pid) = text.trim().parse::<i32>() {
                    grandchild_pid = Some(pid);
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let grandchild = grandchild_pid.expect("grandchild pid was never recorded");
        let grandchild_process = AdoptedProcess::new(
            NonZeroU32::new(grandchild as u32).expect("grandchild pid is non-zero"),
        );
        // Guard against a gate that proves nothing: the pid must be live first.
        assert!(
            grandchild_process.is_running().expect("liveness probe"),
            "grandchild {grandchild} was not running before cancellation"
        );

        interrupt_tx.send(true).expect("interrupt send");
        wait_for_status(&store, &_shell_id, BgStatus::Killed, "interrupt").await;

        let mut alive = true;
        for _ in 0..40 {
            if !grandchild_process.is_running().unwrap_or(false) {
                alive = false;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(!alive, "grandchild {grandchild} survived the cancel");
    }

    /// DoD: a second session's work survives the first session's cancel.
    /// Stores and interrupt channels are per-engine; cancelling session one
    /// must not touch session two's running child.
    #[tokio::test]
    async fn cancelling_one_session_spares_another_sessions_background_work() {
        let _test_guard = PROCESS_LIFECYCLE_TEST_LOCK.lock().await;
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();
        let (mut engine1, interrupt_tx1) =
            bg_spawn_engine("r4-cancel-s1", dir1.path(), &long_sleep_command());
        let (mut engine2, _interrupt_tx2) =
            bg_spawn_engine("r4-cancel-s2", dir2.path(), &long_sleep_command());
        let store1 = engine1.background_shells.clone();
        let store2 = engine2.background_shells.clone();

        let id1 = run_spawn_turn(&mut engine1).await;
        let id2 = run_spawn_turn(&mut engine2).await;
        assert_ne!(id1, id2, "sessions must own distinct background shells");

        interrupt_tx1.send(true).expect("interrupt session one");
        wait_for_status(&store1, &id1, BgStatus::Killed, "session one cancel").await;

        // Give an over-broad implementation ample time to do damage.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            matches!(
                store2.lock().unwrap().get(&id2).unwrap().status(),
                BgStatus::Running
            ),
            "session two's background work must survive session one's cancel"
        );

        // Cleanup so the test does not leak a live sleeper.
        store2.lock().unwrap().get(&id2).unwrap().kill();
        wait_for_terminal_status(&store2, &id2, "cleanup").await;
    }

    /// Known limitation, stated here because the code cannot say it loudly
    /// enough: the baseline resets when a turn's FIRST TOOL CALL dispatches.
    /// A turn that dispatches no tool calls is invisible to this seam, so an
    /// interrupt following such a turn still scopes against the last
    /// tool-dispatching turn's baseline. Turn-start rebaselining without a
    /// hook into the loop itself is not achievable from this file.
    ///
    /// What IS guaranteed — and proven below — is the consecutive-tool-turn
    /// case: work from turn N survives an interrupt in turn N+1 when both
    /// turns dispatched tools, because turn N+1's first registration
    /// rebaselines the store to protect turn N's shells.
    #[tokio::test]
    async fn interrupting_a_new_turn_spares_previous_turns_background_work() {
        let _test_guard = PROCESS_LIFECYCLE_TEST_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        // Two consecutive tool-calling turns, each spawning one background
        // shell. The second turn's registration must rebaseline the store so
        // turn one's shell becomes protected.
        let (mut engine, interrupt_tx) =
            bg_spawn_engine("r4-cancel-cross-turn", dir.path(), &long_sleep_command());
        let store = engine.background_shells.clone();

        let first_turn_shell = run_spawn_turn(&mut engine).await;
        let second_turn_shell = run_spawn_turn(&mut engine).await;
        assert_ne!(first_turn_shell, second_turn_shell);

        interrupt_tx.send(true).expect("interrupt send");
        wait_for_status(
            &store,
            &second_turn_shell,
            BgStatus::Killed,
            "current-turn cancel",
        )
        .await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            matches!(
                store
                    .lock()
                    .unwrap()
                    .get(&first_turn_shell)
                    .unwrap()
                    .status(),
                BgStatus::Running
            ),
            "an earlier turn's background work must survive a later turn's interrupt"
        );

        // Cleanup so the test does not leak a live sleeper.
        store.lock().unwrap().get(&first_turn_shell).unwrap().kill();
        wait_for_terminal_status(&store, &first_turn_shell, "cleanup").await;
    }
}

/// CR-28: externalization policy gate at the dispatch boundary. These tests
/// drive the REAL dispatch path (`submit_user_message` over a scripted LLM),
/// so every result flows through `store_tool_output` exactly as production
/// results do. Written against the policy, not the store's internals.
#[cfg(test)]
mod cr28_externalization_policy_tests {
    use super::*;
    use crate::llm::{LlmResponse, ScriptedLlm};

    /// Fixture tool returning a fixed payload. Read-only so it is eligible
    /// for either dispatch path; both paths record through the same
    /// `store_tool_output` gate.
    struct FixedOutputTool {
        name: &'static str,
        content: String,
    }

    #[async_trait::async_trait]
    impl rightkit_agent_tools::coding_tools::Tool for FixedOutputTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "CR-28 fixture returning fixed output"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        fn is_read_only(&self) -> bool {
            true
        }
        async fn execute(
            &self,
            _args: serde_json::Value,
            _ctx: &ToolContext,
        ) -> rightkit_agent_tools::coding_tools::ToolResult {
            rightkit_agent_tools::coding_tools::ToolResult::ok(self.content.clone())
        }
    }

    fn engine_with(
        tools: ToolRegistry,
        responses: Vec<LlmResponse>,
        session_id: &str,
        workspace_root: std::path::PathBuf,
    ) -> SessionEngine {
        let cfg = SessionEngineConfig {
            session_id: session_id.into(),
            workspace_root,
            permission_mode: PermissionMode::Bypass,
            ..Default::default()
        };
        SessionEngine::new(
            Arc::new(ScriptedLlm::new(responses)),
            Arc::new(tools),
            Arc::new(Conduit::new(128)),
            cfg,
        )
    }

    fn fixture_registry(pairs: &[(&'static str, String)]) -> ToolRegistry {
        let mut tools = ToolRegistry::new();
        for (name, content) in pairs {
            tools.register(Box::new(FixedOutputTool {
                name,
                content: content.clone(),
            }));
        }
        tools
    }

    fn call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments_json: "{}".into(),
        }
    }

    /// Pull `(summary, output, is_error)` for one tool result out of the transcript.
    fn recorded_result(
        engine: &SessionEngine,
        tool_call_id: &str,
    ) -> (String, Option<OutputRef>, bool) {
        engine
            .transcript
            .messages()
            .iter()
            .find_map(|message| match message {
                rightkit_agent_engine_protocol::TranscriptMessage::Tool {
                    tool_call_id: id,
                    summary,
                    output,
                    is_error,
                    ..
                } if id == tool_call_id => Some((summary.clone(), output.clone(), *is_error)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no transcript result for {tool_call_id}"))
    }

    fn oversized() -> String {
        "x".repeat(TOOL_OUTPUT_PREVIEW_LIMIT + 500)
    }

    /// Invariant 1: an exempt-class result arrives unmodified through the
    /// real dispatch path — never masked, truncated, or externalized.
    #[tokio::test]
    async fn exempt_class_result_arrives_verbatim_through_real_dispatch() {
        let tmp = tempfile::tempdir().unwrap();
        let big = oversized();
        let response = LlmResponse::default().with_tool_call(call("tc-exempt", "plugin_tool"));
        let mut engine = engine_with(
            fixture_registry(&[("plugin_tool", big.clone())]),
            vec![response, LlmResponse::text("done")],
            "cr28-exempt",
            tmp.path().to_path_buf(),
        );

        let out = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            engine.submit_user_message("run the exempt tool"),
        )
        .await
        .expect("turn should finish")
        .expect("turn should succeed");
        assert_eq!(out, "done");

        let (summary, output, is_error) = recorded_result(&engine, "tc-exempt");
        assert_eq!(
            summary, big,
            "exempt-class result must arrive verbatim, byte-for-byte"
        );
        assert!(!is_error, "exempt result is a clean observation");
        let output = output.expect("exempt result carries an inline envelope");
        assert!(!output.truncated, "exempt result must not be truncated");
        assert!(
            !output.retrievable,
            "exempt result must not be externalized"
        );
        assert_eq!(output.size_bytes, big.len() as i64);
    }

    /// Audit M9: an exempt-class result past the inline cap spills instead of
    /// staying verbatim in the transcript forever.
    #[tokio::test]
    async fn exempt_class_result_over_the_cap_spills_to_the_store() {
        let tmp = tempfile::tempdir().unwrap();
        let big = "y".repeat(EXEMPT_TOOL_OUTPUT_INLINE_CAP + 1000);
        let response = LlmResponse::default().with_tool_call(call("tc-exempt-big", "plugin_tool"));
        let mut engine = engine_with(
            fixture_registry(&[("plugin_tool", big.clone())]),
            vec![response, LlmResponse::text("done")],
            "m9-exempt-big",
            tmp.path().to_path_buf(),
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            engine.submit_user_message("run the exempt tool"),
        )
        .await
        .expect("turn should finish")
        .expect("turn should succeed");
        let (summary, output, _) = recorded_result(&engine, "tc-exempt-big");
        assert!(
            summary.len() < big.len(),
            "oversized exempt result must not stay verbatim inline"
        );
        let output = output.expect("envelope");
        assert!(output.truncated, "spilled result is marked truncated");
        assert_eq!(output.size_bytes, big.len() as i64);
    }

    /// Invariant 2 (CQ-07): an oversized non-exempt result is externalized
    /// at record time, in the SAME turn that produced it — the model
    /// projection carries the bounded preview plus a retrievable reference,
    /// while the full output stays in the store/disk corpus as execution
    /// data recoverable through the reference id.
    #[tokio::test]
    async fn current_turn_oversized_result_externalizes_at_record() {
        let tmp = tempfile::tempdir().unwrap();
        let big = oversized();
        let head: String = big.chars().take(64).collect();
        let response = LlmResponse::default().with_tool_call(call("tc-current", "big_output"));
        let mut engine = engine_with(
            fixture_registry(&[("big_output", big.clone())]),
            vec![response, LlmResponse::text("done")],
            "cq07-current",
            tmp.path().to_path_buf(),
        );

        engine
            .submit_user_message("run the big tool")
            .await
            .unwrap();

        let (summary, output, is_error) = recorded_result(&engine, "tc-current");
        assert!(
            summary.contains("[Large tool output externalized]"),
            "current-turn oversized result must project the externalized preview, got prefix: {}",
            summary.chars().take(80).collect::<String>()
        );
        assert!(
            summary.contains(&head),
            "externalized preview must retain the head of the content"
        );
        assert!(
            summary.len() < big.len(),
            "model projection must be bounded, not the full payload"
        );
        assert!(!is_error, "a successful externalization is not an error");
        let output = output.expect("externalized envelope present");
        assert!(output.truncated, "oversized result is marked truncated");
        assert!(output.retrievable, "oversized result is retrievable");
        assert_eq!(output.kind, "tool_output");
        assert_eq!(output.size_bytes, big.len() as i64);
        // Execution data: the full content survives behind the reference.
        let retrieved = engine.tool_results.get(&output.id);
        assert_eq!(
            retrieved.as_deref(),
            Some(big.as_str()),
            "full current-turn content must be retrievable via the reference id"
        );
    }

    /// Invariant 2b (CQ-07): an oversized result stays externalized across
    /// turns — a later turn's first record leaves the earlier projection
    /// untouched and the reference resolvable.
    #[tokio::test]
    async fn oversized_result_stays_externalized_across_turns() {
        let tmp = tempfile::tempdir().unwrap();
        let big = oversized();
        let head: String = big.chars().take(64).collect();
        let turn1 = LlmResponse::default().with_tool_call(call("tc-old", "big_output"));
        let turn2 = LlmResponse::default().with_tool_call(call("tc-new", "small_output"));
        let mut engine = engine_with(
            fixture_registry(&[
                ("big_output", big.clone()),
                ("small_output", "tiny".to_string()),
            ]),
            vec![
                turn1,
                LlmResponse::text("t1 done"),
                turn2,
                LlmResponse::text("t2 done"),
            ],
            "cq07-older-turn",
            tmp.path().to_path_buf(),
        );

        engine.submit_user_message("turn one").await.unwrap();
        let (summary, output, _) = recorded_result(&engine, "tc-old");
        assert!(
            summary.contains("[Large tool output externalized]"),
            "turn one's own oversized result is already externalized"
        );
        let output = output.expect("externalized envelope present");
        assert!(output.retrievable);
        let ref_id = output.id.clone();

        engine.submit_user_message("turn two").await.unwrap();

        let (summary, output, _) = recorded_result(&engine, "tc-old");
        assert!(
            summary.contains("[Large tool output externalized]"),
            "later turns must leave the externalized projection intact, got prefix: {}",
            summary.chars().take(80).collect::<String>()
        );
        assert!(
            summary.contains(&head),
            "externalized preview must retain the head of the content"
        );
        let output = output.expect("externalized envelope present");
        assert!(output.truncated, "oversized result stays marked truncated");
        assert!(output.retrievable, "oversized result stays retrievable");
        assert_eq!(output.id, ref_id, "the reference identity is stable");
        // The full content must survive behind the reference.
        let retrieved = engine.tool_results.get(&output.id);
        assert_eq!(
            retrieved.as_deref(),
            Some(big.as_str()),
            "full content must remain retrievable via the reference id"
        );

        // The new turn's own small result is untouched.
        let (new_summary, _, _) = recorded_result(&engine, "tc-new");
        assert_eq!(new_summary, "tiny");
    }

    /// Invariant 3: a store write failure never discards content and never
    /// issues a false reference. With the store directory unusable, the
    /// result projects the bounded head+tail failure preview at record time
    /// and the transcript carries `is_error` — a failed archive is a typed
    /// failure, not a clean observation.
    #[tokio::test]
    async fn store_failure_projects_bounded_typed_error() {
        let tmp = tempfile::tempdir().unwrap();
        // `<state-dir>` as a regular FILE makes every store write fail
        // (`create_dir_all` on `<file>/tool-results` errors), exercising the
        // fallback path deterministically on every platform.
        std::fs::write(tmp.path().join(".rightkit"), "not a directory").unwrap();
        let big = oversized();
        let head: String = big.chars().take(64).collect();
        let tail: String = big.chars().skip(big.len() - 64).collect();
        let turn1 = LlmResponse::default().with_tool_call(call("tc-fail", "big_output"));
        let turn2 = LlmResponse::default().with_tool_call(call("tc-next", "small_output"));
        let mut engine = engine_with(
            fixture_registry(&[
                ("big_output", big.clone()),
                ("small_output", "tiny".to_string()),
            ]),
            vec![
                turn1,
                LlmResponse::text("t1 done"),
                turn2,
                LlmResponse::text("t2 done"),
            ],
            "cq07-store-failure",
            tmp.path().to_path_buf(),
        );

        engine.submit_user_message("turn one").await.unwrap();
        let (summary, output, is_error) = recorded_result(&engine, "tc-fail");
        assert!(
            summary.contains("could not be archived"),
            "store failure must project the bounded failure preview, got prefix: {}",
            summary.chars().take(80).collect::<String>()
        );
        assert!(
            summary.contains(&head) && summary.contains(&tail),
            "fallback preview must retain head and tail of the content"
        );
        assert!(
            summary.len() < big.len(),
            "fallback preview must be bounded, not the full payload re-inlined"
        );
        assert!(
            is_error,
            "failed externalization must set the tool disposition to error"
        );
        let output = output.expect("envelope present");
        assert!(
            !output.retrievable,
            "a failed write must not issue a retrieval reference"
        );
        assert_eq!(
            output.kind, "tool_output_error",
            "failed externalization must not look like a successful preview"
        );

        engine.submit_user_message("turn two").await.unwrap();

        let (summary, output, is_error) = recorded_result(&engine, "tc-fail");
        assert!(
            summary.contains("could not be archived"),
            "the typed failure preview must survive later turns, got prefix: {}",
            summary.chars().take(80).collect::<String>()
        );
        assert!(is_error, "the typed failure disposition must persist");
        let output = output.expect("envelope present");
        assert!(!output.retrievable);
        assert_eq!(output.kind, "tool_output_error");
    }
}
