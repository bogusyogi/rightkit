use super::*;
#[path = "engine_tool_dispatch_concurrent.rs"]
pub(in crate::engine) mod concurrent;
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
// ToolResult is the dispatch error contract shared by every caller; boxing it would change them all.
#[allow(clippy::result_large_err)]
fn required_process_launch_authority(
    tools: &rightkit_agent_tools::coding_tools::ToolRegistry,
    tool_name: &str,
    args: &serde_json::Value,
    workspace_root: &std::path::Path,
) -> Result<Option<ProcessLaunchAuthority>, Box<rightkit_agent_tools::coding_tools::ToolResult>> {
    let Some(tool) = tools.get(tool_name) else {
        return Ok(None);
    };
    let Some(kind) = tool.process_resource_kind_for(args) else {
        return Ok(None);
    };
    let governor = rightkit_agent_tools::coding_tools::process_resource_authority::require_process_resource_governor()
        .map_err(|error| {
            Box::new(rightkit_agent_tools::coding_tools::ToolResult::error(
                error.to_string(),
            ))
        })?;
    if !governor.capability_available(kind) {
        return Err(Box::new(
            rightkit_agent_tools::coding_tools::ToolResult::error(format!(
                "{}: tool '{}' requires unavailable resource {kind:?}",
                rightkit_sandbox::execution::EffectExecutionError::ResourceLimitsMissing,
                tool_name
            )),
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

/// True when any of `definitions` (already filtered to enabled, local-plugin
/// `PreToolCall` middleware) matches `call`. Reuses the exact same matcher
/// the pre-tool middleware applies on the sequential path, so a call
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
