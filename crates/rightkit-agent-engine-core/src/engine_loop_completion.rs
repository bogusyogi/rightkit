use super::*;
use rightkit_agent_engine_protocol::AssistantMessagePhase as Phase;

pub(in crate::engine) enum AssistantCompletionOutcome {
    Continue,
    ContinueAfterGoal,
    Finished(String),
}

impl SessionEngine {
    /// Reasoning produced since the latest user message. Tool-call turns are
    /// not published as their own durable assistant message, so the turn's
    /// closing assistant message carries the reasoning summary for the turn.
    pub(in crate::engine) fn current_turn_reasoning(&self) -> Option<String> {
        let mut parts: Vec<&str> = Vec::new();
        for message in self.transcript.messages().iter().rev() {
            match message {
                // A harness nudge inside the turn is not where the turn began.
                message if crate::transcript::is_nudge(message) => {}
                TranscriptMessage::User { .. } => break,
                TranscriptMessage::Assistant {
                    reasoning_content: Some(reasoning),
                    ..
                } if !reasoning.is_empty() => parts.push(reasoning.as_str()),
                _ => {}
            }
        }
        if parts.is_empty() {
            return None;
        }
        parts.reverse();
        Some(parts.join(
            "

",
        ))
    }

    pub(in crate::engine) fn complete_forced_stop(
        &mut self,
        sid: &str,
        text: &str,
        session_span_id: Option<&str>,
    ) {
        self.emit(|seq| ProtocolEvent::AssistantDelta {
            session_id: sid.to_string(),
            seq,
            created_at: now_iso(),
            text: text.to_string(),
            is_reasoning: false,
        });
        self.transcript
            .push_assistant(text.to_string(), None, Vec::new());
        self.emit(|seq| ProtocolEvent::AssistantMessageCompleted {
            session_id: sid.to_string(),
            seq,
            created_at: now_iso(),
            message: rightkit_agent_engine_protocol::TranscriptMessage::Assistant {
                id: "forced-stop".into(),
                created_at: now_iso(),
                content: text.to_string(),
                reasoning_content: None,
                tool_calls: vec![],
                pinned: false,
                origin: self.llm.message_origin(),
            },
            phase: rightkit_agent_engine_protocol::AssistantMessagePhase::Final,
        });
        self.publish_hook(rightkit_hooks::agent_hooks::HookEvent::Notification {
            session_id: sid.to_string(),
            message: text.chars().take(120).collect(),
        });
        self.publish_hook(rightkit_hooks::agent_hooks::HookEvent::Stop {
            session_id: sid.to_string(),
        });
        self.record_session_end(session_span_id, "completed", Some("forced stop"));
    }

    /// Re-verify pending required verification once at attempted completion.
    /// The same fan-out (receipt binding, budgets, staleness rules) re-runs
    /// over the latest on-disk bytes of the mutated results recorded at
    /// dispatch time. A clean recheck clears the debt; actionable findings
    /// or a still-open required gap re-enter with the rendered instruction;
    /// once the recheck bound is spent the turn ends with an explicit
    /// unverified warning — never a verified-clean claim.
    async fn recheck_pending_verification(
        &mut self,
        sid: &str,
        pending_verify: &mut crate::verify_fanout::PendingVerification,
        session_span_id: Option<&str>,
    ) -> Option<AssistantCompletionOutcome> {
        if !pending_verify.is_pending() {
            return None;
        }
        if !pending_verify.try_recheck() {
            let warning = format!(
                "[Loop-safety gate: required verification still could not confirm the edited \
                 files after {} completion recheck(s); stopping without a verified-clean claim. \
                 Outstanding gaps: {}]",
                crate::verify_fanout::PendingVerification::MAX_RECHECKS,
                pending_verify.gaps().join("; ")
            );
            self.emit(|seq| ProtocolEvent::AssistantDelta {
                session_id: sid.to_string(),
                seq,
                created_at: now_iso(),
                // A separate paragraph, never glued to the model's last word.
                text: format!("\n\n{warning}"),
                is_reasoning: false,
            });
            self.record_session_end(
                session_span_id,
                "completed",
                Some("required verification unconfirmed"),
            );
            return Some(AssistantCompletionOutcome::Finished(warning));
        }
        let receipts = self.assurance_mutation_receipts();
        let report = crate::verify_fanout::run_verify_fanout(crate::verify_fanout::FanoutRequest {
            session_id: &self.session_id,
            workspace_root: &self.workspace_root,
            permission_mode: self.permission_mode,
            workspace_trust: self.workspace_trust,
            turn_results: pending_verify.results(),
            receipt_binding: crate::verify_fanout::ReceiptBinding::Enforced {
                receipts: &receipts,
            },
            command_verifiers: crate::verify_fanout::default_command_verifiers(
                &self.workspace_root,
            ),
            budgets: crate::verify_fanout::VerifyBudgets::default(),
        })
        .await;
        let required_gap = crate::verify_fanout::has_required_gap(&report);
        let mut fields = crate::verify_fanout::observation_fields(&report);
        fields.insert("recheck".into(), pending_verify.rechecks().to_string());
        self.emit(|seq| ProtocolEvent::OrchestrationObservationRecorded {
            session_id: sid.to_string(),
            seq,
            created_at: now_iso(),
            observation: rightkit_agent_engine_protocol::OrchestrationObservation {
                id: format!("pvf-recheck-{}", pending_verify.rechecks()),
                kind: "post_edit_verification_recheck".into(),
                subject: sid.to_string(),
                summary: format!(
                    "completion recheck over {} pending mutation(s); required gap: {required_gap}",
                    pending_verify.results().len()
                ),
                fields,
                created_at: now_iso(),
            },
        });
        let blocked = report.is_fenced()
            || required_gap
            || !crate::verify_fanout::actionable_findings(&report).is_empty();
        if !blocked {
            pending_verify.clear();
            return None;
        }
        pending_verify.replace_gaps(crate::verify_fanout::required_gap_lines(&report));
        let instruction =
            crate::verify_fanout::render_followup_injection(&report).unwrap_or_else(|| {
                "[post-edit verification] Required verification still could not confirm the \
                 edits; resolve the gaps above before finishing."
                    .to_string()
            });
        let instruction =
            crate::tool_result_store::bound_gate_text(&mut self.tool_results, &instruction);
        self.transcript.push_nudge(instruction.clone());
        self.emit(|seq| ProtocolEvent::HumanSteeringInjected {
            session_id: sid.to_string(),
            seq,
            created_at: now_iso(),
            injected_by: crate::verify_fanout::injection::FOLLOWUP_SOURCE.into(),
            content_preview: instruction.chars().take(80).collect(),
            text: Some(crate::engine::engine_loop::bound_interjection_text(
                &instruction,
            )),
        });
        Some(AssistantCompletionOutcome::Continue)
    }

    pub(in crate::engine) async fn handle_assistant_completion(
        &mut self,
        response: &crate::llm::LlmResponse,
        sid: &str,
        session_span_id: Option<&str>,
        post_turn_gate: &mut crate::post_turn_gate::PostTurnGateState,
        made_tool_progress: bool,
        pending_verify: &mut crate::verify_fanout::PendingVerification,
    ) -> AssistantCompletionOutcome {
        let mut final_text = response.text.clone();
        // Verify edited bytes before any external signoff or close.
        if self.auto_post_edit_diagnostics {
            if let Some(outcome) = self
                .recheck_pending_verification(sid, pending_verify, session_span_id)
                .await
            {
                if let AssistantCompletionOutcome::Finished(text) = outcome {
                    let combined = format!("{final_text}\n\n{text}");
                    let reasoning = self.current_turn_reasoning();
                    self.emit_assistant_completed(sid, Phase::Final, combined.clone(), reasoning);
                    return AssistantCompletionOutcome::Finished(combined);
                }
                return outcome;
            }
        }
        // Assurance/Sentinel refusals re-enter. A host that sets `completion_repair_cap` bounds
        // them per turn and lets the turn finish with an explicit warning after the cap (audit
        // MEDIUM-1); otherwise every refusal re-enters until the iteration budget ends the turn.
        if let Some(repair) = self.completion_gate_refusal().await {
            if !crate::host_options::completion_repair_cap() {
                self.transcript.push_nudge(repair);
                return AssistantCompletionOutcome::Continue;
            }
            match post_turn_gate.after_completion_repair(repair) {
                crate::post_turn_gate::PostTurnGateDecision::Reenter { instruction, .. } => {
                    self.transcript.push_nudge(instruction);
                    return AssistantCompletionOutcome::Continue;
                }
                crate::post_turn_gate::PostTurnGateDecision::ForceFinish { warning } => {
                    self.emit(|seq| ProtocolEvent::AssistantDelta {
                        session_id: sid.to_string(),
                        seq,
                        created_at: now_iso(),
                        text: format!("\n\n{warning}"),
                        is_reasoning: false,
                    });
                    final_text = format!("{final_text}\n\n{warning}");
                }
                crate::post_turn_gate::PostTurnGateDecision::Proceed => {}
            }
        }
        self.finish_assistant_completion(
            response,
            sid,
            session_span_id,
            post_turn_gate,
            made_tool_progress,
            final_text,
        )
        .await
    }

    /// The assurance-broker and Sentinel completion gates. `Some(text)` is the
    /// refusal to hand back to the model; `None` means signoff and close passed.
    async fn completion_gate_refusal(&mut self) -> Option<String> {
        if let Some(broker) = &self.assurance_broker {
            let gate = broker
                .lock()
                .map_err(|_| EngineError::Assurance("assurance broker lock poisoned".into()));
            let gate = match gate {
                Ok(broker) => broker.response_gate(),
                Err(error) => {
                    return Some(format!("[Sentinel repair] {error}"));
                }
            };
            if !gate.allowed {
                let text = crate::tool_result_store::bound_gate_text(
                    &mut self.tool_results,
                    &format!(
                        "[Sentinel repair] {}: {}",
                        gate.reason,
                        gate.repair_packet.join("; ")
                    ),
                );
                return Some(text);
            }
        }
        if let (Some(client), Some(run_id)) = (&self.sentinel_client, &self.sentinel_run_id) {
            let mut client = client.lock().await;
            let verified = client
                .beacon(serde_json::json!({
                    "operation": "verify",
                    "run_id": run_id,
                    "gate": "signoff",
                    "session_id": self.session_id,
                }))
                .await;
            let verification = match verified {
                Ok(value) => value,
                Err(error) => {
                    let repair = format!("Sentinel verify transport failed: {error}");
                    return Some(format!("[Sentinel repair] {repair}"));
                }
            };
            let decision = verification
                .get("decision")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("blocked");
            if matches!(decision, "blocked" | "escalate") {
                let repair = verification
                    .get("next_actions")
                    .and_then(serde_json::Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .collect::<Vec<_>>()
                            .join("; ")
                    })
                    .filter(|text| !text.is_empty())
                    .unwrap_or_else(|| {
                        "Sentinel signoff is blocked; obtain valid evidence before final response."
                            .into()
                    });
                let text = crate::tool_result_store::bound_gate_text(
                    &mut self.tool_results,
                    &format!("[Sentinel repair] {repair}"),
                );
                return Some(text);
            }
            let closed = client
                .beacon(serde_json::json!({
                    "operation": "close",
                    "run_id": run_id,
                    "summary": format!("{} host completed after Sentinel signoff", rightkit_agent_config::app_name()),
                    "session_id": self.session_id,
                }))
                .await;
            match closed {
                Ok(value)
                    if value.get("decision").and_then(serde_json::Value::as_str)
                        == Some("closed") => {}
                Ok(value) => {
                    let repair = value
                        .get("next_actions")
                        .and_then(serde_json::Value::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(serde_json::Value::as_str)
                                .collect::<Vec<_>>()
                                .join("; ")
                        })
                        .filter(|text| !text.is_empty())
                        .unwrap_or_else(|| "Sentinel close did not return closed.".into());
                    return Some(format!("[Sentinel repair] {repair}"));
                }
                Err(error) => {
                    return Some(format!(
                        "[Sentinel repair] Sentinel close transport failed: {error}"
                    ));
                }
            }
        }
        None
    }

    /// Everything after the completion gates passed or were capped: the
    /// pending-steer check, Notification and Stop hooks, goal continuation.
    /// The answer's `AssistantMessageCompleted` is emitted only once its fate is
    /// known: "final" where the turn ends, "interim" where it continues.
    async fn finish_assistant_completion(
        &mut self,
        response: &crate::llm::LlmResponse,
        sid: &str,
        session_span_id: Option<&str>,
        post_turn_gate: &mut crate::post_turn_gate::PostTurnGateState,
        made_tool_progress: bool,
        final_text: String,
    ) -> AssistantCompletionOutcome {
        // A steer pushed while this response streamed missed the drain at the
        // top of the loop, and the turn is about to end: carry on so the next
        // iteration injects it, instead of dropping the user's message.
        if self.steer_rx.as_ref().is_some_and(|rx| !rx.is_empty()) {
            let reasoning = self.current_turn_reasoning();
            self.emit_assistant_completed(sid, Phase::Interim, final_text, reasoning);
            return AssistantCompletionOutcome::Continue;
        }
        self.publish_hook(rightkit_hooks::agent_hooks::HookEvent::Notification {
            session_id: sid.to_string(),
            message: response.text.chars().take(120).collect(),
        });
        // Notification hooks (non-blocking; additionalContext is forwarded).
        self.run_lifecycle_hooks(
            "Notification",
            &[(
                "RIGHTKIT_HOOK_MESSAGE",
                response.text.chars().take(120).collect::<String>(),
            )],
        )
        .await;
        if let Some(bus) = self.hook_bus.clone() {
            // Sub-agents gate on SubagentStop; the main agent on Stop. A deny
            // (exit 2, decision block, continue:false) re-enters with the
            // reason through the post-turn gate below.
            let stop_event = if self.subagent_role.is_some() {
                "SubagentStop"
            } else {
                "Stop"
            };
            let outcome = bus
                .run_blocking_stop_event_for_role(
                    stop_event,
                    sid,
                    "assistant_message_completed",
                    post_turn_gate.consecutive_stop_blocks(),
                    self.subagent_role.as_deref(),
                )
                .await;
            match post_turn_gate.after_blocking_stop(outcome) {
                crate::post_turn_gate::PostTurnGateDecision::Reenter {
                    instruction,
                    reason,
                } => {
                    let instruction = crate::tool_result_store::bound_gate_text(
                        &mut self.tool_results,
                        &instruction,
                    );
                    // The blocked answer was shown: complete it as interim
                    // before the re-entry instruction joins the transcript.
                    let reasoning = self.current_turn_reasoning();
                    self.emit_assistant_completed(sid, Phase::Interim, final_text, reasoning);
                    self.transcript.push_nudge(instruction.clone());
                    self.emit(|seq| ProtocolEvent::HumanSteeringInjected {
                        session_id: sid.to_string(),
                        seq,
                        created_at: now_iso(),
                        injected_by: reason,
                        content_preview: instruction.chars().take(80).collect(),
                        text: Some(crate::engine::engine_loop::bound_interjection_text(
                            &instruction,
                        )),
                    });
                    return AssistantCompletionOutcome::Continue;
                }
                crate::post_turn_gate::PostTurnGateDecision::ForceFinish { warning } => {
                    self.emit(|seq| ProtocolEvent::AssistantDelta {
                        session_id: sid.to_string(),
                        seq,
                        created_at: now_iso(),
                        text: format!("\n\n{warning}"),
                        is_reasoning: false,
                    });
                    self.record_session_end(
                        session_span_id,
                        "completed",
                        Some("loop-safety gate cap"),
                    );
                    let combined = format!("{final_text}\n\n{warning}");
                    let reasoning = self.current_turn_reasoning();
                    self.emit_assistant_completed(sid, Phase::Final, combined.clone(), reasoning);
                    return AssistantCompletionOutcome::Finished(combined);
                }
                crate::post_turn_gate::PostTurnGateDecision::Proceed => {}
            }
        }
        self.publish_hook(rightkit_hooks::agent_hooks::HookEvent::Stop {
            session_id: sid.to_string(),
        });
        // [F2] Continue toward an active goal instead of stopping, when the
        // shared GoalState permits (budget + no-progress + continuation caps).
        // Goal-less / disabled sessions fall straight through unchanged.
        let reasoning = self.current_turn_reasoning();
        if self.maybe_continue_toward_goal(made_tool_progress, sid) {
            self.emit_assistant_completed(sid, Phase::Interim, final_text, reasoning);
            return AssistantCompletionOutcome::ContinueAfterGoal;
        }
        self.record_session_end(session_span_id, "completed", None);
        self.emit_assistant_completed(sid, Phase::Final, final_text.clone(), reasoning);
        AssistantCompletionOutcome::Finished(final_text)
    }

    /// One `AssistantMessageCompleted` per answer the user saw, emitted only
    /// after the turn's fate is known: `phase` is `Final` where the turn ends,
    /// `Interim` where a steer, a blocking Stop hook or goal continuation
    /// carries it on. Clients end a turn on `sessionTurnFinished`, never on a
    /// completion (issue 450). The message id mirrors the phase but is rewritten
    /// by canonicalization; `phase` is the wire truth. `reasoning` is captured
    /// before any re-entry message is pushed, so it stays the answer's own.
    pub(in crate::engine) fn emit_assistant_completed(
        &self,
        sid: &str,
        phase: Phase,
        content: String,
        reasoning: Option<String>,
    ) {
        let id = match phase {
            Phase::Interim => "interim",
            Phase::Final => "final",
        };
        self.emit(|seq| ProtocolEvent::AssistantMessageCompleted {
            session_id: sid.to_string(),
            seq,
            created_at: now_iso(),
            message: rightkit_agent_engine_protocol::TranscriptMessage::Assistant {
                id: id.to_string(),
                created_at: now_iso(),
                content,
                reasoning_content: reasoning,
                tool_calls: vec![],
                pinned: false,
                origin: self.llm.message_origin(),
            },
            phase,
        });
    }
}
