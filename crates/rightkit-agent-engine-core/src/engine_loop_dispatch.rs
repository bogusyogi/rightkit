use super::*;

pub(in crate::engine) enum ToolDispatchOutcome {
    Proceed,
    Continue,
    Finished(String),
}

impl SessionEngine {
    /// Host mutation receipts for the fan-out's FM-179 binding leg.
    pub(in crate::engine) fn assurance_mutation_receipts(
        &self,
    ) -> std::collections::HashMap<String, crate::assurance_broker::ToolReceipt> {
        self.assurance_broker
            .as_ref()
            .and_then(|broker| {
                broker.lock().ok().map(|broker| {
                    broker
                        .receipts()
                        .map(|receipt| (receipt.tool_call_id.clone(), receipt.clone()))
                        .collect()
                })
            })
            .unwrap_or_default()
    }

    /// The shared post-edit verification fan-out gate (Block 2). Runs the
    /// bounded fan-out, emits the typed orchestration observation (verifier
    /// status, requirement, covered paths, reasons — zero findings never
    /// reads as fully verified), then applies the requirement-aware
    /// decision: actionable findings and required gaps reenter; a required
    /// gap also arms `pending_verify` so attempted completion re-verifies
    /// the latest bytes; an optional-only gap is reported to the transcript
    /// and observation but neither finishes nor reenters the turn.
    async fn post_edit_verify_gate(
        &mut self,
        turn_results: &std::collections::HashMap<
            String,
            rightkit_agent_tools::coding_tools::ToolResult,
        >,
        iteration: usize,
        sid: &str,
        pending_verify: &mut crate::verify_fanout::PendingVerification,
    ) -> Option<ToolDispatchOutcome> {
        if !self.auto_post_edit_diagnostics {
            return None;
        }
        let receipts = self.assurance_mutation_receipts();
        let report = crate::verify_fanout::run_verify_fanout(crate::verify_fanout::FanoutRequest {
            session_id: &self.session_id,
            workspace_root: &self.workspace_root,
            permission_mode: self.permission_mode,
            workspace_trust: self.workspace_trust,
            turn_results,
            receipt_binding: crate::verify_fanout::ReceiptBinding::Enforced {
                receipts: &receipts,
            },
            command_verifiers: crate::verify_fanout::default_command_verifiers(
                &self.workspace_root,
            ),
            budgets: crate::verify_fanout::VerifyBudgets::default(),
        })
        .await;
        if report.mutated_paths.is_empty() && !report.is_fenced() {
            return None;
        }
        let file_count = report.mutated_paths.len();
        let finding_count = report.findings.len();
        let required_gap = crate::verify_fanout::has_required_gap(&report);
        let mut fields = crate::verify_fanout::observation_fields(&report);
        fields.insert("iteration".into(), iteration.to_string());
        self.emit(|seq| ProtocolEvent::OrchestrationObservationRecorded {
            session_id: sid.to_string(),
            seq,
            created_at: now_iso(),
            observation: rightkit_agent_engine_protocol::OrchestrationObservation {
                id: format!("pvf-{iteration}"),
                kind: "post_edit_verification".into(),
                subject: sid.to_string(),
                summary: format!(
                    "verification fan-out reported {finding_count} finding(s) for {file_count} edited file(s); required gap: {required_gap}"
                ),
                fields,
                created_at: now_iso(),
            },
        });
        match crate::verify_fanout::post_dispatch_action(&report) {
            crate::verify_fanout::PostDispatchAction::Proceed => None,
            crate::verify_fanout::PostDispatchAction::ReportOnly(instruction) => {
                // Optional verifiers produced no verdict (and/or warnings
                // only). The note lands in the transcript so the model reads
                // it on the next request, but the turn proceeds — a missing
                // optional language server must neither force-finish nor
                // endlessly reenter.
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
                None
            }
            crate::verify_fanout::PostDispatchAction::Reenter {
                instruction,
                required_gap,
            } => {
                if required_gap || !crate::verify_fanout::actionable_findings(&report).is_empty() {
                    pending_verify.arm(
                        turn_results,
                        crate::verify_fanout::required_gap_lines(&report),
                    );
                }
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
                Some(ToolDispatchOutcome::Continue)
            }
        }
    }

    /// Dispatch one call emitted by a live response after the stream reaches
    /// its terminal event and the EFF-009 completeness gate admits it. All
    /// calls, including read-only calls, use this governed serial path so no
    /// approval, ToolCallStarted, or effect can precede complete generation.
    pub(in crate::engine) async fn dispatch_streaming_tool_call(
        &mut self,
        call: &ToolCall,
        turn_results: &mut std::collections::HashMap<
            String,
            rightkit_agent_tools::coding_tools::ToolResult,
        >,
    ) {
        self.dispatch_tool_call(call, turn_results).await;
    }

    pub(in crate::engine) fn streaming_tool_is_concurrent_safe(&self, call: &ToolCall) -> bool {
        let schema = self.tools.get(&call.name).map(|tool| tool.input_schema());
        let (args, parse_error) = crate::tool_dispatch_plumbing::parse_tool_arguments_attributed(
            &call.arguments_json,
            schema.as_ref(),
            Some(&call.name),
            self.repair_attribution.as_ref(),
        );
        if parse_error.is_some() {
            return false;
        }
        let mut canonical_call = call.clone();
        canonical_call.arguments_json = args.to_string();
        self.can_dispatch_readonly_fast(&&canonical_call)
    }

    pub(in crate::engine) fn announce_streaming_tool_call(&mut self, call: &ToolCall) {
        let sid = self.session_id.clone();
        self.emit(|seq| ProtocolEvent::ToolCallStarted {
            session_id: sid,
            seq,
            created_at: now_iso(),
            call: call.clone(),
        });
        self.record_tool_runtime_span(
            MiddlewarePhase::PreToolCall,
            "completed",
            call,
            None,
            serde_json::json!({
                "path": extract_path_arg(&call.arguments_json),
                "hasCommand": extract_command_arg(&call.arguments_json).is_some(),
                "commandBytes": extract_command_arg(&call.arguments_json).map(|value| value.len()),
                "dispatchMode": "streaming",
            }),
        );
        let path_arg = extract_path_arg(&call.arguments_json);
        let command_arg = extract_command_arg(&call.arguments_json);
        self.add_triggered_skill_hint("PreToolUse", |ctx| {
            ctx.set("tool", &call.name);
            if let Some(path) = &path_arg {
                ctx.set("file_path", path);
            }
            if let Some(command) = &command_arg {
                ctx.set("command", command);
            }
            ctx.trigger_type = Some("tool_call".into());
            ctx.trigger_payload = Some(call.name.clone());
        });
    }

    pub(in crate::engine) async fn dispatch_response_tools_and_post_checks(
        &mut self,
        response: &mut crate::llm::LlmResponse,
        iteration: usize,
        sid: &str,
        post_turn_gate: &mut crate::post_turn_gate::PostTurnGateState,
        pending_verify: &mut crate::verify_fanout::PendingVerification,
    ) -> ToolDispatchOutcome {
        use crate::engine::stall_guardrail::{
            block_message, skipped_message, warn_message, StallSeverity,
        };

        // Plan approval can change mode while an earlier tool is awaiting
        // operator input. Apply that live authority before the next batch.
        self.refresh_permission_mode();

        // Accumulator for every tool result produced by this turn. The post-
        // edit guard, the typed unbacked-mutation detector, and the renderer
        // all read from this single map instead of re-parsing call arguments.
        // Read-only turns may build this map cheaply; they never trigger the
        // guard, but they still flow through the same type so the dispatcher
        // has one code path.
        let mut turn_results: std::collections::HashMap<
            String,
            rightkit_agent_tools::coding_tools::ToolResult,
        > = std::collections::HashMap::new();

        // [FEATURE-MATRIX par §#120] Graduate per-tool-signature stall
        // guardrail before partition. Hermes's `ToolCallGuardrailController`
        // shape: `Allow < Warn < Skip < Block`, thresholds 2 / 4 / 6. The
        // existing flat `MAX_NO_PROGRESS=3` cap is preserved as a SEPARATE
        // safety net (spec: ADD, not REPLACE).
        let mut blocked_info: Option<(u32, String)> = None;
        let mut kept_calls: Vec<ToolCall> = Vec::with_capacity(response.tool_calls.len());
        let mut skipped_calls: Vec<(u32, ToolCall)> = Vec::new();
        for call in response.tool_calls.iter() {
            let sev = self.stall_guardrail.record_call(call);
            match sev {
                StallSeverity::Allow => kept_calls.push(call.clone()),
                StallSeverity::Warn { count } => {
                    // Warn runs but the nudge is appended to the assistant
                    // turn text so the model reads it in the NEXT turn's
                    // context (response.text persists to the transcript).
                    let w = warn_message(&call.name, count);
                    if response.text.is_empty() {
                        response.text = w;
                    } else {
                        response.text.push_str("\n\n");
                        response.text.push_str(&w);
                    }
                    kept_calls.push(call.clone());
                }
                StallSeverity::Skip { count } => {
                    // Drop from dispatch set; synthetic result injected below.
                    skipped_calls.push((count, call.clone()));
                }
                StallSeverity::Block { count } => {
                    blocked_info = Some((count, call.name.clone()));
                    break;
                }
            }
        }

        if let Some((count, name)) = blocked_info {
            let block_msg = block_message(&name, count);
            // Drain any Skip-grade calls that accumulated BEFORE the Block:
            // their synthetic tool results must reach the model even on the
            // Block path so it sees what happened to every call it issued, not
            // just a vanishing subset.
            for (skip_count, skip_call) in &skipped_calls {
                self.record_tool_error(
                    sid,
                    skip_call,
                    skipped_message(&skip_call.name, *skip_count),
                    &mut turn_results,
                );
            }
            response.text = block_msg.clone();
            response.tool_calls.clear();
            // Block closes the goal as blocked through the SAME
            // MAX_NO_PROGRESS path the flat counter uses (and bumps it high
            // enough so the existing `update_goal_state` Close("blocked")
            // terminal fires). We also emit the terminal `GoalProgressUpdated`
            // event + a `stalled`-shaped OrchestrationObservation so observers
            // see one consistent "blocked" shape, not two.
            let snapshot = {
                let mut guard = self.goal_state.lock().unwrap();
                if let Some(gs) = guard.as_mut() {
                    gs.no_progress_count = std::cmp::max(
                        gs.no_progress_count,
                        crate::goal_state::GoalState::MAX_NO_PROGRESS,
                    );
                    if gs.status == "active" || gs.status == "paused" {
                        gs.status = "blocked".into();
                    }
                    Some(gs.snapshot())
                } else {
                    None
                }
            };
            if let Some(snapshot) = snapshot {
                self.emit(|seq| ProtocolEvent::GoalProgressUpdated {
                    session_id: sid.to_string(),
                    seq,
                    created_at: now_iso(),
                    snapshot,
                });
            }
            self.emit(|seq| ProtocolEvent::OrchestrationObservationRecorded {
                session_id: sid.to_string(),
                seq,
                created_at: now_iso(),
                observation: rightkit_agent_engine_protocol::OrchestrationObservation {
                    id: format!("stall-guardrail-{iteration}"),
                    kind: "stalled".into(),
                    subject: sid.to_string(),
                    summary: crate::stall_strategy::stalled_reason(&block_msg),
                    fields: std::collections::HashMap::new(),
                    created_at: now_iso(),
                },
            });
            return ToolDispatchOutcome::Finished(block_msg);
        }

        // Skip'd calls: inject synthetic tool-result events so the model
        // sees the explanation AS the tool outcome (the call itself does
        // NOT execute). The result is also added to `turn_results` so the
        // post-edit guard sees the failure as `is_error == true` and skips
        // it (a skipped call never edits the workspace).
        for (count, call) in &skipped_calls {
            self.record_tool_error(
                sid,
                call,
                skipped_message(&call.name, *count),
                &mut turn_results,
            );
        }
        response.tool_calls = kept_calls;

        // Fold the ORIGINAL call sequence into consecutive runs (Book A §5a:
        // "a batch of safe reads together, an unsafe call alone" — consecutive
        // runs of the model's own order, never a global re-sort by category).
        // A prior version partitioned the whole turn into read_calls/mut_calls
        // globally and ran all reads before all writes, which silently
        // reordered a turn like [write A, read B, write C] into
        // [read B], [write A, write C] — a verify-after-edit pattern (write,
        // then read back in the same turn) would read pre-write state with no
        // error. Runs now execute strictly in the order they occur in
        // `response.tool_calls`; only calls *within* one run may overlap.
        enum ToolRun<'a> {
            /// A maximal consecutive run of concurrency-safe (read-only)
            /// calls — dispatched as one bounded concurrent batch.
            Read(Vec<&'a ToolCall>),
            /// A maximal consecutive run of calls that are not concurrency-
            /// safe by themselves. Still checked for a leading pairwise-
            /// disjoint-write prefix (`parallel_write_prefix`); the remainder
            /// runs strictly sequentially.
            Other(Vec<&'a ToolCall>),
        }

        let mut runs: Vec<ToolRun> = Vec::new();
        for call in response.tool_calls.iter() {
            // `can_dispatch_readonly_fast` must never panic here — an
            // unclassifiable call has to fall closed to serial, not abort the
            // whole turn (mirrors claude-code's isConcurrencySafe try/catch ->
            // false). It only reads self.tools/self.assurance_broker/
            // self.permission_verdict_for and returns a bool, no unwraps on
            // caller-controlled data, so it cannot panic on a malformed call.
            let safe = self.can_dispatch_readonly_fast(&call);
            match runs.last_mut() {
                Some(ToolRun::Read(run)) if safe => run.push(call),
                Some(ToolRun::Other(run)) if !safe => run.push(call),
                _ => {
                    if safe {
                        runs.push(ToolRun::Read(vec![call]));
                    } else {
                        runs.push(ToolRun::Other(vec![call]));
                    }
                }
            }
        }

        for run in runs {
            match run {
                ToolRun::Read(calls) => {
                    // Bounded (default 10, `RIGHTKIT_MAX_CONCURRENT_TOOL_CALLS`)
                    // so an unusually large read-only run cannot fan out an
                    // unbounded number of concurrent tool executions at once.
                    self.dispatch_concurrent_batch_bounded(&calls, &mut turn_results)
                        .await;
                }
                ToolRun::Other(calls) => {
                    // CR-07 (Invariant X): a mutating tool never executes
                    // concurrently with another tool. Path-prefix
                    // disjointness (the former `parallel_write_prefix`
                    // leading-batch optimization) is NOT a sufficient
                    // transaction boundary — symlinks, case-folding,
                    // generated files, repo hooks, shared indexes,
                    // process-global state, and multi-file invariants can
                    // still overlap even when the raw paths look disjoint.
                    // Every call in a non-concurrency-safe run therefore
                    // executes strictly sequentially, one at a time, with no
                    // batch fan-out. Concurrency is permitted ONLY for the
                    // `ToolRun::Read` case above, whose calls are already
                    // gated as side-effect-free and idempotent by
                    // `can_dispatch_readonly_fast`.
                    for call in &calls {
                        self.dispatch_tool_call(call, &mut turn_results).await;
                    }
                }
            }
        }

        for call in &response.tool_calls {
            if let Some(result) = turn_results.get(&call.id) {
                self.stall_guardrail.observe_result(call, &result.content);
            }
        }

        // ─── Block 1: typed unbacked-mutation guard ────────────────────────
        // A WriteFile-class success with an empty `mutations` list is a
        // contract violation: the tool claims to have edited something but the
        // engine cannot see what landed. Read-only turns never produce an
        // unbacked mutation (their capability class is filtered out before the
        // detector sees them) so this branch stays free of edit-check work.
        let unbacked =
            crate::post_turn_gate::detect_unbacked_mutations(&response.tool_calls, &turn_results);
        if !unbacked.is_empty() {
            let mut touched_paths: Vec<std::path::PathBuf> = Vec::new();
            for result in turn_results.values() {
                for m in &result.mutations {
                    touched_paths.push(m.path.clone());
                }
            }
            touched_paths.sort();
            touched_paths.dedup();
            match post_turn_gate.after_unbacked_mutations(&unbacked, &touched_paths) {
                crate::post_turn_gate::PostTurnGateDecision::Reenter {
                    instruction,
                    reason,
                } => {
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
                    return ToolDispatchOutcome::Continue;
                }
                crate::post_turn_gate::PostTurnGateDecision::ForceFinish { warning } => {
                    self.transcript.push_user(warning.clone());
                    return ToolDispatchOutcome::Finished(warning);
                }
                crate::post_turn_gate::PostTurnGateDecision::Proceed => {}
            }
        }

        // ─── Block 2: bounded post-edit verification fan-out ───────────────
        // Change detection reads typed mutation results, then applicable
        // verifiers fan out under bounded budgets. The rendered follow-up is
        // injected before completion so actionable findings re-enter the
        // turn; a required verification gap arms pending verification so
        // completion re-verifies the latest bytes, while optional-only gaps
        // are reported without forcing reentry or ending the turn.
        if let Some(outcome) = self
            .post_edit_verify_gate(&turn_results, iteration, sid, pending_verify)
            .await
        {
            return outcome;
        }

        ToolDispatchOutcome::Proceed
    }
}

impl SessionEngine {
    /// Complete same post-dispatch gate used by 5a after streaming results
    /// have been drained in call order.
    pub(in crate::engine) async fn post_streaming_tool_checks(
        &mut self,
        response: &mut crate::llm::LlmResponse,
        iteration: usize,
        sid: &str,
        post_turn_gate: &mut crate::post_turn_gate::PostTurnGateState,
        turn_results: &std::collections::HashMap<
            String,
            rightkit_agent_tools::coding_tools::ToolResult,
        >,
        pending_verify: &mut crate::verify_fanout::PendingVerification,
    ) -> ToolDispatchOutcome {
        for call in &response.tool_calls {
            if let Some(result) = turn_results.get(&call.id) {
                self.stall_guardrail.observe_result(call, &result.content);
            }
        }
        let unbacked =
            crate::post_turn_gate::detect_unbacked_mutations(&response.tool_calls, turn_results);
        if !unbacked.is_empty() {
            let mut touched_paths = Vec::new();
            for result in turn_results.values() {
                for mutation in &result.mutations {
                    touched_paths.push(mutation.path.clone());
                }
            }
            touched_paths.sort();
            touched_paths.dedup();
            match post_turn_gate.after_unbacked_mutations(&unbacked, &touched_paths) {
                crate::post_turn_gate::PostTurnGateDecision::Reenter {
                    instruction,
                    reason,
                } => {
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
                    return ToolDispatchOutcome::Continue;
                }
                crate::post_turn_gate::PostTurnGateDecision::ForceFinish { warning } => {
                    self.transcript.push_user(warning.clone());
                    return ToolDispatchOutcome::Finished(warning);
                }
                crate::post_turn_gate::PostTurnGateDecision::Proceed => {}
            }
        }

        if let Some(outcome) = self
            .post_edit_verify_gate(turn_results, iteration, sid, pending_verify)
            .await
        {
            return outcome;
        }
        ToolDispatchOutcome::Proceed
    }
}
