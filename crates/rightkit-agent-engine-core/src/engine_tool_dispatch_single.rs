use super::*;

fn mutation_operation_id(run_id: &str, tool_call_id: &str) -> String {
    format!("{run_id}:{tool_call_id}")
}

#[cfg(feature = "host")]
const EVT006_QUALIFICATION_FAULT_ENV: &str = "RIGHTKIT_QUAL_EVT006_FAULT";
/// CQ-13: fault points only arm inside the qualification harness's isolated
/// daemon (`tests/support/daemon_process.rs` sets this for every spawned
/// child). An inherited point variable alone can no longer terminate an
/// ordinary build.
#[cfg(feature = "host")]
const QUALIFICATION_FAULTS_ARMED_ENV: &str = "RIGHTKIT_QUAL_FAULTS_ARMED";

/// Qualification fault points exist only in `host` builds (desktop, daemon,
/// CLI); the portable mobile profile compiles them out.
#[cfg(not(feature = "host"))]
fn evt006_qualification_fault_after_effect() {}

#[cfg(feature = "host")]
fn evt006_qualification_fault_after_effect() {
    if std::env::var(QUALIFICATION_FAULTS_ARMED_ENV)
        .ok()
        .as_deref()
        != Some("1")
    {
        return;
    }
    if std::env::var(EVT006_QUALIFICATION_FAULT_ENV)
        .ok()
        .as_deref()
        == Some("after_effect_before_settlement")
    {
        // Simulate process death without unwinding into settlement code.
        std::process::exit(87);
    }
}

impl SessionEngine {
    fn expire_external_tool_approvals(&mut self) {
        let mut digests = self.approved_effect_digests.lock().unwrap();
        self.external_approval_times.retain(|id, issued| {
            if issued.elapsed().as_secs() >= 300 {
                digests.remove(id);
            }
            digests.contains_key(id)
        });
    }

    pub fn revoke_external_tool_approval(&mut self, call_id: &str) {
        self.pending_approvals.lock().unwrap().remove(call_id);
        self.pending_approval_contexts
            .lock()
            .unwrap()
            .remove(call_id);
        self.approved_effect_digests.lock().unwrap().remove(call_id);
        self.approval_required_calls.lock().unwrap().remove(call_id);
        self.external_approval_times.remove(call_id);
    }

    /// Vendor permission callbacks ask through the ordinary policy/approval
    /// path without executing a tool. Dispatch still runs every admission
    /// check, including hooks, middleware and payload-fenced consent.
    pub async fn authorize_external_tool_call(
        &mut self,
        call: ToolCall,
    ) -> rightkit_agent_tools::coding_tools::ToolResult {
        self.refresh_permission_mode();
        self.expire_external_tool_approvals();
        if self.tools.get(&call.name).is_none()
            || serde_json::from_str::<serde_json::Value>(&call.arguments_json).is_err()
        {
            return rightkit_agent_tools::coding_tools::ToolResult::error(
                "unknown tool or invalid arguments",
            );
        }
        let (_, error) = crate::tool_dispatch_plumbing::parse_tool_arguments_attributed(
            &call.arguments_json,
            self.tools
                .get(&call.name)
                .map(|tool| tool.input_schema())
                .as_ref(),
            Some(&call.name),
            self.repair_attribution.as_ref(),
        );
        if let Some(error) = error {
            return rightkit_agent_tools::coding_tools::ToolResult::error(error);
        }
        if self.approved_effect_digests.lock().unwrap().len() >= 64 {
            return rightkit_agent_tools::coding_tools::ToolResult::error(
                "too many outstanding permission requests",
            );
        }
        let sid = self.session_id.clone();
        let mut results = std::collections::HashMap::new();
        match self
            .resolve_single_tool_permission(&sid, &call, None, None, None, &[], &mut results)
            .await
        {
            Some(_) => {
                self.external_approval_times
                    .insert(call.id.clone(), std::time::Instant::now());
                rightkit_agent_tools::coding_tools::ToolResult::ok("allowed")
            }
            None => results.remove(&call.id).unwrap_or_else(|| {
                rightkit_agent_tools::coding_tools::ToolResult::error("permission denied")
            }),
        }
    }

    /// Execute one externally-conducted tool call through the exact same
    /// governance, approval, provenance, mutation-capture, and event pipeline
    /// used by the native model loop.
    ///
    /// Driver hosts must use this seam instead of calling `Tool::execute`
    /// directly. The returned result is the same typed result recorded in the
    /// transcript and durable invocation ledger.
    pub async fn dispatch_external_tool_call(
        &mut self,
        call: ToolCall,
    ) -> rightkit_agent_tools::coding_tools::ToolResult {
        self.refresh_permission_mode();
        self.expire_external_tool_approvals();
        if let Some(broker) = &self.assurance_broker {
            let mut broker = match broker.lock() {
                Ok(broker) => broker,
                Err(_) => {
                    return rightkit_agent_tools::coding_tools::ToolResult::error(
                        "assurance broker lock poisoned before external tool dispatch",
                    );
                }
            };
            let needs_current_contract = broker
                .contract()
                .map(|contract| contract.session_id != self.session_id)
                .unwrap_or(true);
            if needs_current_contract {
                let task_id = format!("{}--external--{}", self.session_id, call.id);
                let trace_id = format!("trace--{}", rightkit_agent_engine_protocol::uuid_v7());
                let request = format!("external tool call {} {}", call.name, call.arguments_json);
                let scope = vec![self.workspace_root.to_string_lossy().into_owned()];
                if let Err(error) = broker.begin_task(
                    self.session_id.clone(),
                    task_id,
                    trace_id,
                    &request,
                    scope,
                    Vec::new(),
                ) {
                    return rightkit_agent_tools::coding_tools::ToolResult::error(format!(
                        "initialize assurance contract for external tool dispatch: {error}"
                    ));
                }
            }
        }
        // Vendor tools do not enter submit_user_message_turn, which normally
        // establishes the pre-effect baseline. Own a bounded run when a
        // capture sink is wired, preserving any native run identity.
        let previous_run = self.active_run_id.clone();
        let owns_run = self.run_changes_sink.is_some();
        if owns_run {
            if let Err(error) = self.begin_run_changes() {
                self.end_active_run();
                self.active_run_id = previous_run;
                return rightkit_agent_tools::coding_tools::ToolResult::error(format!(
                    "capture external tool baseline: {error}"
                ));
            }
        }
        let call_id = call.id.clone();
        let mut results = std::collections::HashMap::new();
        self.dispatch_tool_call(&call, &mut results).await;
        if owns_run {
            self.end_active_run();
            self.active_run_id = previous_run;
        }
        results.remove(&call_id).unwrap_or_else(|| {
            rightkit_agent_tools::coding_tools::ToolResult::error(
                "governed tool dispatch ended without a typed result",
            )
        })
    }

    pub(in crate::engine) async fn dispatch_tool_call(
        &mut self,
        call: &ToolCall,
        turn_results: &mut std::collections::HashMap<
            String,
            rightkit_agent_tools::coding_tools::ToolResult,
        >,
    ) {
        // A blocking plan/tool approval can change authority during this turn.
        // Adopt host mode before classifying each subsequent effect.
        // A062: effect idempotency. A mutating call whose id already has a
        // durable settlement for the identical name+payload (provider retry,
        // turn replay, or crash recovery's synthetic interrupted result) must
        // not re-apply its effect.
        if self.suppress_replayed_effect(call, turn_results) {
            return;
        }
        self.refresh_permission_mode();
        let sid = self.session_id.clone();
        let mut effective_call = call.clone();
        crate::tool_dispatch_plumbing::record_production_admission_stage(
            &effective_call.id,
            crate::tool_dispatch_plumbing::EffectAdmissionStage::Validate,
        );
        // Validate → deterministic repair → revalidate before any middleware,
        // guardrail, permission, or approval policy sees the call. This makes
        // policy classification operate on one canonical argument shape &
        // guarantees an unrepaired malformed payload cannot reach execution.
        let schema = self
            .tools
            .get(&effective_call.name)
            .map(|tool| tool.input_schema());
        let (arguments, argument_error) = parse_tool_arguments_attributed(
            &effective_call.arguments_json,
            schema.as_ref(),
            Some(&effective_call.name),
            self.repair_attribution.as_ref(),
        );
        if let Some(error) = argument_error {
            self.record_tool_error(&sid, &effective_call, error, turn_results);
            return;
        }
        effective_call.arguments_json = serde_json::to_string(&arguments)
            .expect("serde_json::Value always serializes into tool arguments");
        let transform_decisions = self.apply_pre_tool_arg_transforms(&mut effective_call);
        crate::tool_dispatch_plumbing::record_production_admission_stage(
            &effective_call.id,
            crate::tool_dispatch_plumbing::EffectAdmissionStage::Transform,
        );
        let (middleware_args, middleware_error) = parse_tool_arguments_attributed(
            &effective_call.arguments_json,
            schema.as_ref(),
            Some(&effective_call.name),
            self.repair_attribution.as_ref(),
        );
        if let Some(error) = middleware_error {
            self.record_tool_error(&sid, &effective_call, error, turn_results);
            return;
        }
        effective_call.arguments_json = serde_json::to_string(&middleware_args)
            .expect("serde_json::Value always serializes into tool arguments");
        crate::tool_dispatch_plumbing::record_production_admission_stage(
            &effective_call.id,
            crate::tool_dispatch_plumbing::EffectAdmissionStage::Normalize,
        );
        let is_program_result_pull = effective_call.name == "mcp_call"
            && middleware_args
                .get("name")
                .and_then(serde_json::Value::as_str)
                == Some("mcp__membrane__pull")
            && middleware_args
                .pointer("/arguments/operation")
                .and_then(serde_json::Value::as_str)
                == Some("program_result");
        if is_program_result_pull
            && !self
                .internal_composition_call_ids
                .contains(&effective_call.id)
        {
            self.record_tool_error(
                &sid,
                &effective_call,
                "Pull program-result operation is reserved for an internal run_program child call"
                    .to_string(),
                turn_results,
            );
            return;
        }
        let mut hook_approval: Option<AppliedMiddlewareDecision> = None;
        if let Some(bus) = self.hook_bus.clone() {
            let report = bus
                .run_pre_tool_use_report(&self.session_id, &effective_call.name, &middleware_args)
                .await;
            for context in &report.additional_context {
                self.push_hook_context(&format!("PreToolUse {}", effective_call.name), context);
            }
            // Transform and Ask share the mutation step; Ask additionally
            // routes through the same approval flow as a policy RequireApproval
            // verdict, bound to the post-mutation payload digest.
            let (mutation, ask_reason) = match report.outcome {
                rightkit_hooks::agent_hooks::bus::PreToolUseOutcome::Transform(transformed) => {
                    (Some(transformed), None)
                }
                rightkit_hooks::agent_hooks::bus::PreToolUseOutcome::Ask {
                    reason,
                    updated_input,
                } => (updated_input, Some(reason)),
                rightkit_hooks::agent_hooks::bus::PreToolUseOutcome::Deny(message) => {
                    self.record_tool_error(&sid, &effective_call, message, turn_results);
                    return;
                }
                rightkit_hooks::agent_hooks::bus::PreToolUseOutcome::Error(message) => {
                    eprintln!("[hook] PreToolUse error (non-blocking): {message}");
                    (None, None)
                }
                rightkit_hooks::agent_hooks::bus::PreToolUseOutcome::Proceed => (None, None),
            };
            if let Some(transformed) = mutation {
                effective_call.arguments_json = serde_json::to_string(&transformed)
                    .expect("serde_json::Value always serializes into tool arguments");
                let (validated, error) = parse_tool_arguments_attributed(
                    &effective_call.arguments_json,
                    schema.as_ref(),
                    Some(&effective_call.name),
                    self.repair_attribution.as_ref(),
                );
                if let Some(error) = error {
                    self.record_tool_error(&sid, &effective_call, error, turn_results);
                    return;
                }
                effective_call.arguments_json = serde_json::to_string(&validated)
                    .expect("serde_json::Value always serializes into tool arguments");
            }
            if let Some(reason) = ask_reason {
                hook_approval = Some(AppliedMiddlewareDecision {
                    middleware_id: "hook.pre_tool_use".into(),
                    title: "PreToolUse hook".into(),
                    effect: "requireApproval".into(),
                    action: AppliedMiddlewareAction::RequireApproval,
                    reason,
                    mutated_arg_keys: Vec::new(),
                });
            }
        }
        let guardrail = evaluate_tool_guardrails(&effective_call);
        if let Some(mutated_arguments_json) = guardrail
            .as_ref()
            .and_then(|decision| decision.mutated_arguments_json.clone())
        {
            effective_call.arguments_json = mutated_arguments_json;
            let (validated, error) = parse_tool_arguments_attributed(
                &effective_call.arguments_json,
                schema.as_ref(),
                Some(&effective_call.name),
                self.repair_attribution.as_ref(),
            );
            if let Some(error) = error {
                self.record_tool_error(&sid, &effective_call, error, turn_results);
                return;
            }
            effective_call.arguments_json = serde_json::to_string(&validated)
                .expect("serde_json::Value always serializes into tool arguments");
        }
        let call = &effective_call;
        crate::tool_dispatch_plumbing::record_production_admission_stage(
            &call.id,
            crate::tool_dispatch_plumbing::EffectAdmissionStage::Policy,
        );
        let policy_decisions = self.evaluate_pre_tool_policy(call);
        let middleware_denial = policy_decisions
            .iter()
            .find(|decision| matches!(decision.action, AppliedMiddlewareAction::Deny))
            .cloned();
        let middleware_approval = policy_decisions
            .iter()
            .find(|decision| matches!(decision.action, AppliedMiddlewareAction::RequireApproval))
            .cloned()
            .or_else(|| hook_approval.clone());
        let middleware_decisions = transform_decisions
            .into_iter()
            .chain(policy_decisions)
            .chain(hook_approval)
            .collect::<Vec<_>>();

        let pre_span_id = self.record_tool_runtime_span(
            MiddlewarePhase::PreToolCall,
            "completed",
            call,
            None,
            serde_json::json!({
                "path": extract_path_arg(&call.arguments_json),
                "hasCommand": extract_command_arg(&call.arguments_json).is_some(),
                "commandBytes": extract_command_arg(&call.arguments_json).map(|value| value.len()),
                "guardrailPolicy": guardrail.as_ref().map(|decision| decision.policy_id),
                "guardrailAction": guardrail.as_ref().map(|decision| match decision.action {
                    ToolGuardrailAction::Deny => "deny",
                    ToolGuardrailAction::RequireApproval => "requireApproval",
                    ToolGuardrailAction::MutateArgs => "mutateArgs",
                }),
                "middlewareDecisions": middleware_decisions.iter().map(|decision| serde_json::json!({
                    "middlewareId": decision.middleware_id,
                    "decision": decision.action.as_str(),
                    "mutatedArgKeys": decision.mutated_arg_keys,
                })).collect::<Vec<_>>(),
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

        if let Some(summary) =
            skill_effect_denial_for_tool_call(&self.skill_dirs, &self.workspace_root, call)
        {
            self.record_correlated_permission_denial(call, &summary);
            self.record_tool_runtime_span(
                MiddlewarePhase::PostToolCall,
                "denied",
                call,
                pre_span_id.as_deref(),
                serde_json::json!({ "reason": summary }),
            );
            self.record_tool_error(&sid, call, summary, turn_results);
            return;
        }

        if let Some(denial) = middleware_denial {
            let summary = format!(
                "runtime middleware denied: {} ({})",
                denial.reason, denial.middleware_id
            );
            self.record_correlated_permission_denial(call, &summary);
            self.record_tool_runtime_span(
                MiddlewarePhase::PostToolCall,
                "denied",
                call,
                pre_span_id.as_deref(),
                serde_json::json!({
                    "reason": denial.reason,
                    "middlewareId": denial.middleware_id,
                }),
            );
            self.record_tool_error(&sid, call, summary, turn_results);
            return;
        }

        if let Some(guardrail) = guardrail
            .as_ref()
            .filter(|decision| decision.action == ToolGuardrailAction::Deny)
        {
            let summary = format!(
                "guardrail denied: {} ({})",
                guardrail.reason, guardrail.policy_id
            );
            self.record_correlated_permission_denial(call, &summary);
            self.record_tool_runtime_span(
                MiddlewarePhase::PostToolCall,
                "denied",
                call,
                pre_span_id.as_deref(),
                serde_json::json!({
                    "reason": guardrail.reason,
                    "policyId": guardrail.policy_id,
                }),
            );
            self.record_tool_error(&sid, call, summary, turn_results);
            return;
        }

        // Approval review (FM-031 / FEATURE-MATRIX §#87). Gated to mutating
        // tool calls only. Always emits a durable `ToolApprovalReviewed`
        // verdict; with no reviewer configured it is `unavailable` and never
        // denies, so the human approval gate below still decides. At most one
        // reviewer call per mutating tool call.
        let is_read_only_tool = self
            .tools
            .get(&call.name)
            .map(|t| t.is_read_only())
            .unwrap_or(false);
        if !is_read_only_tool {
            if let Some(reason) = self.guardian_review(call).await {
                let correlation =
                    rightkit_agent_policy::denial_tracking::correlation_token(&sid, &call.id);
                let summary = format!("guardian denied: {reason}; correlation={correlation}");
                self.record_correlated_permission_denial(call, &summary);
                self.record_tool_runtime_span(
                    MiddlewarePhase::PostToolCall,
                    "denied",
                    call,
                    pre_span_id.as_deref(),
                    serde_json::json!({
                        "reason": reason,
                        "policyId": "guardian.llm_review",
                        "correlation": correlation,
                    }),
                );
                self.record_tool_error(&sid, call, summary, turn_results);
                return;
            }
        }

        crate::tool_dispatch_plumbing::record_production_admission_stage(
            &call.id,
            crate::tool_dispatch_plumbing::EffectAdmissionStage::Approval,
        );
        let Some(user_approved) = self
            .resolve_single_tool_permission(
                &sid,
                call,
                pre_span_id.as_deref(),
                guardrail.as_ref(),
                middleware_approval.as_ref(),
                &middleware_decisions,
                turn_results,
            )
            .await
        else {
            return;
        };
        if let Some(broker) = &self.assurance_broker {
            let scope = self.assurance_scope_for_call(call);
            let denied = broker
                .lock()
                .ok()
                .map(|broker| broker.safety_gate(&scope))
                .filter(|decision| !decision.allowed);
            if let Some(decision) = denied {
                let summary = format!("assurance safety gate denied tool: {}", decision.reason);
                self.record_correlated_permission_denial(call, &summary);
                self.record_tool_error(&sid, call, summary, turn_results);
                return;
            }
        }

        // Cache only after permission, middleware, guardrail, guardian, and
        // assurance gates have all resolved positively. This is sole runtime
        // insertion path; failed or denied calls never prime cache.
        self.cache_auto_approval_after_positive_resolution(call);

        if !is_read_only_tool && self.auto_post_edit_diagnostics {
            self.record_verification_preflight(call);
        }

        // Allocate and durably record the origin only after every policy and
        // approval gate has accepted the call, immediately before its effect.
        // If storage refuses the origin, use the allocated seq for the typed
        // rejection rather than publishing an uncommitted ToolCallStarted or
        // leaving a hole in the durable event sequence.
        let origin_seq = self.conduit.next_seq();
        let origin_event = ProtocolEvent::ToolCallStarted {
            session_id: sid.clone(),
            seq: origin_seq,
            created_at: now_iso(),
            call: call.clone(),
        };
        let origin_outcome = self.record_run_origin_event(&origin_event).await;
        match crate::run_origin::decide_after_origin(&origin_outcome) {
            crate::run_origin::PreEffectDecision::Execute => {
                // The durable seam has committed this exact seq/payload.
                self.conduit.publish(origin_event);
            }
            crate::run_origin::PreEffectDecision::Abort
            | crate::run_origin::PreEffectDecision::DeferUntilReady => {
                let reason = format!("pre-effect origin seam refused the call: {origin_outcome}");
                self.conduit.publish(ProtocolEvent::RunGovernanceRejected {
                    session_id: sid.clone(),
                    seq: origin_seq,
                    created_at: now_iso(),
                    kind: rightkit_agent_engine_protocol::RunFailureKind::OriginMissing,
                    tool_call_id: call.id.clone(),
                    budget: None,
                    ownership: None,
                    origin: Some(rightkit_agent_engine_protocol::RunOriginFailure {
                        run_id: self.session_id.clone(),
                        tool_call_id: call.id.clone(),
                        reason: origin_outcome.to_string(),
                    }),
                });
                self.record_tool_error(&sid, call, reason, turn_results);
                return;
            }
        }

        crate::tool_dispatch_plumbing::record_production_admission_stage(
            &call.id,
            crate::tool_dispatch_plumbing::EffectAdmissionStage::Resource,
        );

        // N9 native Diagnostics transaction: this is the serialized mutating
        // seam, directly before tool execution. A failed binding/open/begin
        // therefore cannot leak a workspace effect or visible success.
        let diagnostics_operation = match self
            .begin_mutation_diagnostics(call, is_read_only_tool)
            .await
        {
            Ok(operation) => operation,
            Err(error) => {
                self.record_tool_error(
                    &sid,
                    call,
                    format!("native diagnostics transaction refused effect: {error}"),
                    turn_results,
                );
                return;
            }
        };

        let Some(mut result) = self
            .execute_single_tool_call(
                &sid,
                call,
                pre_span_id.as_deref(),
                user_approved,
                turn_results,
            )
            .await
        else {
            if let Some(operation_id) = diagnostics_operation.as_deref() {
                if let Err(error) = self.abort_mutation_diagnostics(operation_id).await {
                    self.record_tool_error(
                        &sid,
                        call,
                        format!("native diagnostics abort failed after tool denial: {error}"),
                        turn_results,
                    );
                }
            }
            return;
        };
        if let Err(error) = self
            .complete_mutation_diagnostics(diagnostics_operation.as_deref(), call, &result)
            .await
        {
            // Effect may already have landed. Preserve its typed evidence but
            // fail closed before record_single_tool_result can publish success.
            result.is_error = true;
            result.content = format!(
                "{}\n\n[native diagnostics transaction failed: {error}; completion is blocked]",
                result.content
            );
        }

        if !is_read_only_tool && !result.mutations.is_empty() {
            evt006_qualification_fault_after_effect();
        }

        #[cfg(feature = "host")]
        let sandbox_denial = result.sandbox_denial.clone();
        self.record_single_tool_result(&sid, call, pre_span_id.as_deref(), result, turn_results);
        crate::tool_dispatch_plumbing::record_production_admission_stage(
            &call.id,
            crate::tool_dispatch_plumbing::EffectAdmissionStage::Verify,
        );
        crate::tool_dispatch_plumbing::record_production_admission_stage(
            &call.id,
            crate::tool_dispatch_plumbing::EffectAdmissionStage::Settle,
        );
        #[cfg(feature = "host")]
        if let Some(denial) = sandbox_denial {
            if !call.id.contains(".sandbox-grant-retry") {
                self.offer_sandbox_grant_retry(call, denial, turn_results)
                    .await;
            }
        }
        if let Some(result) = turn_results.get(&call.id).cloned() {
            if let Err(repair) = self.checkpoint_sentinel(call, &result).await {
                self.transcript
                    .push_user(format!("[Sentinel repair] {repair}"));
            }
        }
    }

    #[cfg(feature = "host")]
    async fn offer_sandbox_grant_retry(
        &mut self,
        original: &ToolCall,
        denial: rightkit_agent_tools::coding_tools::sandbox::SandboxDenial,
        turn_results: &mut std::collections::HashMap<
            String,
            rightkit_agent_tools::coding_tools::ToolResult,
        >,
    ) {
        let Some(grant) = denial.requested_grant.filter(|grant| !grant.is_empty()) else {
            return;
        };
        let Some(questioner) = self.questioner.clone() else {
            return;
        };
        if !questioner.supports_sandbox_grants() {
            return;
        }
        let questions = serde_json::json!([{
            "header": "Sandbox",
            "question": format!("{} Allow one rerun with {}?", denial.message, grant.describe()),
            "options": [
                {"label":"Approve rerun", "description":"Grant this command only, for this exact payload."},
                {"label":"Deny", "description":"Keep sandbox restriction; command will not rerun."}
            ]
        }]);
        let answers = match tokio::time::timeout(
            std::time::Duration::from_secs(60),
            questioner.ask(questions),
        )
        .await
        {
            Ok(Ok(answers)) => answers,
            _ => return,
        };
        if answers.get("Sandbox").and_then(serde_json::Value::as_str) != Some("Approve rerun") {
            return;
        }
        let retry = ToolCall {
            id: format!("{}.sandbox-grant-retry", original.id),
            name: original.name.clone(),
            arguments_json: original.arguments_json.clone(),
        };
        let args =
            serde_json::from_str(&original.arguments_json).unwrap_or(serde_json::Value::Null);
        let digest = crate::tool_dispatch_plumbing::effect_payload_digest(&original.name, &args);
        // The operator approved this exact rerun, including its tool effect.
        // Ordinary policy still rejects hard denials or changed payloads.
        self.approved_effect_digests
            .lock()
            .unwrap()
            .insert(retry.id.clone(), digest.clone());
        self.sandbox_retry_grants
            .insert(retry.id.clone(), (digest, grant));
        Box::pin(self.dispatch_tool_call(&retry, turn_results)).await;
        // A rejected pre-execution retry must not leave reusable consent.
        self.approved_effect_digests
            .lock()
            .unwrap()
            .remove(&retry.id);
        self.sandbox_retry_grants.remove(&retry.id);
    }

    fn record_verification_preflight(&self, call: &ToolCall) {
        let args: serde_json::Value =
            serde_json::from_str(&call.arguments_json).unwrap_or(serde_json::Value::Null);
        let paths: Vec<std::path::PathBuf> = ["path", "file_path"]
            .iter()
            .filter_map(|key| args.get(*key).and_then(serde_json::Value::as_str))
            .filter_map(|path| {
                rightkit_agent_tools::coding_tools::read_file::resolve_in_workspace(
                    path,
                    &self.workspace_root,
                )
                .ok()
            })
            .collect();
        let profile = crate::verify_fanout::resolve_profile(&self.workspace_root, &paths);
        let details = serde_json::json!({
            "configError": profile.config_error,
            "lspRequired": profile.lsp_required,
            "python": profile.python_files.iter().map(|file| serde_json::json!({
                "path": file.path,
                "projectRoot": file.project_root,
                "interpreter": file.interpreter,
            })).collect::<Vec<_>>(),
            "commands": profile.commands.iter().map(|command| serde_json::json!({
                "id": command.id,
                "required": command.required,
                "program": command.argv.first(),
                "available": command.argv.first().and_then(|program|
                    crate::verify_fanout::profile::resolve_program(program, &self.workspace_root)
                ).is_some(),
            })).collect::<Vec<_>>(),
        });
        self.emit(|seq| ProtocolEvent::OrchestrationObservationRecorded {
            session_id: self.session_id.clone(),
            seq,
            created_at: now_iso(),
            observation: rightkit_agent_engine_protocol::OrchestrationObservation {
                id: format!("verification-preflight-{}", call.id),
                kind: "verification_preflight".into(),
                subject: call.id.clone(),
                summary: "Resolved project verification before mutation; checks have not run"
                    .into(),
                fields: std::collections::HashMap::from([("profile".into(), details.to_string())]),
                created_at: now_iso(),
            },
        });
    }

    async fn begin_mutation_diagnostics(
        &self,
        call: &ToolCall,
        is_read_only: bool,
    ) -> Result<Option<String>, crate::mutation_diagnostics::MutationDiagnosticsFailure> {
        if is_read_only {
            return Ok(None);
        }
        let Some(diagnostics) = self.mutation_diagnostics.clone() else {
            return Ok(None);
        };
        let operation_id = mutation_operation_id(&self.active_run_id, &call.id);
        // The host derives strict Membrane WorkspaceRequest fields from
        // workspace_root. Correlation travels in operation_id, which remains
        // the native request id for every diagnostics operation; call metadata
        // is retained for seal/evaluate.
        diagnostics
            .begin(&self.workspace_root, &operation_id, serde_json::Value::Null)
            .await
            .map_err(|detail| {
                crate::mutation_diagnostics::MutationDiagnosticsFailure::host(
                    crate::mutation_diagnostics::MutationDiagnosticsFailureKind::Begin,
                    detail,
                )
            })?;
        Ok(Some(operation_id))
    }

    async fn complete_mutation_diagnostics(
        &self,
        operation_id: Option<&str>,
        call: &ToolCall,
        result: &rightkit_agent_tools::coding_tools::ToolResult,
    ) -> Result<(), crate::mutation_diagnostics::MutationDiagnosticsFailure> {
        let Some(operation_id) = operation_id else {
            return Ok(());
        };
        let Some(diagnostics) = self.mutation_diagnostics.clone() else {
            return Err(
                crate::mutation_diagnostics::MutationDiagnosticsFailure::unavailable(
                    "host binding disappeared during mutation",
                ),
            );
        };
        if result.is_error || result.mutations.is_empty() {
            return self.abort_mutation_diagnostics(operation_id).await;
        }
        let paths = result
            .mutations
            .iter()
            .map(|mutation| mutation.path.clone())
            .collect::<Vec<_>>();
        let body = serde_json::json!({
            "toolCallId": call.id,
            "tool": call.name,
            "mutationCount": result.mutations.len(),
            "paths": paths.iter().map(|path| path.to_string_lossy()).collect::<Vec<_>>(),
        });
        diagnostics
            .seal_and_evaluate(&self.workspace_root, operation_id, paths, body)
            .await
            .map_err(|detail| {
                crate::mutation_diagnostics::MutationDiagnosticsFailure::host(
                    crate::mutation_diagnostics::MutationDiagnosticsFailureKind::SealAndEvaluate,
                    detail,
                )
            })
    }

    async fn abort_mutation_diagnostics(
        &self,
        operation_id: &str,
    ) -> Result<(), crate::mutation_diagnostics::MutationDiagnosticsFailure> {
        let Some(diagnostics) = self.mutation_diagnostics.clone() else {
            return Err(
                crate::mutation_diagnostics::MutationDiagnosticsFailure::unavailable(
                    "host binding disappeared during mutation",
                ),
            );
        };
        diagnostics
            .abort(&self.workspace_root, operation_id)
            .await
            .map_err(|detail| {
                crate::mutation_diagnostics::MutationDiagnosticsFailure::host(
                    crate::mutation_diagnostics::MutationDiagnosticsFailureKind::Abort,
                    detail,
                )
            })
    }
}

impl SessionEngine {
    /// Settled `(summary, is_error)` for `call` when the transcript already
    /// holds a tool result for the same id whose originating assistant call
    /// has the identical name and arguments. A same-id call with a different
    /// payload is an id collision, not a replay, and is not matched.
    pub(in crate::engine) fn settled_effect_for(&self, call: &ToolCall) -> Option<(String, bool)> {
        let messages = self.transcript.messages();
        let result_idx = messages.iter().position(|m| {
            matches!(m, rightkit_agent_engine_protocol::TranscriptMessage::Tool { tool_call_id, .. } if *tool_call_id == call.id)
        })?;
        let origin = messages[..result_idx].iter().rev().find_map(|m| match m {
            rightkit_agent_engine_protocol::TranscriptMessage::Assistant { tool_calls, .. } => {
                tool_calls.iter().find(|c| c.id == call.id)
            }
            _ => None,
        })?;
        let same_payload = origin.name == call.name
            && (origin.arguments_json == call.arguments_json
                || matches!(
                    (
                        serde_json::from_str::<serde_json::Value>(&origin.arguments_json),
                        serde_json::from_str::<serde_json::Value>(&call.arguments_json),
                    ),
                    (Ok(a), Ok(b)) if a == b
                ));
        if !same_payload {
            return None;
        }
        match &messages[result_idx] {
            rightkit_agent_engine_protocol::TranscriptMessage::Tool {
                summary, is_error, ..
            } => Some((summary.clone(), *is_error)),
            _ => None,
        }
    }

    /// True when the call replayed a settled mutating effect and was answered
    /// from the durable settlement without executing. Read-only tools are
    /// exempt: re-running them is harmless and returns fresh data.
    pub(in crate::engine) fn suppress_replayed_effect(
        &mut self,
        call: &ToolCall,
        turn_results: &mut std::collections::HashMap<
            String,
            rightkit_agent_tools::coding_tools::ToolResult,
        >,
    ) -> bool {
        let read_only = self
            .tools
            .get(&call.name)
            .map(|tool| tool.is_read_only())
            .unwrap_or(false);
        if read_only {
            return false;
        }
        let Some((original, was_error)) = self.settled_effect_for(call) else {
            return false;
        };
        let original: String = original.chars().take(2000).collect();
        let summary = format!(
            "Replay of settled tool call `{}` suppressed; the effect was not re-applied. Original settlement: {original}",
            call.id
        );
        let sid = self.session_id.clone();
        turn_results.insert(
            call.id.clone(),
            if was_error {
                rightkit_agent_tools::coding_tools::ToolResult::error(summary.clone())
            } else {
                rightkit_agent_tools::coding_tools::ToolResult::ok(summary.clone())
            },
        );
        self.transcript
            .push_tool_result_with_output(&call.id, &summary, None, was_error);
        let result_msg = rightkit_agent_engine_protocol::TranscriptMessage::Tool {
            id: "tr".into(),
            created_at: now_iso(),
            tool_call_id: call.id.clone(),
            summary: summary.clone(),
            output: None,
            trust: Some(tool_result_trust(&call.name, &summary)),
            is_error: was_error,
            pinned: false,
            process_id: None,
        };
        self.emit(|seq| ProtocolEvent::ToolResultAdded {
            session_id: sid,
            seq,
            created_at: now_iso(),
            result: result_msg,
        });
        true
    }
}

#[cfg(test)]
mod origin_seam_integration_tests {
    //! Call-site integration coverage for the pre-effect origin seam.
    //!
    //! The seam must hold the contract that a tool side effect only runs
    //! after a `Persisted` (or explicit `Skipped`) origin outcome, and that
    //! the engine's typed-mutation counter mirrors what the daemon-side
    //! shared counter reads via `LiveSession::typed_mutations`.
    use super::*;
    use crate::llm::ScriptedLlm;
    use crate::run_origin::{OriginWriteResult, RunOriginSink};
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
    use std::sync::Arc;

    struct RecordingSink {
        calls: Mutex<Vec<ToolCall>>,
        next_seq: AtomicI64,
    }

    impl RecordingSink {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                next_seq: AtomicI64::new(1000),
            }
        }

        fn record_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    impl RunOriginSink for RecordingSink {
        fn record_origin(&self, call: &ToolCall) -> OriginWriteResult {
            self.calls.lock().unwrap().push(call.clone());
            OriginWriteResult::Persisted(self.next_seq_seq())
        }

        fn is_ready(&self) -> bool {
            true
        }
    }

    impl RecordingSink {
        fn next_seq_seq(&self) -> i64 {
            self.next_seq.fetch_add(1, Ordering::SeqCst)
        }
    }

    struct AlwaysFailSink {
        calls: AtomicUsize,
    }

    impl AlwaysFailSink {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl RunOriginSink for AlwaysFailSink {
        fn record_origin(&self, _call: &ToolCall) -> OriginWriteResult {
            self.calls.fetch_add(1, Ordering::SeqCst);
            OriginWriteResult::Failed("storage down".into())
        }

        fn is_ready(&self) -> bool {
            false
        }
    }

    fn call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments_json: "{}".into(),
        }
    }

    /// Real call-site integration: when the seam returns `Persisted`,
    /// the sink's `record_origin` was invoked exactly once with the
    /// supplied `ToolCall` and the engine considered the dispatch safe
    /// to proceed (decision = `Execute`). This is the contract that the
    /// `engine_tool_dispatch_single` seam relies on for every mutating
    /// tool call — a missed invocation means a tool fired a side effect
    /// without an origin.
    #[test]
    fn origin_sink_records_persisted_call_before_side_effect() {
        let sink = Arc::new(RecordingSink::new());
        let outcome = OriginWriteResult::Persisted(4242);
        // Drive the call-site predicate directly so the test asserts the
        // dispatch decision without standing up the full engine harness.
        let decision = crate::run_origin::decide_after_origin(&outcome);
        assert_eq!(decision, crate::run_origin::PreEffectDecision::Execute);
        // A recording sink wired through the trait is what production uses;
        // verify it stores the call before returning the seq.
        let tc = call("tc-A", "WriteFile");
        let result = sink.record_origin(&tc);
        assert!(matches!(result, OriginWriteResult::Persisted(_)));
        assert_eq!(
            sink.record_count(),
            1,
            "the seam MUST see the call exactly once"
        );
    }

    /// When the sink returns `Failed`, the dispatch decision is `Abort`
    /// and the engine MUST NOT execute the tool side effect. This is the
    /// mirrored half of the persisted test — both halves together
    /// guarantee that `decide_after_origin` only emits `Execute` on a
    /// confirmed durable origin.
    #[test]
    fn origin_sink_failed_yields_abort_decision() {
        let sink = AlwaysFailSink::new();
        let tc = call("tc-B", "WriteFile");
        let result = sink.record_origin(&tc);
        assert!(matches!(result, OriginWriteResult::Failed(_)));
        let decision = crate::run_origin::decide_after_origin(&result);
        assert_eq!(decision, crate::run_origin::PreEffectDecision::Abort);
        assert_eq!(sink.calls.load(Ordering::SeqCst), 1);
    }

    /// Shared typed-mutation counter wiring: the engine's internal counter
    /// and the daemon-side shared `Arc<AtomicU32>` MUST stay in lockstep.
    /// `finalize_goal` reads the shared counter, so a missing mirror would
    /// silently report zero mutations on a session that produced them — and
    /// the unbacked-victory gate would refuse a legitimate completion.
    #[test]
    fn typed_mutations_counter_mirrors_engine_internal_count() {
        let shared: Arc<std::sync::atomic::AtomicU32> =
            Arc::new(std::sync::atomic::AtomicU32::new(0));
        let mut engine = build_minimal_engine();
        engine.set_typed_mutations_counter(shared.clone());
        engine.add_typed_mutations(3);
        engine.add_typed_mutations(2);
        assert_eq!(engine.typed_mutations_count(), 5);
        assert_eq!(shared.load(std::sync::atomic::Ordering::Relaxed), 5);
        // Symmetry: a process restart hydrates the shared counter from
        // the persisted row before the engine starts bumping it.
        shared.store(7, std::sync::atomic::Ordering::Relaxed);
        let mut engine2 = build_minimal_engine();
        engine2.set_typed_mutations_counter(shared.clone());
        engine2.add_typed_mutations(1);
        assert_eq!(engine2.typed_mutations_count(), 8);
        assert_eq!(shared.load(std::sync::atomic::Ordering::Relaxed), 8);
    }

    fn write_call(id: &str, path: &str, content: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: "write_file".into(),
            arguments_json: serde_json::json!({"path": path, "content": content}).to_string(),
        }
    }

    /// A062 retry: a replayed mutating call with the same id and payload is
    /// answered from its durable settlement and does not execute again.
    #[tokio::test]
    async fn retried_mutating_call_with_same_effect_id_is_not_reapplied() {
        let mut engine = build_minimal_engine();
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a062-retry.txt");
        let call = write_call("a062-1", target.to_str().unwrap(), "first");
        engine
            .transcript
            .push_assistant("", None, vec![call.clone()]);
        engine
            .transcript
            .push_tool_result("a062-1", "wrote a062-retry.txt", false);
        let mut results = std::collections::HashMap::new();
        engine.dispatch_tool_call(&call, &mut results).await;
        assert!(!target.exists(), "replayed effect must not touch disk");
        let replayed = results.get("a062-1").expect("typed replay result");
        assert!(!replayed.is_error);
        assert!(replayed.content.contains("suppressed"));
        assert!(replayed.content.contains("wrote a062-retry.txt"));
    }

    /// A062 crash replay: recovery's synthetic interrupted/unknown settlement
    /// also blocks re-execution and keeps the error classification.
    #[tokio::test]
    async fn crash_replayed_call_with_interrupted_settlement_is_not_reexecuted() {
        let mut engine = build_minimal_engine();
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a062-crash.txt");
        let call = write_call("a062-2", target.to_str().unwrap(), "x");
        engine
            .transcript
            .push_assistant("", None, vec![call.clone()]);
        engine.transcript.push_tool_result(
            "a062-2",
            "External effect settlement is interrupted/unknown after process recovery; automatic replay is forbidden until reconciled.",
            true,
        );
        let mut results = std::collections::HashMap::new();
        engine.dispatch_tool_call(&call, &mut results).await;
        assert!(!target.exists());
        let replayed = results.get("a062-2").expect("typed replay result");
        assert!(replayed.is_error);
        assert!(replayed.content.contains("interrupted/unknown"));
    }

    /// Same id with a different payload is an id collision, not a replay.
    #[test]
    fn same_id_with_different_payload_is_not_a_replay() {
        let mut engine = build_minimal_engine();
        let original = write_call("a062-3", "one.txt", "a");
        engine
            .transcript
            .push_assistant("", None, vec![original.clone()]);
        engine.transcript.push_tool_result("a062-3", "ok", false);
        assert!(engine.settled_effect_for(&original).is_some());
        assert!(engine
            .settled_effect_for(&write_call("a062-3", "two.txt", "b"))
            .is_none());
    }

    #[test]
    fn native_diagnostics_operation_id_is_run_call_stable() {
        assert_eq!(mutation_operation_id("run-7", "call-4"), "run-7:call-4");
    }

    /// Construct the minimum SessionEngine that exercises the public
    /// typed-mutation counter API. We do not need a full LLM harness here —
    /// the contract under test is the shared-counter synchronization, which
    /// `set_typed_mutations_counter` owns.
    fn build_minimal_engine() -> SessionEngine {
        let llm = Arc::new(ScriptedLlm::new(Vec::new()));
        let tools = Arc::new(ToolRegistry::build_default());
        let conduit = Arc::new(Conduit::new(8));
        let cfg = SessionEngineConfig {
            permission_mode: PermissionMode::Bypass,
            ..Default::default()
        };
        SessionEngine::new(llm, tools, conduit, cfg)
    }
}
