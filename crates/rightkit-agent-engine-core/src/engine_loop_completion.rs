use super::*;

pub(in crate::engine) enum AssistantCompletionOutcome {
    Continue,
    ContinueAfterGoal,
    Finished(String),
}

impl SessionEngine {
    /// Reasoning produced since the latest user message. Tool-call turns are
    /// not published as their own durable assistant message, so the turn's
    /// closing assistant message carries the reasoning summary for the turn.
    fn current_turn_reasoning(&self) -> Option<String> {
        let mut parts: Vec<&str> = Vec::new();
        for message in self.transcript.messages().iter().rev() {
            match message {
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
            },
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
        self.transcript.push_user(instruction.clone());
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
        let final_text = response.text.clone();
        // Verify edited bytes before any external signoff or close.
        if self.auto_post_edit_diagnostics {
            if let Some(outcome) = self
                .recheck_pending_verification(sid, pending_verify, session_span_id)
                .await
            {
                return outcome;
            }
        }
        if let Some(broker) = &self.assurance_broker {
            let gate = broker
                .lock()
                .map_err(|_| EngineError::Assurance("assurance broker lock poisoned".into()));
            let gate = match gate {
                Ok(broker) => broker.response_gate(),
                Err(error) => {
                    self.transcript
                        .push_user(format!("[Sentinel repair] {error}"));
                    return AssistantCompletionOutcome::Continue;
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
                self.transcript.push_user(text);
                return AssistantCompletionOutcome::Continue;
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
                    self.transcript
                        .push_user(format!("[Sentinel repair] {repair}"));
                    return AssistantCompletionOutcome::Continue;
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
                self.transcript.push_user(text);
                return AssistantCompletionOutcome::Continue;
            }
            let closed = client
                .beacon(serde_json::json!({
                    "operation": "close",
                    "run_id": run_id,
                    "summary": "Agent host completed after Sentinel signoff",
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
                    self.transcript
                        .push_user(format!("[Sentinel repair] {repair}"));
                    return AssistantCompletionOutcome::Continue;
                }
                Err(error) => {
                    self.transcript.push_user(format!(
                        "[Sentinel repair] Sentinel close transport failed: {error}"
                    ));
                    return AssistantCompletionOutcome::Continue;
                }
            }
        }
        self.emit(|seq| ProtocolEvent::AssistantMessageCompleted {
            session_id: sid.to_string(),
            seq,
            created_at: now_iso(),
            message: rightkit_agent_engine_protocol::TranscriptMessage::Assistant {
                id: "final".into(),
                created_at: now_iso(),
                content: final_text.clone(),
                reasoning_content: self.current_turn_reasoning(),
                tool_calls: vec![],
                pinned: false,
            },
        });
        // A steer pushed while this response streamed missed the drain at the
        // top of the loop, and the turn is about to end: carry on so the next
        // iteration injects it, instead of dropping the user's message.
        if self.steer_rx.as_ref().is_some_and(|rx| !rx.is_empty()) {
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
                .run_blocking_stop_event(
                    stop_event,
                    sid,
                    "assistant_message_completed",
                    post_turn_gate.consecutive_stop_blocks(),
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
                    self.transcript.push_user(instruction.clone());
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
                    return AssistantCompletionOutcome::Finished(format!(
                        "{final_text}\n\n{warning}"
                    ));
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
        if self.maybe_continue_toward_goal(made_tool_progress, sid) {
            return AssistantCompletionOutcome::ContinueAfterGoal;
        }
        self.record_session_end(session_span_id, "completed", None);
        AssistantCompletionOutcome::Finished(final_text)
    }
}
