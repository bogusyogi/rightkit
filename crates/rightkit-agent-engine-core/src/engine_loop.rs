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

impl SessionEngine {
    /// One user turn, from submission to the run's final outcome (including
    /// every early error exit). Wall-clock duration of the whole span is
    /// recorded per turn via `rightkit_logs::agent_telemetry::record_turn_latency`, so
    /// this wrapper owns the timer and the inner body keeps every return path.
    pub async fn submit_user_message(&mut self, text: &str) -> Result<String, EngineError> {
        let turn_started = std::time::Instant::now();
        let result = self.submit_user_message_turn(text).await;
        rightkit_logs::agent_telemetry::record_turn_latency(
            rightkit_logs::agent_telemetry::TurnLatencyEvent {
                latency_ms: u64::try_from(turn_started.elapsed().as_millis()).unwrap_or(u64::MAX),
            },
        );
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
                            crate::assurance_broker::HOST_ISSUER,
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
                    "summary": "Agent host task started",
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
                eprintln!("[hook] UserPromptSubmit error (non-blocking): {error}");
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
            self.transcript.push_user(format!(
                "{text}\n\n[Hook context: UserPromptSubmit]\n{}",
                prompt_context.join("\n")
            ));
        }

        let sid = self.session_id.clone();

        // Meltdown detection: if the agent loops (same tool action repeatedly),
        // reset its context rather than burn the whole iteration budget.
        // Thresholds are now lifted into the unified LoopDetectorConfig
        // (parity P-3) so all three loop-guard axes share one config
        // surface. Defaults reproduce the pre-P-3 hard-coded values.
        let cfg = &self.loop_detector_config;
        let mut mop = crate::mop::MopDetector::new(cfg.mop_window_size, cfg.mop_repeat_threshold);
        let mut loop_detector = crate::mop::ToolLoopDetector::default();
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
        // path fires.
        let mut iteration: usize = 0;
        while !self.iteration_budget.is_exhausted() {
            // Persistence failure is latched on the conduit: no further
            // semantic work. Stop through the interrupt path, but emit nothing
            // durable (it would be refused and would recurse into the failure).
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
                self.transcript.push_user(format!(
                    "[Loop warning] {warning}. {}",
                    crate::stall_strategy::STRATEGY_RESET_DIRECTIVE
                ));
                self.emit(|seq| ProtocolEvent::OrchestrationObservationRecorded {
                    session_id: sid.clone(),
                    seq,
                    created_at: now_iso(),
                    observation: rightkit_agent_engine_protocol::OrchestrationObservation {
                        id: format!("loop-warning-{iteration}"),
                        kind: "loop_warning".into(),
                        subject: sid.clone(),
                        summary: format!(
                            "loop warning at iteration {iteration}; injected strategy-reset directive"
                        ),
                        fields: std::collections::HashMap::new(),
                        created_at: now_iso(),
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
            // `rightkit_agent_tools::coding_tools::registry::lazy_loading` (byte-stable across
            // turns with an identical tool set) so a tool-set change is
            // folded into the same observation and reported as a cache
            // Break rather than a silent Hit. `CacheTracker` (rightkit-cache)
            // takes the digest as a caller-supplied `Option<&str>` instead of
            // depending on `host_tools` itself.
            // CR-33: use the SAME accumulated promotion state the provider
            // client puts on the wire (see `LlmClient::promoted_tool_state_snapshot`),
            // never a fresh/empty one — otherwise a promotion changes the
            // wire prefix with no corresponding CacheTracker digest
            // transition, defeating invariant F.
            let tool_promoted_state = self.llm.promoted_tool_state_snapshot();
            let tool_defs_for_digest =
                rightkit_agent_tools::coding_tools::registry::tool_defs_for_messages(
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
            let request_metadata = LlmRequestMetadata {
                request_identity: Some(crate::llm::LlmRequestIdentity::new(
                    self.session_id.clone(),
                    self.active_run_id().to_string(),
                    llm_turn_id.clone(),
                    u64::try_from(iteration).expect("request iteration fits u64"),
                )),
                stable_prefix_key,
                max_output_tokens: None,
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
            let mut stream_messages = self.wire_projection.project(messages_slice);
            stream_messages.retain(|message| message.is_provider_visible());
            let injection_budget = self.compaction_policy.context_window_tokens.map(|window| {
                let used = serde_json::to_string(&stream_messages)
                    .map(|body| body.len() as u64 / 4)
                    .unwrap_or(0);
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
                    content: format!("Persistent goal (track until complete): {goal}"),
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
            let model_request = self
                .collect_streaming_response(&stream_messages, &request_metadata, &sid)
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
                        rightkit_logs::agent_telemetry::record_round_trip(
                            rightkit_logs::agent_telemetry::RoundTripEvent { task: sid.clone() },
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
                        if crate::overflow_recovery::is_reactive_context_overflow(&e) {
                            match reactive_overflow_recovery.overflow_detected() {
                                Ok(_) => {}
                                Err(_) => {
                                    self.record_session_end(
                                        session_span_id.as_deref(),
                                        "failed",
                                        Some(&e),
                                    );
                                    return Err(EngineError::Llm(e));
                                }
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

                            let _ = reactive_overflow_recovery.begin_compaction();
                            match self
                                .execute_compaction_command(crate::compactor::CompactionCommandV1 {
                                    id: crate::compactor::next_compaction_command_id(),
                                    // Reactive provider-overflow recovery may
                                    // have no pending retry decision; it is a
                                    // fresh automatic compaction in that case.
                                    kind: crate::compactor::CompactionCommandKindV1::Overflow,
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
                        self.record_runtime_span(
                            MiddlewarePhase::PostLlmCall,
                            "error",
                            Some(&llm_turn_id),
                            llm_span_id.as_deref(),
                            serde_json::json!({
                                "iteration": iteration,
                                "error": e,
                                "kind": kind.as_str(),
                            }),
                        );
                        self.emit(|seq| ProtocolEvent::RunFailed {
                            session_id: sid.clone(),
                            seq,
                            created_at: now_iso(),
                            error: e.clone(),
                            kind: kind.as_str().into(),
                            hint: kind.hint().into(),
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
                let prior_ids =
                    crate::transcript_plumbing::prior_tool_call_ids(&self.transcript, None);
                for dropped in crate::transcript_plumbing::drop_cross_turn_duplicate_tool_calls(
                    &mut response.tool_calls,
                    &prior_ids,
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
                match loop_detector.record(&response.tool_calls) {
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

            self.usage.record(
                response.prompt_tokens,
                response.completion_tokens,
                response.cache_read_tokens,
                response.cache_creation_tokens,
            );
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
            let cost_known = self.usage.is_priced();
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
            });
            self.emit_turn_usage(
                &sid,
                response.prompt_tokens,
                response.cache_read_tokens,
                response.cache_creation_tokens,
                response.completion_tokens,
                stream_persistence.reasoning_tokens,
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
            }

            self.observe_response_tool_activity(
                &response,
                &sid,
                iteration,
                &mut mop,
                &mut tool_calls_since_continuation,
            )
            .await;

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
                self.iteration_budget
                    .refund(cost, "no-op turn: no tool calls, no assistant text");
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
                super::engine_loop_dispatch::ToolDispatchOutcome::Proceed => {}
                super::engine_loop_dispatch::ToolDispatchOutcome::Continue => {
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
    ) -> Result<
        (
            crate::llm::LlmResponse,
            bool,
            std::collections::HashMap<String, rightkit_agent_tools::coding_tools::ToolResult>,
            StreamPersistence,
        ),
        EngineError,
    > {
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
            rightkit_agent_tools::coding_tools::ToolResult,
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
                            let _ = pump.await;
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
                        let _ = pump.await;
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
                    let _ = pump.await;
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
                crate::llm::LlmStreamEvent::ToolCall(call) => {
                    streamed_tool_calls = true;
                    if crate::transcript_plumbing::prior_tool_call_ids(
                        &self.transcript,
                        assistant_id.as_deref(),
                    )
                    .contains(&call.id)
                    {
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
                        let message = self.transcript.push_assistant("", None, Vec::new());
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
                crate::llm::LlmStreamEvent::Finished(reason) => {
                    response.finish_reason = reason;
                    stream_finished = true;
                }
            }
        }
        if !stream_finished {
            pump.abort();
            let _ = pump.await;
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
        let _ = pump.await;
        // Apply the completeness gate before finalizing the assistant
        // envelope or dispatching any deferred call. Clearing the queue
        // makes an incomplete streamed generation tool-free downstream.
        let generation_gate = enforce_generation_completeness_gate(&mut response);
        if generation_gate.is_some() {
            deferred_calls.clear();
            streamed_tool_calls = false;
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
        // Execute strictly in issue order, one at a time; a user interrupt
        // here still aborts before any not-yet-started deferred call runs.
        for (index, call) in deferred_calls.iter().enumerate() {
            if self.conduit.persistence_failure().is_some() {
                // Results could not be stored; do not start more owned work.
                return Err(EngineError::Interrupted);
            }
            let interrupt_rx = self.interrupt_rx.clone();
            let serial = self.dispatch_streaming_tool_call(call, &mut turn_results);
            if let Some(interrupt_rx) = interrupt_rx {
                tokio::select! {
                    _ = serial => {}
                    () = wait_for_interrupt_signal(interrupt_rx) => {
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
        }

        stream_persistence.assistant_precommitted = assistant_id.is_some();
        Ok((
            response,
            streamed_tool_calls,
            turn_results,
            stream_persistence,
        ))
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
            let message = self.transcript.push_assistant("", None, Vec::new());
            if let TranscriptMessage::Assistant { id, .. } = message {
                *assistant_id = Some(id.clone());
            }
        }
        if let Some(id) = assistant_id.as_deref() {
            self.finalize_streaming_assistant(id, response);
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
    fn record_undispatched_streaming_calls(
        &mut self,
        sid: &str,
        calls: impl IntoIterator<Item = ToolCall>,
        reason: &str,
        turn_results: &mut std::collections::HashMap<
            String,
            rightkit_agent_tools::coding_tools::ToolResult,
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

/// The provider layer's typed-by-message signal for a reasoning-only completion.
fn is_empty_completion_error(error: &str) -> bool {
    error.contains("no visible content or tool calls")
}
