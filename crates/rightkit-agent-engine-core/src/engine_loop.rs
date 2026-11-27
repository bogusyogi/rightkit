use super::engine_helpers::{
    inspect_stable_system_prefix, StableSystemPrefixOutcome, STABLE_SYSTEM_KERNEL_MAX_BYTES,
};
use super::session_controls::InterjectionDirective;
use super::*;
use futures::StreamExt;

const STEER_INTERJECTION_NOTE: &str = "The user sent a message while you were working:";
const REPROMPT_INTERJECTION_NOTE: &str = "The user replaced this turn with a new request:";
const INTERJECTION_REMINDER: &str =
    "Make sure to complete any unfinished tasks from previous turns.";
const MAX_INTERJECTION_BYTES: usize = 25_000;

#[derive(Debug, Default, Clone, Copy)]
struct StreamPersistence {
    text_bytes: usize,
    reasoning_bytes: usize,
    assistant_precommitted: bool,
    /// Reasoning tokens itemized by the provider for this response (subset of
    /// `completion_tokens`); carried here so `LlmResponse` keeps its shape.
    reasoning_tokens: u32,
    /// The provider's own charge for this response's request(s), when it reported one.
    provider_cost_usd: Option<f64>,
    /// The rate-limit budget the provider reported on this response's headers.
    rate_limit: Option<rightkit_agent_engine_protocol::event::RateLimitObserved>,
    /// Request start to the first streamed text, reasoning or tool call, and to
    /// the end of the stream, in milliseconds; `None` when the reply was not streamed.
    ttft_ms: Option<u64>,
    total_ms: Option<u64>,
}

fn frame_live_interjection(note: &str, text: &str) -> String {
    // TUI callers already send the canonical envelope. Preserve it rather
    // than nesting one synthetic user turn inside another.
    if text.starts_with(STEER_INTERJECTION_NOTE) || text.starts_with(REPROMPT_INTERJECTION_NOTE) {
        return text.to_string();
    }
    let bounded = bound_interjection_text(text);
    format!("{note}\n<user_query>\n{bounded}\n</user_query>\n{INTERJECTION_REMINDER}")
}

/// UTF-8-safe truncation at the interjection byte cap. The durable steering
/// event carries this bounded text — never the unbounded raw control payload.
pub(crate) fn bound_interjection_text(text: &str) -> String {
    let mut bounded = text.to_string();
    if bounded.len() > MAX_INTERJECTION_BYTES {
        let mut end = MAX_INTERJECTION_BYTES;
        while end > 0 && !bounded.is_char_boundary(end) {
            end -= 1;
        }
        bounded.truncate(end);
        bounded.push_str("... [truncated]");
    }
    bounded
}

async fn wait_for_interrupt_signal(mut rx: tokio::sync::watch::Receiver<bool>) {
    loop {
        if *rx.borrow() {
            return;
        }
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// Wait for the stream pump task to end. A pump stopped with `abort` ends as cancelled, the
/// expected end; one that panicked is logged, since nothing else reports it.
async fn settle_stream_pump(pump: tokio::task::JoinHandle<()>) {
    if let Err(error) = pump.await {
        if error.is_panic() {
            tracing::error!(%error, "the provider stream pump panicked");
        }
    }
}

impl SessionEngine {
    /// One user turn, from submission to the run's final outcome (including
    /// every early error exit). Wall-clock duration of the whole span is
    /// recorded per turn via `rightkit_agent_telemetry::record_turn_latency`, so
    /// this wrapper owns the timer and the inner body keeps every return path.
    pub async fn submit_user_message(&mut self, text: &str) -> Result<String, EngineError> {
        use tracing::Instrument as _;
        let turn_started = std::time::Instant::now();
        // PERF-08: the whole turn runs under a `turn` span (target `rightkit_turn`) with the session id and a
        // fresh turn id; neither the prompt nor the answer is ever a field.
        let turn_span = tracing::info_span!(target: "rightkit_turn", "turn", session_id = %self.session_id, turn_id = %rightkit_agent_engine_protocol::uuid_v7());
        let result = async {
            tracing::info!(target: "rightkit_turn", "turn started");
            let result = self.submit_user_message_turn(text).await;
            tracing::info!(target: "rightkit_turn", outcome = if result.is_ok() { "ok" } else { "error" }, "turn finished");
            result
        }
        .instrument(turn_span)
        .await;
        rightkit_agent_telemetry::record_turn_latency(rightkit_agent_telemetry::TurnLatencyEvent {
            latency_ms: u64::try_from(turn_started.elapsed().as_millis()).unwrap_or(u64::MAX),
        });
        // ENG-73: `submit_user_message_turn` is the single call site of
        // `begin_run_changes` (which unconditionally starts the idle-grace
        // turn via `idle_grace.begin_turn()` before it can fail on a missing
        // sink), and every one of its many internal `return Ok`/`return Err`
        // paths — normal completion, provider/tool errors, and user
        // interruption alike — resolves into this single awaited `result`.
        // Ending the turn here, once, on every path back through this
        // funnel, is equivalent to catching each exit individually but
        // cannot miss one.
        self.end_active_run();
        self.prune_resolved_approval_markers();
        // Tools rule 58 `Interrupt`: fires once when a turn is interrupted
        // (user stop, live cancel, or interrupt during a provider request). A
        // latched persistence failure stops the loop through the same path but
        // is not a user interrupt, so it fires nothing.
        if matches!(result, Err(EngineError::Interrupted))
            && self.conduit.persistence_failure().is_none()
        {
            self.run_lifecycle_hooks("Interrupt", &[]).await;
        }
        result
    }

    async fn submit_user_message_turn(&mut self, text: &str) -> Result<String, EngineError> {
        self.begin_run_changes().map_err(EngineError::RunChanges)?;
        self.assurance_context_injected = false;
        self.sentinel_run_id = None;
        if self.sentinel_client.is_none() {
            self.sentinel_client = crate::sentinel_client::SentinelClient::spawn_from_env()
                .await
                .map_err(|error| EngineError::Assurance(error.to_string()))?
                .map(|client| Arc::new(tokio::sync::Mutex::new(client)));
        }
        let sentinel_host_authorized = if let Some(client) = &self.sentinel_client {
            client.lock().await.host_authorized()
        } else {
            false
        };
        if let Some(broker) = &self.assurance_broker {
            let task_id = format!(
                "{}--{}",
                self.session_id,
                rightkit_agent_engine_protocol::uuid_v7()
            );
            let trace_id = format!("trace--{}", rightkit_agent_engine_protocol::uuid_v7());
            let scope = vec![self.workspace_root.to_string_lossy().into_owned()];
            if let Ok(mut broker) = broker.lock() {
                broker
                    .begin_task(
                        self.session_id.clone(),
                        task_id,
                        trace_id,
                        text,
                        scope,
                        Vec::new(),
                    )
                    .map_err(|error| EngineError::Assurance(error.to_string()))?;
            }
            // Only an authoritative gate gives the model something to act on. A
            // degraded line carried fresh ids every engine start and, never being
            // persisted, broke the resumed provider prefix; it stays in the broker.
            if !self.assurance_context_injected && sentinel_host_authorized {
                if let Ok(broker) = broker.lock() {
                    if let Some(contract) = broker.contract() {
                        let gate_state = "Beacon-authoritative";
                        self.transcript.push_system(format!(
                            "[Assurance contract] task_id={} trace_id={} issuer={} receipts=host-issued; gate={gate_state}; repair=exact-missing-evidence",
                            contract.task_id,
                            contract.trace_id,
                            crate::assurance_broker::host_issuer(),
                        ));
                        self.assurance_context_injected = true;
                    }
                }
            }
        }
        if let Some(client) = &self.sentinel_client {
            let contract = self
                .assurance_broker
                .as_ref()
                .and_then(|broker| {
                    broker
                        .lock()
                        .ok()
                        .and_then(|broker| broker.contract().cloned())
                })
                .ok_or_else(|| {
                    EngineError::Assurance("task contract unavailable for Sentinel".into())
                })?;
            let mut client = client.lock().await;
            let assessed = client
                .beacon(serde_json::json!({
                    "operation": "assess",
                    "summary": format!("{} host task started", rightkit_agent_config::app_name()),
                    "task_id": contract.task_id,
                    "session_id": contract.session_id,
                    "acceptance_criteria": contract.criteria.iter().map(|criterion| criterion.criterion_id.clone()).collect::<Vec<_>>(),
                }))
                .await
                .map_err(|error| EngineError::Assurance(error.to_string()))?;
            let run_id = assessed
                .get("run_id")
                .and_then(serde_json::Value::as_str)
                .filter(|run_id| !run_id.is_empty())
                .ok_or_else(|| {
                    EngineError::Assurance("Sentinel assess returned no run_id".into())
                })?;
            self.sentinel_run_id = Some(run_id.to_string());
        }
        let session_span_id = self.record_runtime_span(
            MiddlewarePhase::SessionStart,
            "completed",
            None,
            None,
            serde_json::json!({
                "channel": self.mcp_channel,
                // Runtime-span payloads cross the daemon/API boundary; keep
                // them stable across Windows and POSIX consumers.
                "workspaceRoot": self.workspace_root.to_string_lossy().replace('\\', "/"),
                "executionBackend": self.execution_backend,
                "executionWorkspaceId": self.execution_workspace_id,
                "executionNetworkProfile": self.execution_network_profile,
            }),
        );
        // Adopt any live permission-mode change before this turn's tool calls.
        self.refresh_permission_mode();
        self.refresh_compaction_policy();
        // Paired with `note_session_end` in `record_session_end`, which every path out of this
        // turn calls: a short-lived process waits for the pair before exiting.
        rightkit_hooks::agent_hooks::note_session_start();
        if !self.session_start_hooks_fired {
            self.session_start_hooks_fired = true;
            self.publish_hook(rightkit_hooks::agent_hooks::HookEvent::SessionStart {
                session_id: self.session_id.clone(),
            });
            self.run_lifecycle_hooks("SessionStart", &[]).await;
        }
        self.publish_hook(rightkit_hooks::agent_hooks::HookEvent::UserPromptSubmit {
            session_id: self.session_id.clone(),
        });
        // Blocking UserPromptSubmit hooks: a deny drops the prompt and ends the
        // turn with the hook's reason; additionalContext is appended to the
        // prompt the model sees.
        let mut prompt_context: Vec<String> = Vec::new();
        if let Some(bus) = self.hook_bus.clone() {
            let hook_session_id = self.session_id.clone();
            let prompt_env: String = text.chars().take(32 * 1024).collect();
            let report = bus
                .run_event_hooks(
                    "UserPromptSubmit",
                    &hook_session_id,
                    None,
                    None,
                    &[("RIGHTKIT_HOOK_PROMPT", prompt_env)],
                )
                .await;
            for error in &report.errors {
                // A host that sets `hook_errors_as_tracing` gets a `tracing` event on the
                // `rightkit_hook` target (filterable with `RUST_LOG`); otherwise stderr.
                if crate::host_options::hook_errors_as_tracing() {
                    tracing::warn!(target: "rightkit_hook", event = "UserPromptSubmit", %error, "hook error (non-blocking)");
                } else {
                    eprintln!("[hook] UserPromptSubmit error (non-blocking): {error}");
                }
            }
            if report.is_denied() {
                let message = format!(
                    "[UserPromptSubmit hook blocked this prompt] {}",
                    report.reason.as_deref().unwrap_or("blocked by hook")
                );
                let delta = message.clone();
                let blocked_sid = self.session_id.clone();
                self.emit(|seq| ProtocolEvent::AssistantDelta {
                    session_id: blocked_sid,
                    seq,
                    created_at: now_iso(),
                    text: delta,
                    is_reasoning: false,
                });
                self.record_session_end(
                    session_span_id.as_deref(),
                    "blocked",
                    Some("UserPromptSubmit hook"),
                );
                return Ok(message);
            }
            prompt_context = report.additional_context;
        }
        self.add_triggered_skill_hint("UserPromptSubmit", |ctx| {
            ctx.set("message", text);
            ctx.trigger_type = Some("message".into());
            ctx.trigger_payload = Some(text.to_string());
        });
        if prompt_context.is_empty() {
            self.transcript.push_user(text);
        } else {
            let context = crate::tool_result_store::bound_gate_text(
                &mut self.tool_results,
                &prompt_context.join("\n"),
            );
            self.transcript.push_user(format!(
                "{text}\n\n[Hook context: UserPromptSubmit]\n{context}"
            ));
        }

        let sid = self.session_id.clone();

        // Meltdown detection: if the agent loops (same tool action repeatedly),
        // reset its context rather than burn the whole iteration budget.
        // Thresholds are now lifted into the unified LoopDetectorConfig
        // (parity P-3) so all three loop-guard axes share one config
        // surface. Defaults reproduce the pre-P-3 hard-coded values.
        // A host that sets `reset_loop_counters_per_turn` bounds a loop within one user turn
        // (sessions-and-turn-loop rule 32: "a turn is also bounded across iterations"): a new
        // user message starts every repeat count afresh, so an identical call the person asks
        // for again in a later turn is not taken for the loop of an earlier one, and a resumed
        // process (which starts with fresh counters) behaves exactly as the live one did.
        // Otherwise the counts of the engine's lifetime carry over from turn to turn.
        if crate::host_options::reset_loop_counters_per_turn() {
            self.stall_guardrail.reset();
            self.idempotency_axis.reset();
        }
        let cfg = &self.loop_detector_config;
        let mut mop = crate::mop::MopDetector::new(cfg.mop_window_size, cfg.mop_repeat_threshold);
        let mut loop_detector = crate::mop::ToolLoopDetector::from_config(cfg);
        let mut post_turn_gate = crate::post_turn_gate::PostTurnGateState::default();
        // Turn-scoped pending required verification (PL-007): armed by the
        // post-edit fan-out when a required verifier produced no verdict or
        // failed, rechecked once per completion attempt against the latest
        // bytes, and reset here for every new user turn.
        let mut pending_verify = crate::verify_fanout::PendingVerification::new();
        let mut pending_loop_warning: Option<String> = None;
        // VG-002: provider-reported context rejection enters this bounded,
        // reactive recovery state. It is intentionally independent from the
        // proactive compaction policy applied after successful responses.
        let mut reactive_overflow_recovery =
            crate::overflow_recovery::ReactiveOverflowRecovery::default();
        // A provider that finishes with reasoning only (no visible text, no
        // tool call) produced nothing to act on. Re-ask a bounded number of
        // times instead of ending the turn on an error the user did not cause.
        let mut empty_completion_retries: u32 = 0;
        // A stream that fails midway (cut connection, in-stream overloaded event) is
        // re-asked a bounded number of times; reset once a provider response arrives.
        let mut stream_retries: u32 = 0;
        // A reply cut off at the provider's output limit is re-asked a bounded
        // number of times in smaller pieces; the counter resets after a good reply.
        let mut output_limit_retries: u32 = 0;
        // The output cap a truncated reply is re-asked with: `None` leaves the
        // provider's default; each output-limit retry doubles it.
        let mut output_cap: Option<u32> = None;
        // [F2] Count tool activity since the last goal continuation, so a turn
        // that did nothing flips the no-progress cap toward `blocked`.
        let mut tool_calls_since_continuation: usize = 0;

        // FEATURE-MATRIX par §#141: iteration budget with refund. The loop
        // is now driven by `self.iteration_budget` instead of a raw
        // `max_iterations` counter. Programmatic turns (read-only tool
        // chaining with no user-visible text) cost 0.25; non-programmatic
        // turns cost 1.0. When the budget is exhausted the loop falls
        // through to `attempt_graceful_summary` (one final turn with
        // tools stripped) before the existing `RunFailed` regression
        // path fires. The budget is per user turn: the engine outlives turns,
        // so a fresh balance is set here like the other turn-scoped state.
        self.iteration_budget =
            crate::engine::iteration_budget::IterationBudget::new(self.iteration_budget.max());
        let mut iteration: usize = 0;
        while !self.iteration_budget.is_exhausted() {
            // Persistence failure is latched on the conduit: no further
            // semantic work. Stop through the interrupt path, but emit nothing
            // durable (it would be refused and would recurse into the failure).
            // The facts of the previous iteration (tool results) are still on their way to
            // storage; wait for them, so one that cannot be stored stops the turn here and is
            // not sent to the provider first.
            self.conduit
                .durable_settled(std::time::Duration::from_secs(5))
                .await;
            if let Some(reason) = self.conduit.persistence_failure() {
                tracing::error!(%reason, "stopping owned execution: session persistence failed");
                self.record_session_end(session_span_id.as_deref(), "interrupted", None);
                return Err(EngineError::Interrupted);
            }
            // Check for user-requested interrupt.
            if let Some(rx) = &self.interrupt_rx {
                if *rx.borrow() {
                    self.emit(|seq| ProtocolEvent::RunInterrupted {
                        session_id: sid.clone(),
                        seq,
                        created_at: now_iso(),
                        reason: Some("interrupted by user".into()),
                    });
                    self.record_session_end(session_span_id.as_deref(), "interrupted", None);
                    return Err(EngineError::Interrupted);
                }
            }

            // HCTX-002: promote asynchronous host state before any provider
            // request for this iteration. Fixed cross-source order is
            // prompt -> context -> world-state -> tool-catalog -> MCP pair
            // -> steering. MCP promotion carries manager + catalog together.
            // The provider client owns each already-started request body, so
            // updates observed during a stream cannot mutate that request.
            if let Some(rx) = &mut self.provider_boundary_rx {
                let promotion = self.provider_boundary_projection.promote_pending(rx);
                if let Some((manager, tools)) = promotion.mcp {
                    // MCP manager and its visible catalog are one boundary
                    // promotion: an in-flight provider request keeps its
                    // captured manager/catalog pair immutable.
                    self.mcp = Some(manager);
                    self.llm.replace_tool_registry(tools.clone());
                    self.tools = tools;
                }
                if let Some(tools) = promotion.tools {
                    self.llm.replace_tool_registry(tools.clone());
                    self.tools = tools;
                }
            }
            self.provider_boundary_projection
                .append_promoted_to_transcript(&mut self.transcript);

            self.observe_host_revision(
                "promotion",
                Some(&self.provider_boundary_projection.generation().to_string()),
            );

            // Drain live controls in FIFO order, then consume their typed
            // meaning. Plain API strings are parsed at this boundary so raw
            // control text can never become an ordinary user turn.
            let interjections: Vec<InterjectionDirective> =
                if let Some(steer_rx) = &mut self.steer_rx {
                    std::iter::from_fn(|| steer_rx.try_recv().ok()).collect()
                } else {
                    vec![]
                }
                .into_iter()
                .map(InterjectionDirective::parse)
                .collect();
            let interjection_count = interjections.len();
            for (index, directive) in interjections.into_iter().enumerate() {
                match directive {
                    InterjectionDirective::Steer(text) => {
                        let framed = frame_live_interjection(STEER_INTERJECTION_NOTE, &text);
                        self.transcript.push_user(&framed);
                        let preview: String = framed.chars().take(80).collect();
                        let steering_text = bound_interjection_text(&text);
                        self.emit(|seq| ProtocolEvent::HumanSteeringInjected {
                            session_id: sid.clone(),
                            seq,
                            created_at: now_iso(),
                            injected_by: "steer".into(),
                            content_preview: preview,
                            text: Some(steering_text),
                        });
                    }
                    InterjectionDirective::Reprompt(text) => {
                        let framed = frame_live_interjection(REPROMPT_INTERJECTION_NOTE, &text);
                        self.transcript.push_user(&framed);
                        let preview: String = framed.chars().take(80).collect();
                        let steering_text = bound_interjection_text(&text);
                        self.emit(|seq| ProtocolEvent::HumanSteeringInjected {
                            session_id: sid.clone(),
                            seq,
                            created_at: now_iso(),
                            injected_by: "reprompt".into(),
                            content_preview: preview,
                            text: Some(steering_text),
                        });
                        // A replacement supersedes controls queued behind it.
                        if let Some(steer_rx) = &mut self.steer_rx {
                            while steer_rx.try_recv().is_ok() {}
                        }
                        break;
                    }
                    InterjectionDirective::Cancel { reason } => {
                        let mut discarded = interjection_count.saturating_sub(index + 1);
                        if let Some(steer_rx) = &mut self.steer_rx {
                            while steer_rx.try_recv().is_ok() {
                                discarded += 1;
                            }
                        }
                        let disposition = reason
                            .as_deref()
                            .unwrap_or("cancelled by live interjection")
                            .to_string();
                        self.record_runtime_span(
                            MiddlewarePhase::SessionEnd,
                            "cancelled",
                            None,
                            session_span_id.as_deref(),
                            serde_json::json!({
                                "disposition": "cancelled",
                                "source": "live_interjection",
                                "reason": disposition.clone(),
                                "queuedDiscarded": discarded,
                            }),
                        );
                        self.emit(|seq| ProtocolEvent::RunInterrupted {
                            session_id: sid.clone(),
                            seq,
                            created_at: now_iso(),
                            reason: Some(format!(
                                "live interjection cancelled turn: {disposition}"
                            )),
                        });
                        self.record_session_end(
                            session_span_id.as_deref(),
                            "interrupted",
                            Some(&disposition),
                        );
                        return Err(EngineError::Interrupted);
                    }
                }
            }
            repair_dangling_tool_calls(&mut self.transcript);
            if let Some(warning) = pending_loop_warning.take() {
                let nudge = format!(
                    "[Loop warning] {warning}. {}",
                    crate::stall_strategy::STRATEGY_RESET_DIRECTIVE
                );
                let created_at = now_iso();
                self.transcript
                    .push_nudge_at(nudge.clone(), created_at.clone());
                // The model reads the nudge as a message, so it is part of the history a resumed
                // process must send: the stored observation carries its exact text and time
                // (`LOOP_WARNING_NUDGE_FIELD`), and replay puts it back at this position.
                let mut fields = std::collections::HashMap::new();
                fields.insert(
                    crate::transcript::LOOP_WARNING_NUDGE_FIELD.to_string(),
                    nudge,
                );
                self.emit(|seq| ProtocolEvent::OrchestrationObservationRecorded {
                    session_id: sid.clone(),
                    seq,
                    created_at: created_at.clone(),
                    observation: rightkit_agent_engine_protocol::OrchestrationObservation {
                        id: format!("loop-warning-{iteration}"),
                        kind: "loop_warning".into(),
                        subject: sid.clone(),
                        summary: format!(
                            "loop warning at iteration {iteration}; injected strategy-reset directive"
                        ),
                        fields,
                        created_at: created_at.clone(),
                    },
                });
            }
            inject_pending_view_images_with_state(
                &mut self.transcript,
                &mut self.pending_view_images,
            );

            // Tier 2 (proactive) compaction runs at the iteration boundary,
            // before the next provider request is built, against the live
            // capacity and the usage the previous response reported. At this
            // point the previous step's assistant message and tool results are
            // already in the transcript, so the summary covers them and
            // tool_use/tool_result pairing is never split. A failure
            // propagates as a typed `compaction_failed` observation without
            // mutating the full transcript; there is no authoritative
            // statistical fallback.
            if self.auto_compaction_due() {
                if let Err(error) = self
                    .execute_compaction_command(crate::compactor::CompactionCommandV1 {
                        id: crate::compactor::next_compaction_command_id(),
                        kind: crate::compactor::CompactionCommandKindV1::Automatic,
                        instructions: None,
                        response: None,
                    })
                    .await
                {
                    tracing::warn!(
                        session_id = %sid,
                        error = %error,
                        "automatic compaction failed closed; full transcript retained"
                    );
                }
            }

            // "Approve and clear context" on a plan card (A209-2): the tool result is in the transcript
            // now, so the earlier conversation can be replaced by the plan before the next request.
            if let Some(plan) = self
                .plan_exit
                .as_ref()
                .and_then(|plan_exit| plan_exit.take_context_reset())
            {
                self.apply_plan_context_reset(&plan);
            }

            // Budget check.
            if self.usage.exceeds_budget(self.budget_ceiling_usd) {
                let (used, ceiling) = (self.usage.cost_usd, self.budget_ceiling_usd);
                self.emit(|seq| ProtocolEvent::BudgetExceeded {
                    session_id: sid.clone(),
                    seq,
                    created_at: now_iso(),
                    scope: "session".into(),
                    current_usd: used,
                    ceiling_usd: ceiling,
                    hint: "raise the ceiling to continue".into(),
                });
                self.record_session_end(
                    session_span_id.as_deref(),
                    "failed",
                    Some("budget exceeded"),
                );
                return Err(EngineError::Llm("budget exceeded".into()));
            }

            // Build the message list. P2-6: production stable-prefix / history /
            // volatile ordering. Stable tier = leading pinned System messages +
            // day (hashed via `pinned_system_prefix_as_stable`). History = the
            // transcript's user/assistant/tool messages. Volatile overlay (goal
            // reminder, per-turn memory) serializes AFTER every cache-stable
            // field and never changes cache identity — so the goal is volatile,
            // not pinned, and appended after history, not prepended.
            let goal_text = self
                .goal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let messages_slice = self.transcript.messages();
            // A host that sets `loop_progress_resets_repeats` tells the loop detector what the
            // last calls returned, so a repeat that brought a new result counts as progress.
            if crate::host_options::loop_progress_resets_repeats() {
                for message in messages_slice.iter().rev() {
                    match message {
                        TranscriptMessage::Tool {
                            tool_call_id,
                            summary,
                            is_error,
                            ..
                        } => {
                            if loop_detector.observe_result(tool_call_id, summary, *is_error) {
                                mop.reset();
                            }
                        }
                        // A loop warning pushed just above sits after the results it follows.
                        TranscriptMessage::User { .. } => {}
                        _ => break,
                    }
                }
            }

            // Hermes three-tier split: observe only the stable tier (identity +
            // day). The volatile prefix argument carries any per-turn memory or
            // timestamp context the caller wants to log alongside the
            // observation; CacheTracker::observe_stable(_with_tools) takes no
            // volatile argument, so it is EXCLUDED from the hash, so per-turn volatile
            // churn cannot break cache stability across turns on the same day.
            //
            // Book A Lane 2 (final item): tool_defs previously entered no
            // cache key here, so tool-schema churn broke the provider's
            // prompt cache with no metric showing it. `tool_digest` reuses
            // the canonical-ordered serialization already implemented in
            // `rightkit_agent_coding_tools::registry::lazy_loading` (byte-stable across
            // turns with an identical tool set) so a tool-set change is
            // folded into the same observation and reported as a cache
            // Break rather than a silent Hit. `CacheTracker` (rightkit-cache)
            // takes the digest as a caller-supplied `Option<&str>` instead of
            // depending on `rightkit-agent-coding-tools` itself.
            // CR-33: use the SAME accumulated promotion state the provider
            // client puts on the wire (see `LlmClient::promoted_tool_state_snapshot`),
            // never a fresh/empty one — otherwise a promotion changes the
            // wire prefix with no corresponding CacheTracker digest
            // transition, defeating invariant F.
            let tool_promoted_state = self.llm.promoted_tool_state_snapshot();
            let tool_defs_for_digest =
                rightkit_agent_coding_tools::registry::tool_defs_for_messages(
                    &self.tools,
                    &tool_promoted_state,
                );
            let tool_digest = serde_json::to_vec(&tool_defs_for_digest).ok().map(|bytes| {
                format!(
                    "{:016x}",
                    crate::cache::fnv1a(&String::from_utf8_lossy(&bytes))
                )
            });
            let llm_turn_id = format!("iteration-{iteration}");
            let stable_prefix_key = match inspect_stable_system_prefix(
                messages_slice,
                &self.day_key,
            ) {
                StableSystemPrefixOutcome::NoPinnedPrefix => None,
                StableSystemPrefixOutcome::WithinBudget(stable) => {
                    let (status, reason) = self
                        .cache_tracker
                        .observe_stable_with_tools(&stable, tool_digest.as_deref());
                    if matches!(status, CacheStatus::Break) {
                        tracing::warn!(
                            "[cache] stable system prompt cache key ({}) changed for session {} (reason={})",
                            stable.compute(),
                            self.session_id,
                            reason.as_deref().unwrap_or("unattributed-prefix-drift"),
                        );
                    }
                    Some(stable.compute())
                }
                StableSystemPrefixOutcome::Oversized { byte_len } => {
                    // Project instructions routinely exceed the kernel budget.
                    // Send the complete prefix and drop the prefix-content
                    // identity: it must never describe bytes the provider did
                    // not receive, and the turn itself must not fail. Provider
                    // prompt-cache routing (OpenAI `prompt_cache_key`, ChatGPT
                    // `session_id`) only needs a key that is stable across the
                    // turns of one session, so fall back to a session-scoped
                    // key. Without it every follow-up turn is routed cold and
                    // reports zero cached input.
                    tracing::warn!(
                        session_id = %self.session_id,
                        actual_bytes = byte_len,
                        max_bytes = STABLE_SYSTEM_KERNEL_MAX_BYTES,
                        "stable system kernel exceeds byte ceiling; using session-scoped prompt-cache routing key"
                    );
                    self.record_runtime_span(
                        MiddlewarePhase::PreLlmCall,
                        "degraded",
                        Some(&llm_turn_id),
                        session_span_id.as_deref(),
                        serde_json::json!({
                            "iteration": iteration,
                            "reason": "stable_kernel_oversized",
                            "actualBytes": byte_len,
                            "maxBytes": STABLE_SYSTEM_KERNEL_MAX_BYTES,
                            "stablePrefixKey": serde_json::Value::Null,
                            "sessionRoutingKey": true,
                        }),
                    );
                    Some(format!(
                        "session:{:016x}",
                        crate::cache::fnv1a(&self.session_id)
                    ))
                }
            };
            // The model that answers this request is the one the route names now: the tool calls of
            // the reply may end plan mode (and so change the route) before the reply is recorded.
            let request_prices = self.price_resolver.as_ref().map(|resolver| resolver());
            // The 1-hour write costs 2x the input price against 1.25x for 5 minutes, and only pays
            // when the prefix is read again after a pause longer than 5 minutes. Only a person
            // pauses that long between requests, so only a root session with a human client
            // attached (TUI, desktop, web) asks for an hour. A subagent, a headless `run`, a
            // one-shot review and any other unattended loop sends its next request within seconds:
            // the 5-minute write covers every read it will make.
            let request_cache_retention = cache_retention_for(
                self.subagent_role.is_some(),
                self.conduit.human_subscriber_count(),
            );
            let request_metadata = LlmRequestMetadata {
                cache_retention: Some(request_cache_retention),
                request_identity: Some(crate::llm::LlmRequestIdentity::new(
                    self.session_id.clone(),
                    self.active_run_id().to_string(),
                    llm_turn_id.clone(),
                    u64::try_from(iteration).expect("request iteration fits u64"),
                )),
                stable_prefix_key,
                max_output_tokens: output_cap,
                provider_boundary_generation: Some(self.provider_boundary_projection.generation()),
                context_fingerprint: self.provider_boundary_projection.context_fingerprint(),
                retry_notifier: {
                    let conduit = self.conduit.clone();
                    let session_id = sid.clone();
                    rightkit_agent_engine_protocol::RetryNotifier::new(move |notice| {
                        let seq = conduit.next_seq();
                        conduit.publish(ProtocolEvent::ProviderRetrying {
                            session_id: session_id.clone(),
                            seq,
                            created_at: now_iso(),
                            provider: notice.provider,
                            reason: notice.reason,
                            attempt: notice.attempt,
                            max_attempts: notice.max_attempts,
                            delay_ms: notice.delay_ms,
                        });
                    })
                },
            };

            // Build a request-only multimodal history. The provider client
            // accounts the exact wire body and selects route-specific
            // operational water marks. Stored transcript state remains
            // authoritative and unchanged.
            let image_policy = self.llm.image_budget_policy();
            let prepared_image_history =
                crate::image_eviction::prepare_image_history_with_accounting(
                    messages_slice,
                    image_policy,
                    |candidate| {
                        self.llm
                            .image_request_accounting(candidate, &request_metadata)
                    },
                );
            let image_budget = prepared_image_history.evidence;
            if image_budget.inline_images > 0 {
                self.emit(|seq| ProtocolEvent::OrchestrationObservationRecorded {
                    session_id: sid.clone(),
                    seq,
                    created_at: now_iso(),
                    observation: crate::image_eviction::image_budget_observation(
                        &sid,
                        iteration,
                        image_budget,
                        now_iso(),
                    ),
                });
            }
            let messages_slice = prepared_image_history
                .messages
                .as_deref()
                .unwrap_or(messages_slice);

            let llm_span_id = self.record_runtime_span(
                MiddlewarePhase::PreLlmCall,
                "completed",
                Some(&llm_turn_id),
                session_span_id.as_deref(),
                serde_json::json!({
                    "iteration": iteration,
                    "messageCount": messages_slice.len(),
                    "hasGoalReminder": goal_text.is_some(),
                    "runId": self.active_run_id(),
                    "providerBoundaryGeneration": request_metadata.provider_boundary_generation,
                    "contextFingerprint": request_metadata.context_fingerprint.as_deref(),
                }),
            );
            // Projection already owns the provider request vector. Append the
            // volatile goal overlay there so goal-bearing turns do not first
            // clone the entire stable transcript into a second temporary Vec.
            let mut stream_messages = self
                .wire_projection
                .project_with_window(messages_slice, self.compaction_policy.context_window_tokens);
            stream_messages.retain(|message| message.is_provider_visible());
            let injection_budget = self.compaction_policy.context_window_tokens.map(|window| {
                // Blank inline base64 images and charge a flat per-image cost,
                // as the compaction trigger does, so images don't read as text.
                let used = serde_json::to_string(
                    &crate::image_eviction::without_inline_image_payloads(&stream_messages),
                )
                .map(|body| body.len() as u64 / 4)
                .unwrap_or(0)
                .saturating_add(
                    crate::image_eviction::inline_image_count(&stream_messages)
                        .saturating_mul(crate::compaction_policy::IMAGE_TOKEN_COST),
                );
                crate::provider_turn_boundary::remaining_injection_budget(
                    window,
                    used,
                    self.compaction_policy.reserve_tokens.unwrap_or(0),
                )
            });
            self.provider_boundary_projection
                .inject_into_budgeted(&mut stream_messages, injection_budget);
            if let Some(ref goal) = goal_text {
                stream_messages.push(TranscriptMessage::System {
                    id: "goal-reminder".into(),
                    created_at: now_iso(),
                    content: format!("{}{goal}", crate::GOAL_REMINDER_PREFIX),
                    pinned: false,
                });
            }
            // VG-001 phase 1: snapshot exactly what the provider will see so a
            // cache-read collapse on the response can be attributed.
            let promoted_for_cache = tool_promoted_state.scoped_for_request();
            self.record_prompt_cache_request(
                &stream_messages,
                &tool_defs_for_digest,
                request_metadata.stable_prefix_key.as_deref(),
                &promoted_for_cache,
            );
            // Tools rule 58: `PreModelRequest` fires before each provider
            // request and `PostModelRequest` after it, with its outcome.
            self.run_lifecycle_hooks(
                "PreModelRequest",
                &[("RIGHTKIT_HOOK_ITERATION", iteration.to_string())],
            )
            .await;
            let transcript_len_before_request = self.transcript.len();
            // PERF-08: one `provider_request` span per request (target `rightkit_provider`), session id and iteration only.
            let provider_span = tracing::info_span!(target: "rightkit_provider", "provider_request", session_id = %sid, iteration = iteration);
            let model_request = tracing::Instrument::instrument(
                async {
                    tracing::info!(target: "rightkit_provider", "provider request started");
                    let outcome = self
                        .collect_streaming_response(
                            &stream_messages,
                            &request_metadata,
                            &sid,
                            &mut loop_detector,
                            &mut pending_loop_warning,
                            iteration,
                        )
                        .await;
                    tracing::info!(target: "rightkit_provider", outcome = if outcome.is_ok() { "ok" } else { "error" }, "provider request finished");
                    outcome
                },
                provider_span,
            )
            .await;
            let model_request_status = match &model_request {
                Ok(_) => "ok",
                Err(EngineError::Interrupted) => "interrupted",
                Err(_) => "error",
            };
            self.run_lifecycle_hooks(
                "PostModelRequest",
                &[
                    ("RIGHTKIT_HOOK_ITERATION", iteration.to_string()),
                    ("RIGHTKIT_HOOK_STATUS", model_request_status.to_string()),
                ],
            )
            .await;
            let (mut response, streamed_tool_calls, streamed_results, stream_persistence) =
                match model_request {
                    Ok(result) => {
                        // One completed provider round trip per LLM response,
                        // attributed to the session (the stable task identity
                        // available on every turn). Failed provider requests
                        // return `Err` below and are deliberately NOT counted:
                        // `RoundTripEvent` carries no success flag, so counting
                        // them here would silently inflate per-task totals.
                        rightkit_agent_telemetry::record_round_trip(
                            rightkit_agent_telemetry::RoundTripEvent { task: sid.clone() },
                        );
                        result
                    }
                    Err(EngineError::Interrupted) => {
                        self.emit(|seq| ProtocolEvent::RunInterrupted {
                            session_id: sid.clone(),
                            seq,
                            created_at: now_iso(),
                            reason: Some("interrupted by user during provider request".into()),
                        });
                        self.record_session_end(session_span_id.as_deref(), "interrupted", None);
                        return Err(EngineError::Interrupted);
                    }
                    Err(EngineError::Llm(e)) => {
                        if is_empty_completion_error(&e)
                            && empty_completion_retries < MAX_EMPTY_COMPLETION_RETRIES
                        {
                            empty_completion_retries += 1;
                            // Retry with the failed attempt's history: the
                            // streamed reasoning-only assistant would end in a
                            // thinking block, which Anthropic rejects (#453).
                            self.transcript
                                .pop_reasoning_only_assistant(transcript_len_before_request);
                            if empty_completion_retries == 1 {
                                self.transcript.push_nudge(EMPTY_REPLY_HINT);
                            }
                            self.emit(|seq| ProtocolEvent::ProviderRetrying {
                                session_id: sid.clone(),
                                seq,
                                created_at: now_iso(),
                                provider: String::new(),
                                reason: "empty response".into(),
                                attempt: empty_completion_retries,
                                max_attempts: MAX_EMPTY_COMPLETION_RETRIES,
                                delay_ms: 0,
                            });
                            continue;
                        }
                        if crate::failure::is_retryable_stream_failure(&e)
                            && stream_retries < MAX_STREAM_RETRIES
                        {
                            // An Anthropic history may not end in a thinking block.
                            self.transcript
                                .pop_reasoning_only_assistant(transcript_len_before_request);
                            // The failed attempt's partial reply and its 'never dispatched'
                            // results stay: they are durable events and every tool_use
                            // keeps its tool_result.
                            if self.transcript.len() > transcript_len_before_request {
                                self.transcript.push_nudge(STREAM_INTERRUPTED_HINT);
                            }
                            stream_retries += 1;
                            let delay = crate::failure::stream_retry_delay(stream_retries);
                            let delay_ms = delay.as_millis() as u64;
                            self.emit(|seq| ProtocolEvent::ProviderRetrying {
                                session_id: sid.clone(),
                                seq,
                                created_at: now_iso(),
                                provider: String::new(),
                                reason: "stream interrupted".into(),
                                attempt: stream_retries,
                                max_attempts: MAX_STREAM_RETRIES,
                                delay_ms,
                            });
                            self.record_runtime_span(
                                MiddlewarePhase::PostLlmCall,
                                "error",
                                Some(&llm_turn_id),
                                llm_span_id.as_deref(),
                                serde_json::json!({
                                    "iteration": iteration,
                                    "error": e,
                                    "kind": "stream_retry",
                                    "attempt": stream_retries,
                                }),
                            );
                            if let Some(rx) = self.interrupt_rx.clone() {
                                tokio::select! {
                                    () = tokio::time::sleep(delay) => {}
                                    () = wait_for_interrupt_signal(rx) => {
                                        self.emit(|seq| ProtocolEvent::RunInterrupted {
                                            session_id: sid.clone(),
                                            seq,
                                            created_at: now_iso(),
                                            reason: Some("interrupted by user while waiting to retry the provider request".into()),
                                        });
                                        self.record_session_end(session_span_id.as_deref(), "interrupted", None);
                                        return Err(EngineError::Interrupted);
                                    }
                                }
                            } else {
                                tokio::time::sleep(delay).await;
                            }
                            continue;
                        }
                        if crate::overflow_recovery::is_reactive_context_overflow(&e) {
                            // The call performs the transition; a refused one ends the run.
                            if reactive_overflow_recovery.overflow_detected().is_err() {
                                self.record_session_end(
                                    session_span_id.as_deref(),
                                    "failed",
                                    Some(&e),
                                );
                                return Err(EngineError::Llm(e));
                            }
                            if reactive_overflow_recovery.retry_budget_exhausted() {
                                reactive_overflow_recovery.exhaust();
                                self.emit_overflow_recovery_outcome(
                                    crate::overflow_recovery::OverflowRecoveryOutcome::Exhausted,
                                    &e,
                                );
                                self.record_runtime_span(
                                    MiddlewarePhase::PostLlmCall,
                                    "error",
                                    Some(&llm_turn_id),
                                    llm_span_id.as_deref(),
                                    serde_json::json!({
                                        "iteration": iteration,
                                        "error": e,
                                        "kind": "context_overflow",
                                        "reactiveRecovery": "exhausted",
                                    }),
                                );
                                self.emit(|seq| ProtocolEvent::RunFailed {
                                    session_id: sid.clone(),
                                    seq,
                                    created_at: now_iso(),
                                    error: e.clone(),
                                    kind: "context_overflow".into(),
                                    hint:
                                        "context overflow persisted after bounded reactive recovery"
                                            .into(),
                                });
                                self.record_session_end(
                                    session_span_id.as_deref(),
                                    "failed",
                                    Some(&e),
                                );
                                return Err(EngineError::Llm(e));
                            }

                            if reactive_overflow_recovery.begin_compaction().is_err() {
                                self.record_session_end(
                                    session_span_id.as_deref(),
                                    "failed",
                                    Some(&e),
                                );
                                return Err(EngineError::Llm(e));
                            }
                            match self
                                .execute_compaction_command(crate::compactor::CompactionCommandV1 {
                                    id: crate::compactor::next_compaction_command_id(),
                                    // Reactive provider-overflow recovery may
                                    // have no pending retry decision; it is a
                                    // fresh automatic compaction in that case.
                                    kind: crate::compactor::CompactionCommandKindV1::Overflow,
                                    instructions: None,
                                    response: None,
                                })
                                .await
                            {
                                Ok(_) => match reactive_overflow_recovery.compaction_complete() {
                                    Ok(
                                        crate::overflow_recovery::ReactiveOverflowState::Retrying {
                                            attempt,
                                        },
                                    ) => {
                                        tracing::warn!(
                                            session_id = %sid,
                                            iteration,
                                            attempt,
                                            "provider rejected context; retrying after reactive compaction"
                                        );
                                        self.emit(|seq| ProtocolEvent::ProviderRetrying {
                                            session_id: sid.clone(),
                                            seq,
                                            created_at: now_iso(),
                                            provider: String::new(),
                                            reason: "context overflow, compacted".into(),
                                            attempt: u32::from(attempt),
                                            max_attempts: u32::from(
                                                crate::overflow_recovery::MAX_REACTIVE_RETRIES,
                                            ),
                                            delay_ms: 0,
                                        });
                                        continue;
                                    }
                                    Ok(_) | Err(_) => {}
                                },
                                Err(error) => {
                                    tracing::warn!(
                                        session_id = %sid,
                                        error = %error,
                                        "reactive context-overflow compaction failed closed"
                                    );
                                    self.emit_overflow_recovery_outcome(
                                        crate::overflow_recovery::OverflowRecoveryOutcome::CompactionFailed,
                                        &e,
                                    );
                                }
                            }
                        }
                        let kind = classify_failure(&e);
                        let (kind_str, hint_str) = (kind.as_str(), kind.hint());
                        self.record_runtime_span(
                            MiddlewarePhase::PostLlmCall,
                            "error",
                            Some(&llm_turn_id),
                            llm_span_id.as_deref(),
                            serde_json::json!({
                                "iteration": iteration,
                                "error": e,
                                "kind": kind_str,
                            }),
                        );
                        self.emit(|seq| ProtocolEvent::RunFailed {
                            session_id: sid.clone(),
                            seq,
                            created_at: now_iso(),
                            error: e.clone(),
                            kind: kind_str.into(),
                            hint: hint_str.into(),
                        });
                        self.record_session_end(session_span_id.as_deref(), "failed", Some(&e));
                        return Err(EngineError::Llm(e));
                    }
                    Err(error) => return Err(error),
                };
            // A provider response after reactive compaction closes that
            // recovery cycle. Later provider calls in this same user turn
            // start with a fresh bounded retry budget.
            if reactive_overflow_recovery.state().is_recovering() {
                self.emit_overflow_recovery_outcome(
                    crate::overflow_recovery::OverflowRecoveryOutcome::Recovered,
                    "",
                );
            }
            reactive_overflow_recovery.recovered();
            stream_retries = 0;
            self.record_runtime_span(
                MiddlewarePhase::PostLlmCall,
                "completed",
                Some(&llm_turn_id),
                llm_span_id.as_deref(),
                serde_json::json!({
                    "iteration": iteration,
                    "promptTokens": response.prompt_tokens,
                    "completionTokens": response.completion_tokens,
                    "toolCallCount": response.tool_calls.len(),
                    "hasText": !response.text.is_empty(),
                    "hasReasoning": response.reasoning.as_ref().is_some_and(|value| !value.is_empty()),
                    "runId": self.active_run_id(),
                    "providerBoundaryGeneration": request_metadata.provider_boundary_generation,
                    "contextFingerprint": request_metadata.context_fingerprint.as_deref(),
                }),
            );
            if !streamed_tool_calls {
                // EFF-009: completeness gate runs before the safety-finish
                // strip and before any approval/dispatch consideration.
                // Only a `Complete` generation may keep its tool calls.
                enforce_generation_completeness_gate(&mut response);
                strip_safety_terminated_tool_calls(&mut response);
                for dropped in crate::transcript_plumbing::drop_cross_turn_duplicate_tool_calls(
                    &mut response.tool_calls,
                    &self.transcript,
                ) {
                    if !response.text.is_empty() {
                        response.text.push_str("\n\n");
                    }
                    response
                        .text
                        .push_str(&crate::transcript_plumbing::duplicate_tool_call_note(
                            &dropped,
                        ));
                }
            }
            if response.has_tool_calls() {
                // The streamed path already ran the detector before dispatch.
                match if streamed_tool_calls {
                    None
                } else {
                    loop_detector.record(&response.tool_calls)
                } {
                    Some(crate::mop::LoopSignal::Warn(message)) => {
                        pending_loop_warning = Some(message);
                    }
                    Some(crate::mop::LoopSignal::HardStop(message)) => {
                        let reason = crate::stall_strategy::stalled_reason(&message);
                        self.emit(|seq| ProtocolEvent::OrchestrationObservationRecorded {
                            session_id: sid.clone(),
                            seq,
                            created_at: now_iso(),
                            observation: rightkit_agent_engine_protocol::OrchestrationObservation {
                                id: format!("stalled-{iteration}"),
                                kind: "stalled".into(),
                                subject: sid.clone(),
                                summary: reason,
                                fields: std::collections::HashMap::new(),
                                created_at: now_iso(),
                            },
                        });
                        response.tool_calls.clear();
                        if response.text.trim().is_empty() {
                            response.text = format!(
                                "[Forced stop: loop detector halted tool execution. {message}.]"
                            );
                        } else {
                            response.text.push_str(&format!(
                                "\n\n[Forced stop: loop detector halted tool execution. {message}.]"
                            ));
                        }
                    }
                    None => {}
                }
                // Idempotency-aware third axis (parity P-3). Reuses
                // the legacy `ToolLoopDetector` shape counts so the
                // exact-repeat count is shared. Only emits Warn
                // (never HardStop); the warning text is appended to
                // the response so the model sees it in-context on the
                // next turn (Hermes pattern).
                if let Some(crate::mop::LoopSignal::Warn(message)) = self.idempotency_axis.record(
                    &response.tool_calls,
                    self.tools.as_ref(),
                    loop_detector.shape_counts_view(),
                ) {
                    if pending_loop_warning.is_none() {
                        pending_loop_warning = Some(message);
                    }
                }
            }
            if !streamed_tool_calls {
                self.enforce_worker_tool_limit(&mut response);
            }

            if let Some(prices) = &request_prices {
                self.usage.use_prices(prices.as_ref());
            }
            self.usage.record_with_retention(
                response.prompt_tokens,
                response.completion_tokens,
                response.cache_read_tokens,
                response.cache_creation_tokens,
                request_cache_retention,
            );
            // A charge the provider itself reported is the cost of this request: it replaces the
            // estimate just added.
            if let Some(billed) = stream_persistence.provider_cost_usd {
                self.usage.bill_last_record(billed);
            }
            self.usage
                .record_reasoning(stream_persistence.reasoning_tokens);
            self.observe_compaction_accounting();
            // ENG-85: surface explicit cache-breakpoint hit/cost accounting
            // at the point the turn loop has just learned this turn's cache
            // token split, so anything observing this session's tracing
            // output can see whether provider-side prompt caching is
            // actually paying off without needing a new engine<->providers
            // crate dependency (the accounting math itself lives in
            // `rightkit_llm::agent_providers::pricing`, consumed today by callers that
            // already depend on that crate, e.g.
            // `host_mobile_runtime::llm`).
            if response.cache_read_tokens > 0 || response.cache_creation_tokens > 0 {
                tracing::debug!(
                    session_id = %sid,
                    cache_read_tokens = response.cache_read_tokens,
                    cache_creation_tokens = response.cache_creation_tokens,
                    "prompt cache accounting for this turn"
                );
            }
            let (ptok, ctok, cost, ctx_tok) = (
                // Cumulative session totals (uncached input / output), like the
                // cache and cost counters carried by the same event.
                self.usage.prompt_tokens.min(i32::MAX as u64) as i32,
                self.usage.completion_tokens.min(i32::MAX as u64) as i32,
                self.usage.cost_usd,
                self.usage.context_tokens as i32,
            );
            let cost_known = self.usage.cost_is_known();
            let cost_billed = self.usage.cost_is_billed();
            self.emit(|seq| ProtocolEvent::UsageUpdated {
                session_id: sid.clone(),
                seq,
                created_at: now_iso(),
                prompt_tokens: ptok,
                completion_tokens: ctok,
                cost_usd: cost,
                context_tokens: ctx_tok,
                context_percent: self.usage.context_percent(0),
                cache_read_tokens: self.usage.cache_read_tokens as i32,
                cache_creation_tokens: self.usage.cache_creation_tokens as i32,
                cost_known,
                cost_billed,
            });
            self.emit_turn_usage(
                &sid,
                response.prompt_tokens,
                response.cache_read_tokens,
                response.cache_creation_tokens,
                response.completion_tokens,
                stream_persistence.reasoning_tokens,
                stream_persistence.ttft_ms,
                stream_persistence.total_ms,
                stream_persistence.rate_limit,
            );
            // VG-001 phase 2: did the provider reuse the cached prefix?
            self.check_prompt_cache_break(
                &sid,
                response.prompt_tokens,
                response.cache_read_tokens,
                response.cache_creation_tokens,
            );

            if let Some(reasoning) = response
                .reasoning
                .as_ref()
                .filter(|value| !value.is_empty())
            {
                let unpersisted = reasoning
                    .get(stream_persistence.reasoning_bytes..)
                    .unwrap_or(reasoning);
                if !unpersisted.is_empty() {
                    self.emit(|seq| ProtocolEvent::AssistantDelta {
                        session_id: sid.clone(),
                        seq,
                        created_at: now_iso(),
                        text: unpersisted.to_string(),
                        is_reasoning: true,
                    });
                }
            }

            let unpersisted_text = response
                .text
                .get(stream_persistence.text_bytes..)
                .unwrap_or(&response.text);
            if !unpersisted_text.is_empty() {
                let t = unpersisted_text.to_string();
                self.emit(|seq| ProtocolEvent::AssistantDelta {
                    session_id: sid.clone(),
                    seq,
                    created_at: now_iso(),
                    text: t,
                    is_reasoning: false,
                });
            }

            if !stream_persistence.assistant_precommitted {
                self.transcript.push_assistant(
                    response.text.clone(),
                    response.reasoning.clone(),
                    response.tool_calls.clone(),
                );
                self.transcript
                    .tag_last_assistant_origin(self.llm.message_origin());
            }

            // On the non-streaming path the calls have not run yet: a loop directive or a
            // compaction now would sit between the assistant's tool calls and their results,
            // so it waits until the results are in (see the dispatch below).
            let tools_pending_dispatch = response.has_tool_calls() && !streamed_tool_calls;
            if !tools_pending_dispatch {
                self.observe_response_tool_activity(
                    &response,
                    &sid,
                    iteration,
                    &mut mop,
                    &mut tool_calls_since_continuation,
                )
                .await;
            }

            // FEATURE-MATRIX par §#141: charge the iteration budget for this
            // turn. Programmatic turns (read-only tool chaining with no
            // user-visible text) cost 0.25; non-programmatic turns cost 1.0.
            // Classification runs AFTER `enforce_worker_tool_limit` so the
            // budget reflects the actual tool calls the engine will dispatch
            // (including any warning text that was appended to the response).
            let programmatic = crate::engine::iteration_budget::classify_programmatic(
                &response,
                self.tools.as_ref(),
            );
            let cost = self.iteration_budget.charge(programmatic);
            iteration += 1;

            // Refund a genuine no-op turn (parity P-3): the model produced
            // neither tool calls nor user-visible text, so it made no progress
            // and mutated nothing — it should not burn the user's turn budget.
            // Programmatic (read-only tool-chaining) turns are deliberately NOT
            // refunded here: they still cost the fractional 0.25 so varied
            // read-only exploration stays bounded by the budget (the loop guard
            // only catches *repeated* calls, not varied ones). The consecutive
            // empty-turn case is separately bounded by post_turn_gate's
            // ForceFinish cap, so this refund cannot enable an infinite loop.
            if !response.has_tool_calls() && response.text.trim().is_empty() {
                // A no-op turn (no tool calls, no assistant text) gives its charge back.
                self.iteration_budget.refund(cost);
            }

            // A reply cut off at the provider output limit had its tool calls
            // withheld by the completeness gate above. Ask the model to continue
            // in smaller pieces a bounded number of times, then fail loudly
            // instead of ending the turn on a truncated "answer".
            let output_limited = !response.has_tool_calls()
                && !streamed_tool_calls
                && matches!(
                    crate::tool_dispatch_plumbing::classify_generation_completeness(&response),
                    crate::tool_dispatch_plumbing::GenerationCompleteness::IncompleteGeneration
                );
            if output_limited && output_limit_retries < MAX_OUTPUT_LIMIT_RETRIES {
                output_limit_retries += 1;
                // Re-asking at the same cap truncates the same way: double it
                // (the provider default is 16,000 with thinking), within a
                // ceiling most current models accept.
                output_cap = Some(
                    output_cap
                        .map_or(OUTPUT_CAP_FIRST_RETRY, |cap| cap.saturating_mul(2))
                        .min(OUTPUT_CAP_CEILING),
                );
                // The retry is the engine's recovery, not the model's progress:
                // give back this turn's charge so `--max-turns` still leaves room
                // for it. An empty-text turn was already refunded above.
                if !response.text.trim().is_empty() {
                    // Output-limit retry: the reply was cut off, its tool calls withheld.
                    self.iteration_budget.refund(cost);
                }
                // The truncated text was shown: complete it as interim before the
                // retry hint joins the transcript.
                let reasoning = self.current_turn_reasoning();
                self.emit_assistant_completed(
                    &sid,
                    rightkit_agent_engine_protocol::AssistantMessagePhase::Interim,
                    response.text.clone(),
                    reasoning,
                );
                self.transcript.push_nudge(OUTPUT_LIMIT_RETRY_HINT);
                self.emit(|seq| ProtocolEvent::ProviderRetrying {
                    session_id: sid.clone(),
                    seq,
                    created_at: now_iso(),
                    provider: String::new(),
                    reason: "output limit reached".into(),
                    attempt: output_limit_retries,
                    max_attempts: MAX_OUTPUT_LIMIT_RETRIES,
                    delay_ms: 0,
                });
                if !response.text.trim().is_empty() {
                    self.emit(|seq| ProtocolEvent::AssistantDelta {
                        session_id: sid.clone(),
                        seq,
                        created_at: now_iso(),
                        text: "\n\n".into(),
                        is_reasoning: false,
                    });
                }
                continue;
            } else if output_limited {
                let error = format!(
                    "the model's reply was cut off at the provider output limit {} times in a row; any tool calls in those replies were not run",
                    MAX_OUTPUT_LIMIT_RETRIES + 1
                );
                self.record_runtime_span(
                    MiddlewarePhase::PostLlmCall,
                    "error",
                    Some(&llm_turn_id),
                    llm_span_id.as_deref(),
                    serde_json::json!({
                        "iteration": iteration,
                        "error": error,
                        "kind": INCOMPLETE_OUTPUT_KIND,
                    }),
                );
                self.emit(|seq| ProtocolEvent::RunFailed {
                    session_id: sid.clone(),
                    seq,
                    created_at: now_iso(),
                    error: error.clone(),
                    kind: INCOMPLETE_OUTPUT_KIND.into(),
                    hint: INCOMPLETE_OUTPUT_HINT.into(),
                });
                self.record_session_end(session_span_id.as_deref(), "failed", Some(&error));
                return Err(EngineError::Llm(error));
            } else {
                // Both retry counters mean "since the last complete reply": an
                // output-limited reply does not reset the empty-reply counter.
                output_limit_retries = 0;
                output_cap = None;
                empty_completion_retries = 0;
            }

            if !response.has_tool_calls() && !streamed_tool_calls {
                match self
                    .handle_assistant_completion(
                        &response,
                        &sid,
                        session_span_id.as_deref(),
                        &mut post_turn_gate,
                        tool_calls_since_continuation > 0,
                        &mut pending_verify,
                    )
                    .await
                {
                    super::engine_loop_completion::AssistantCompletionOutcome::Continue => {
                        // The loop asks the model again (verification gate,
                        // steer): the next streamed text is a new paragraph,
                        // not a continuation of this one.
                        if !response.text.trim().is_empty() {
                            self.emit(|seq| ProtocolEvent::AssistantDelta {
                                session_id: sid.clone(),
                                seq,
                                created_at: now_iso(),
                                text: "\n\n".into(),
                                is_reasoning: false,
                            });
                        }
                        continue;
                    }
                    super::engine_loop_completion::AssistantCompletionOutcome::ContinueAfterGoal => {
                        tool_calls_since_continuation = 0;
                        // Per-tool-signature stall guardrail (FEATURE-MATRIX
                        // par §#120) — a progress turn clears the slate so
                        // consecutive identical calls reset at 0 in the next
                        // turn; this preserves Hermes's controller semantics
                        // and matches the spec's "varied turn clears the slate".
                        self.stall_guardrail.reset();
                        continue;
                    }
                    super::engine_loop_completion::AssistantCompletionOutcome::Finished(
                        final_text,
                    ) => {
                        let review_key = pinned_system_prefix_as_stable(self.transcript.messages(), &self.day_key).map(|s| s.compute());
                        self.maybe_spawn_self_review_fork(final_text.clone(), review_key);
                        return Ok(final_text);
                    }
                }
            }

            if streamed_tool_calls {
                match self
                    .post_streaming_tool_checks(
                        &mut response,
                        iteration,
                        &sid,
                        &mut post_turn_gate,
                        &streamed_results,
                        &mut pending_verify,
                    )
                    .await
                {
                    super::engine_loop_dispatch::ToolDispatchOutcome::Proceed => continue,
                    super::engine_loop_dispatch::ToolDispatchOutcome::Continue => continue,
                    super::engine_loop_dispatch::ToolDispatchOutcome::Finished(warning) => {
                        self.complete_forced_stop(&sid, &warning, session_span_id.as_deref());
                        return Ok(warning);
                    }
                }
            }

            match self
                .dispatch_response_tools_and_post_checks(
                    &mut response,
                    iteration,
                    &sid,
                    &mut post_turn_gate,
                    &mut pending_verify,
                )
                .await
            {
                super::engine_loop_dispatch::ToolDispatchOutcome::Proceed => {
                    self.observe_response_tool_activity(
                        &response,
                        &sid,
                        iteration,
                        &mut mop,
                        &mut tool_calls_since_continuation,
                    )
                    .await;
                }
                super::engine_loop_dispatch::ToolDispatchOutcome::Continue => {
                    self.observe_response_tool_activity(
                        &response,
                        &sid,
                        iteration,
                        &mut mop,
                        &mut tool_calls_since_continuation,
                    )
                    .await;
                    continue;
                }
                super::engine_loop_dispatch::ToolDispatchOutcome::Finished(warning) => {
                    self.complete_forced_stop(&sid, &warning, session_span_id.as_deref());
                    return Ok(warning);
                }
            }
        }

        // Budget exhausted: try the graceful summary turn before falling
        // through to `RunFailed`. The summary attempt is a genuinely
        // reachable code path — wired into the real turn loop, not a
        // struct that's never called. See
        // `iteration_budget::attempt_graceful_summary` for the
        // tool-strip + text-completion semantics.
        self.attempt_budget_exhaustion_summary(&sid, session_span_id.as_deref())
            .await
    }

    async fn collect_streaming_response(
        &mut self,
        messages: &[TranscriptMessage],
        metadata: &LlmRequestMetadata,
        sid: &str,
        loop_detector: &mut crate::mop::ToolLoopDetector,
        pending_loop_warning: &mut Option<String>,
        iteration: usize,
    ) -> Result<
        (
            crate::llm::LlmResponse,
            bool,
            std::collections::HashMap<String, rightkit_agent_coding_tools::ToolResult>,
            StreamPersistence,
        ),
        EngineError,
    > {
        let request_started = std::time::Instant::now();
        // Opening a stream can wait on response headers before a stream exists.
        // Keep that provider request under the same interrupt authority as reads.
        let opening = self.llm.stream_with_metadata(messages, metadata);
        let opened = if let Some(interrupt_rx) = self.interrupt_rx.clone() {
            tokio::select! {
                biased;
                () = wait_for_interrupt_signal(interrupt_rx) => {
                    return Err(EngineError::Interrupted);
                }
                result = opening => result,
            }
        } else {
            opening.await
        };
        let stream = match opened {
            Ok(stream) => stream,
            Err(error) if error == "streaming seam unavailable" => {
                let completion = self.llm.complete_with_metadata(messages, metadata);
                let result = if let Some(interrupt_rx) = self.interrupt_rx.clone() {
                    tokio::select! {
                        result = completion => result,
                        () = wait_for_interrupt_signal(interrupt_rx) => {
                            return Err(EngineError::Interrupted);
                        }
                    }
                } else {
                    completion.await
                };
                return result
                    .map(|response| {
                        (
                            response,
                            false,
                            std::collections::HashMap::new(),
                            StreamPersistence::default(),
                        )
                    })
                    .map_err(EngineError::Llm);
            }
            Err(error) => return Err(EngineError::Llm(error)),
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let pump = tokio::spawn(async move {
            let mut stream = stream;
            while let Some(item) = stream.next().await {
                if tx.send(item).await.is_err() {
                    break;
                }
            }
        });
        let mut response = crate::llm::LlmResponse::default();
        let mut streamed_tool_calls = false;
        let mut turn_results = std::collections::HashMap::new();
        let mut in_flight: futures::stream::FuturesOrdered<
            crate::engine::tool_dispatch::concurrent::StreamingToolFuture,
        > = futures::stream::FuturesOrdered::new();
        let mut assistant_id: Option<String> = None;
        let mut stream_persistence = StreamPersistence::default();
        let mut stream_finished = false;
        let mut worker_calls = 0usize;
        // Any completed futures are recorded only after the finalized
        // assistant envelope is available; EFF-009 also prevents starting a
        // new model tool call before the provider's terminal signal.
        let mut completed_streaming_results: Vec<(
            ToolCall,
            rightkit_agent_coding_tools::ToolResult,
        )> = Vec::new();
        // CR-08/EFF-009: side effects must never survive an errored,
        // interrupted, or incomplete model stream with no committed assistant
        // tool-call envelope. Keep every call in issue order until the stream
        // reaches a terminal event, the completeness gate runs, and the
        // assistant message is finalized. An incomplete generation clears the
        // deferred queue before any approval or execution can begin.
        let mut deferred_calls: Vec<ToolCall> = Vec::new();

        loop {
            let item = if in_flight.is_empty() {
                if let Some(interrupt_rx) = self.interrupt_rx.clone() {
                    tokio::select! {
                        item = rx.recv() => item,
                        () = wait_for_interrupt_signal(interrupt_rx) => {
                            pump.abort();
                            settle_stream_pump(pump).await;
                            // The stream may already have yielded tool calls
                            // before this interrupt won the receive race. Keep
                            // their authorization envelope ahead of any
                            // completed result, then preserve every started
                            // invocation's terminal disposition.
                            while let Some((call, result)) = in_flight.next().await {
                                completed_streaming_results.push((call, result));
                            }
                            if let Some(id) = assistant_id.as_deref() {
                                self.finalize_streaming_assistant(id, &response);
                            }
                            self.record_undispatched_streaming_calls(
                                sid,
                                deferred_calls.iter().cloned(),
                                "the turn was interrupted before the generation completed",
                                &mut turn_results,
                            );
                            for (call, result) in completed_streaming_results {
                                self.record_streaming_concurrent_result(
                                    call,
                                    result,
                                    &mut turn_results,
                                ).await;
                            }
                            return Err(EngineError::Interrupted);
                        }
                    }
                } else {
                    rx.recv().await
                }
            } else if let Some(interrupt_rx) = self.interrupt_rx.clone() {
                tokio::select! {
                    biased;
                    completed = in_flight.next() => {
                        if let Some((call, result)) = completed {
                            completed_streaming_results.push((call, result));
                        }
                        continue;
                    }
                    item = rx.recv() => item,
                    () = wait_for_interrupt_signal(interrupt_rx) => {
                        pump.abort();
                        settle_stream_pump(pump).await;
                        // P1-8: every started invocation records terminal
                        // disposition even across interruption — drain the
                        // read-only in-flight futures and record their actual
                        // results before returning, so none disappear without
                        // a terminal transcript entry.
                        while let Some((call, result)) = in_flight.next().await {
                            completed_streaming_results.push((call, result));
                        }
                        if let Some(id) = assistant_id.as_deref() {
                            self.finalize_streaming_assistant(id, &response);
                        }
                        self.record_undispatched_streaming_calls(
                            sid,
                            deferred_calls.iter().cloned(),
                            "the turn was interrupted before the generation completed",
                            &mut turn_results,
                        );
                        for (call, result) in completed_streaming_results {
                            self.record_streaming_concurrent_result(
                                call,
                                result,
                                &mut turn_results,
                            ).await;
                        }
                        return Err(EngineError::Interrupted);
                    }
                }
            } else {
                tokio::select! {
                    biased;
                    completed = in_flight.next() => {
                        if let Some((call, result)) = completed {
                            completed_streaming_results.push((call, result));
                        }
                        continue;
                    }
                    item = rx.recv() => item,
                }
            };
            let Some(item) = item else { break };
            let event = match item {
                Ok(event) => event,
                Err(error) => {
                    pump.abort();
                    settle_stream_pump(pump).await;
                    while let Some((call, result)) = in_flight.next().await {
                        completed_streaming_results.push((call, result));
                    }
                    self.persist_stream_checkpoint(
                        sid,
                        &response,
                        &mut assistant_id,
                        &mut stream_persistence,
                    );
                    self.record_undispatched_streaming_calls(
                        sid,
                        deferred_calls.iter().cloned(),
                        "the model stream ended before the generation reached a terminal event",
                        &mut turn_results,
                    );
                    for (call, result) in completed_streaming_results {
                        self.record_streaming_concurrent_result(call, result, &mut turn_results)
                            .await;
                    }
                    return Err(EngineError::Llm(error));
                }
            };
            if stream_persistence.ttft_ms.is_none()
                && matches!(
                    event,
                    crate::llm::LlmStreamEvent::Text(_)
                        | crate::llm::LlmStreamEvent::Reasoning(_)
                        | crate::llm::LlmStreamEvent::ToolCall(_)
                )
            {
                stream_persistence.ttft_ms = Some(request_started.elapsed().as_millis() as u64);
            }
            match event {
                crate::llm::LlmStreamEvent::Response(full) => {
                    let tool_calls = std::mem::take(&mut response.tool_calls);
                    response = full;
                    response.tool_calls.splice(0..0, tool_calls);
                }
                crate::llm::LlmStreamEvent::Text(text) => {
                    response.text.push_str(&text);
                    self.persist_stream_checkpoint(
                        sid,
                        &response,
                        &mut assistant_id,
                        &mut stream_persistence,
                    );
                }
                crate::llm::LlmStreamEvent::Reasoning(text) => {
                    response
                        .reasoning
                        .get_or_insert_with(String::new)
                        .push_str(&text);
                    self.persist_stream_checkpoint(
                        sid,
                        &response,
                        &mut assistant_id,
                        &mut stream_persistence,
                    );
                }
                crate::llm::LlmStreamEvent::ToolCall(mut call) => {
                    streamed_tool_calls = true;
                    let taken: std::collections::HashSet<String> = response
                        .tool_calls
                        .iter()
                        .map(|item| item.id.clone())
                        .collect();
                    if matches!(
                        crate::transcript_plumbing::resolve_reused_tool_call_id(
                            &mut call,
                            &self.transcript,
                            assistant_id.as_deref(),
                            &taken,
                        ),
                        crate::transcript_plumbing::ReusedId::Replay
                    ) {
                        if !response.text.is_empty() {
                            response.text.push_str("\n\n");
                        }
                        response
                            .text
                            .push_str(&crate::transcript_plumbing::duplicate_tool_call_note(&call));
                        continue;
                    }
                    response.tool_calls.push(call.clone());
                    if assistant_id.is_none() {
                        self.transcript.push_assistant("", None, Vec::new());
                        self.transcript
                            .tag_last_assistant_origin(self.llm.message_origin());
                        let message = self
                            .transcript
                            .messages()
                            .last()
                            .expect("assistant just pushed");
                        if let TranscriptMessage::Assistant { id, .. } = message {
                            assistant_id = Some(id.clone());
                        }
                    }
                    if self.max_worker_tool_calls > 0 && is_worker_spawning_tool_call(&call) {
                        worker_calls += 1;
                        if worker_calls > self.max_worker_tool_calls {
                            response.tool_calls.retain(|item| item.id != call.id);
                            let warning = format!(
                                "[Subagent limit: rejected worker-spawning tool call {} after limit {}.]",
                                call.name, self.max_worker_tool_calls
                            );
                            self.record_tool_error(sid, &call, warning.clone(), &mut turn_results);
                            response.text.push_str("\n\n");
                            response.text.push_str(&warning);
                            continue;
                        }
                    }

                    // Sessions rule 32: the fourth identical call in a row is an
                    // Ask with reason `repeated_call`, raised by the permission
                    // path when the call is dispatched.
                    if self.stall_guardrail.record_call(&call).is_ask() {
                        self.repeated_call_ids.insert(call.id.clone());
                    }

                    // EFF-009 is a generation-level gate: no model tool call
                    // may reach approval, ToolCallStarted, or execution until
                    // the provider has emitted its terminal completion signal.
                    // This includes read-only calls. A syntactically valid
                    // call is still a partial model artifact when the stream
                    // later ends with an output-limit reason, so keep every
                    // call deferred until `enforce_generation_completeness_gate`
                    // has classified the finalized response below.
                    deferred_calls.push(call);
                }
                crate::llm::LlmStreamEvent::Usage {
                    prompt_tokens,
                    completion_tokens,
                    cache_read_tokens,
                    cache_creation_tokens,
                } => {
                    if stream_persistence.assistant_precommitted {
                        response.prompt_tokens =
                            response.prompt_tokens.saturating_add(prompt_tokens);
                        response.completion_tokens =
                            response.completion_tokens.saturating_add(completion_tokens);
                        response.cache_read_tokens =
                            response.cache_read_tokens.saturating_add(cache_read_tokens);
                        response.cache_creation_tokens = response
                            .cache_creation_tokens
                            .saturating_add(cache_creation_tokens);
                    } else {
                        response.prompt_tokens = prompt_tokens;
                        response.completion_tokens = completion_tokens;
                        response.cache_read_tokens = cache_read_tokens;
                        response.cache_creation_tokens = cache_creation_tokens;
                    }
                }
                crate::llm::LlmStreamEvent::UsageDetail { reasoning_tokens } => {
                    if stream_persistence.assistant_precommitted {
                        stream_persistence.reasoning_tokens = stream_persistence
                            .reasoning_tokens
                            .saturating_add(reasoning_tokens);
                    } else {
                        stream_persistence.reasoning_tokens = reasoning_tokens;
                    }
                }
                crate::llm::LlmStreamEvent::UsageCost { cost_usd } => {
                    stream_persistence.provider_cost_usd =
                        Some(if stream_persistence.assistant_precommitted {
                            stream_persistence.provider_cost_usd.unwrap_or(0.0) + cost_usd
                        } else {
                            cost_usd
                        });
                }
                crate::llm::LlmStreamEvent::RateLimit {
                    remaining_requests,
                    remaining_tokens,
                    reset_requests_secs,
                    reset_tokens_secs,
                } => {
                    stream_persistence.rate_limit =
                        Some(rightkit_agent_engine_protocol::event::RateLimitObserved {
                            remaining_requests,
                            remaining_tokens,
                            reset_requests_secs,
                            reset_tokens_secs,
                        });
                }
                crate::llm::LlmStreamEvent::OutputLimitCheckpoint {
                    finish_reason,
                    acknowledge,
                } => {
                    response.finish_reason = Some(finish_reason);
                    self.persist_stream_checkpoint(
                        sid,
                        &response,
                        &mut assistant_id,
                        &mut stream_persistence,
                    );
                    acknowledge.notify_one();
                }
                // Display only: a live view of a call still streaming. The call itself waits
                // for the complete generation (EFF-009) and never reads this.
                crate::llm::LlmStreamEvent::ToolArgumentsPartial {
                    id,
                    name,
                    arguments_json,
                } => {
                    self.emit(|seq| ProtocolEvent::ToolArgumentsPartial {
                        session_id: sid.to_string(),
                        seq,
                        created_at: now_iso(),
                        call_id: id,
                        name,
                        arguments_json,
                    });
                }
                crate::llm::LlmStreamEvent::Finished(reason) => {
                    response.finish_reason = reason;
                    stream_finished = true;
                }
            }
        }
        if !stream_finished {
            pump.abort();
            settle_stream_pump(pump).await;
            while let Some((call, result)) = in_flight.next().await {
                completed_streaming_results.push((call, result));
            }
            self.persist_stream_checkpoint(
                sid,
                &response,
                &mut assistant_id,
                &mut stream_persistence,
            );
            self.record_undispatched_streaming_calls(
                sid,
                deferred_calls.iter().cloned(),
                "the model stream ended before the generation reached a terminal event",
                &mut turn_results,
            );
            for (call, result) in completed_streaming_results {
                self.record_streaming_concurrent_result(call, result, &mut turn_results)
                    .await;
            }
            return Err(EngineError::Llm(
                "stream ended before a terminal event".into(),
            ));
        }
        while let Some((call, result)) = in_flight.next().await {
            completed_streaming_results.push((call, result));
        }
        settle_stream_pump(pump).await;
        // Apply the completeness gate before finalizing the assistant
        // envelope or dispatching any deferred call. Clearing the queue
        // makes an incomplete streamed generation tool-free downstream.
        let generation_gate = enforce_generation_completeness_gate(&mut response);
        if generation_gate.is_some() {
            deferred_calls.clear();
            streamed_tool_calls = false;
        }
        // The loop detector must decide before the calls run, not after: a
        // hard stop here drops the repeated calls so none of them executes.
        if !deferred_calls.is_empty() {
            match loop_detector.record(&deferred_calls) {
                Some(crate::mop::LoopSignal::Warn(message)) => {
                    *pending_loop_warning = Some(message);
                }
                Some(crate::mop::LoopSignal::HardStop(message)) => {
                    let reason = crate::stall_strategy::stalled_reason(&message);
                    self.emit(|seq| ProtocolEvent::OrchestrationObservationRecorded {
                        session_id: sid.to_string(),
                        seq,
                        created_at: now_iso(),
                        observation: rightkit_agent_engine_protocol::OrchestrationObservation {
                            id: format!("stalled-{iteration}"),
                            kind: "stalled".into(),
                            subject: sid.to_string(),
                            summary: reason,
                            fields: std::collections::HashMap::new(),
                            created_at: now_iso(),
                        },
                    });
                    deferred_calls.clear();
                    response.tool_calls.clear();
                    streamed_tool_calls = false;
                    if !response.text.trim().is_empty() {
                        response.text.push_str("\n\n");
                    }
                    response.text.push_str(&format!(
                        "[Forced stop: loop detector halted tool execution. {message}.]"
                    ));
                }
                None => {}
            }
        }
        if let Some(id) = assistant_id.as_deref() {
            self.finalize_streaming_assistant(id, &response);
        }
        for (call, result) in completed_streaming_results {
            self.record_streaming_concurrent_result(call, result, &mut turn_results)
                .await;
        }

        // CR-08/EFF-009: only now — after the stream has reached a terminal
        // event, completeness has been established, and the assistant
        // envelope has been finalized — is it safe to dispatch these calls.
        // Execute in issue order. A maximal consecutive run of concurrency-safe
        // (read-only) calls overlaps as one bounded batch, exactly as the
        // non-streaming path does (`engine_loop_dispatch.rs`, `ToolRun::Read`);
        // every other call runs alone. Five parallel reads used to cost five
        // serial executions (audit 2026-11-10, LOOP-01). A user interrupt still
        // aborts before any not-yet-started deferred call runs.
        let mut index = 0usize;
        while index < deferred_calls.len() {
            if self.conduit.persistence_failure().is_some() {
                // Results could not be stored; do not start more owned work.
                return Err(EngineError::Interrupted);
            }
            // A steer or cancel that arrives while earlier calls ran is read before the next
            // call starts, not after the whole reply: the remaining calls are recorded as not
            // run and the next iteration applies the directive.
            if index > 0 && self.steer_is_pending() {
                let unfinished: Vec<ToolCall> = deferred_calls[index..]
                    .iter()
                    .filter(|call| !turn_results.contains_key(&call.id))
                    .cloned()
                    .collect();
                self.record_undispatched_streaming_calls(
                    sid,
                    unfinished,
                    "a new instruction from the user arrived before it started",
                    &mut turn_results,
                );
                break;
            }
            let mut end = index;
            while end < deferred_calls.len()
                && self.streaming_tool_is_concurrent_safe(&deferred_calls[end])
            {
                end += 1;
            }
            let interrupt_rx = self.interrupt_rx.clone();
            if end - index >= 2 {
                let batch: Vec<&ToolCall> = deferred_calls[index..end].iter().collect();
                let overlapped = self.dispatch_concurrent_batch_bounded(&batch, &mut turn_results);
                if let Some(interrupt_rx) = interrupt_rx {
                    tokio::select! {
                        _ = overlapped => {}
                        () = wait_for_interrupt_signal(interrupt_rx) => {
                            // Batch calls without a result were running: they get a
                            // truthful result of their own. Later calls never started.
                            // Materialised first: the filter borrows `turn_results`, and the
                            // body below takes it by `&mut`, so the iterator must not stay live.
                            let interrupted: Vec<_> = deferred_calls[index..end]
                                .iter()
                                .filter(|call| !turn_results.contains_key(&call.id))
                                .cloned()
                                .collect();
                            for call in &interrupted {
                                self.record_tool_error(
                                    sid,
                                    call,
                                    format!(
                                        "Tool call '{}' ({}) was interrupted while it ran: the turn was stopped. \
                                         It only reads, so repeating it is safe, but its result was lost.",
                                        call.id, call.name
                                    ),
                                    &mut turn_results,
                                );
                            }
                            self.record_undispatched_streaming_calls(
                                sid,
                                deferred_calls[end..].iter().cloned(),
                                "the turn was interrupted before dispatch completed",
                                &mut turn_results,
                            );
                            return Err(EngineError::Interrupted);
                        }
                    }
                } else {
                    overlapped.await;
                }
                index = end;
                continue;
            }
            let call = &deferred_calls[index];
            let serial = self.dispatch_streaming_tool_call(call, &mut turn_results);
            if let Some(interrupt_rx) = interrupt_rx {
                tokio::select! {
                    _ = serial => {}
                    () = wait_for_interrupt_signal(interrupt_rx) => {
                        // The call that was running had its future dropped: it needs a
                        // result of its own, or the provider sees a tool call with no answer.
                        if !turn_results.contains_key(&call.id) {
                            self.record_tool_error(
                                sid,
                                call,
                                format!(
                                    "Tool call '{}' ({}) was interrupted while it ran: the turn was stopped. \
                                     It may have partly run, so check the state before repeating it.",
                                    call.id, call.name
                                ),
                                &mut turn_results,
                            );
                        }
                        self.record_undispatched_streaming_calls(
                            sid,
                            deferred_calls[index + 1..].iter().cloned(),
                            "the turn was interrupted before dispatch completed",
                            &mut turn_results,
                        );
                        return Err(EngineError::Interrupted);
                    }
                }
            } else {
                serial.await;
            }
            index += 1;
        }

        stream_persistence.assistant_precommitted = assistant_id.is_some();
        stream_persistence.total_ms = Some(request_started.elapsed().as_millis() as u64);
        Ok((
            response,
            streamed_tool_calls,
            turn_results,
            stream_persistence,
        ))
    }

    /// True while a steer, reprompt or cancel waits in the live-control queue.
    pub(in crate::engine) fn steer_is_pending(&self) -> bool {
        self.steer_rx.as_ref().is_some_and(|rx| !rx.is_empty())
    }

    fn persist_stream_checkpoint(
        &mut self,
        sid: &str,
        response: &crate::llm::LlmResponse,
        assistant_id: &mut Option<String>,
        persistence: &mut StreamPersistence,
    ) {
        if assistant_id.is_none()
            && (!response.text.is_empty()
                || response
                    .reasoning
                    .as_ref()
                    .is_some_and(|value| !value.is_empty())
                || !response.tool_calls.is_empty())
        {
            self.transcript.push_assistant("", None, Vec::new());
            self.transcript
                .tag_last_assistant_origin(self.llm.message_origin());
            let message = self
                .transcript
                .messages()
                .last()
                .expect("assistant just pushed");
            if let TranscriptMessage::Assistant { id, .. } = message {
                *assistant_id = Some(id.clone());
            }
        }
        if let Some(id) = assistant_id.as_deref() {
            self.extend_streaming_assistant(id, response, persistence);
        }

        if let Some(reasoning) = response.reasoning.as_deref() {
            let suffix = reasoning
                .get(persistence.reasoning_bytes..)
                .unwrap_or(reasoning);
            if !suffix.is_empty() {
                let text = suffix.to_string();
                self.emit(|seq| ProtocolEvent::AssistantDelta {
                    session_id: sid.to_string(),
                    seq,
                    created_at: now_iso(),
                    text,
                    is_reasoning: true,
                });
            }
            persistence.reasoning_bytes = reasoning.len();
        }

        let suffix = response
            .text
            .get(persistence.text_bytes..)
            .unwrap_or(&response.text);
        if !suffix.is_empty() {
            let text = suffix.to_string();
            self.emit(|seq| ProtocolEvent::AssistantDelta {
                session_id: sid.to_string(),
                seq,
                created_at: now_iso(),
                text,
                is_reasoning: false,
            });
        }
        persistence.text_bytes = response.text.len();
        persistence.assistant_precommitted = assistant_id.is_some();
    }

    /// P1-8/EFF-009: a deferred call that never reached dispatch still sits in
    /// the finalized assistant envelope, so it must record a terminal
    /// disposition instead of dangling. These calls never started - the
    /// generation-level gate ran (or never ran) before any approval,
    /// ToolCallStarted, or execution could occur.
    pub(in crate::engine) fn record_undispatched_streaming_calls(
        &mut self,
        sid: &str,
        calls: impl IntoIterator<Item = ToolCall>,
        reason: &str,
        turn_results: &mut std::collections::HashMap<
            String,
            rightkit_agent_coding_tools::ToolResult,
        >,
    ) {
        for call in calls {
            self.record_tool_error(
                sid,
                &call,
                format!(
                    "Tool call '{}' ({}) was never dispatched: {reason}. \
                     Treat this as a failed tool result and decide the next safe step.",
                    call.id, call.name
                ),
                turn_results,
            );
        }
    }

    /// Bring the streaming assistant message up to `response` by appending only what is new
    /// since the last checkpoint, so a long reply is not copied again on every delta. Falls back
    /// to a full copy when the stored text is not the prefix the checkpoint expects.
    fn extend_streaming_assistant(
        &mut self,
        assistant_id: &str,
        response: &crate::llm::LlmResponse,
        persistence: &StreamPersistence,
    ) {
        if let Some(TranscriptMessage::Assistant {
            content,
            reasoning_content,
            tool_calls,
            ..
        }) = self.transcript.assistant_mut(assistant_id)
        {
            match response.text.get(persistence.text_bytes..) {
                Some(suffix) if content.len() == persistence.text_bytes => content.push_str(suffix),
                _ => content.clone_from(&response.text),
            }
            match (&response.reasoning, reasoning_content.as_mut()) {
                (Some(full), Some(stored)) if stored.len() == persistence.reasoning_bytes => {
                    match full.get(persistence.reasoning_bytes..) {
                        Some(suffix) => stored.push_str(suffix),
                        None => stored.clone_from(full),
                    }
                }
                _ => reasoning_content.clone_from(&response.reasoning),
            }
            if tool_calls.len() != response.tool_calls.len() {
                tool_calls.clone_from(&response.tool_calls);
            }
        }
    }

    fn finalize_streaming_assistant(
        &mut self,
        assistant_id: &str,
        response: &crate::llm::LlmResponse,
    ) {
        if let Some(TranscriptMessage::Assistant {
            content,
            reasoning_content,
            tool_calls,
            ..
        }) = self.transcript.assistant_mut(assistant_id)
        {
            content.clone_from(&response.text);
            reasoning_content.clone_from(&response.reasoning);
            tool_calls.clone_from(&response.tool_calls);
        }
    }
}

/// Bounded re-asks after a provider completion that carried no visible text and
/// no tool call.
const MAX_EMPTY_COMPLETION_RETRIES: u32 = 2;

/// Bounded turn-level re-asks after a stream that failed midway.
const MAX_STREAM_RETRIES: u32 = 3;

/// Nudge appended when a failed stream left partial output in the transcript.
const STREAM_INTERRUPTED_HINT: &str = "[Stream interrupted] Your previous reply was cut off by a connection error before it finished. Any tool calls in it were not run. Continue from where you stopped and re-issue only what is still needed.";

/// The provider layer's typed-by-message signal for a reasoning-only completion.
fn is_empty_completion_error(error: &str) -> bool {
    error.contains("no visible content or tool calls")
}

/// Bounded re-asks after a reply cut off at the provider's output limit.
const MAX_OUTPUT_LIMIT_RETRIES: u32 = 2;

/// Output cap of the first retry after a truncated reply: twice the 16,000
/// default the Anthropic adapter uses for adaptive thinking.
const OUTPUT_CAP_FIRST_RETRY: u32 = 32_000;

/// No output-limit retry asks for more than this.
const OUTPUT_CAP_CEILING: u32 = 64_000;

/// `runFailed.kind` for a run that ends on output-limit truncation.
const INCOMPLETE_OUTPUT_KIND: &str = "incomplete_output";

/// `runFailed.hint` for `INCOMPLETE_OUTPUT_KIND`.
const INCOMPLETE_OUTPUT_HINT: &str =
    "split large file writes into several smaller calls, or use a route with a larger output limit";

/// Re-entry message after a reply truncated at the output limit.
const OUTPUT_LIMIT_RETRY_HINT: &str = "[Output limit] Your previous reply was cut off at the provider's output limit; any tool calls in it were not run. Continue from where you stopped and re-issue only what is still needed, in fewer and smaller calls: split a large file write into several smaller writes or edits, each well under the limit.";

/// Re-entry message after an empty (reasoning-only) reply. The provider does
/// not report why the reply was empty, so the hint names the likely cause
/// (one oversized tool call) without asserting it.
const EMPTY_REPLY_HINT: &str = "[Empty reply] Your previous reply had no visible text and no tool call. Answer again with visible text or a tool call. If you were writing a very large file, split it into several smaller writes or edits.";

/// The prompt-cache lifetime for one model request: one hour only for a root session a person is
/// attached to, five minutes for everything else (see the call site).
pub(crate) fn cache_retention_for(
    is_subagent: bool,
    human_subscribers: usize,
) -> crate::llm::CacheRetention {
    if !is_subagent && human_subscribers > 0 {
        crate::llm::CacheRetention::Long
    } else {
        crate::llm::CacheRetention::Short
    }
}
