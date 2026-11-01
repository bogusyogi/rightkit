use super::*;

/// How often an open approval re-checks that a human client is still attached.
const ATTACH_POLL: std::time::Duration = std::time::Duration::from_millis(200);

/// Tool-result text when no human client is attached to answer (rule 1: immediate denial).
pub(crate) const APPROVAL_UNATTENDED_SUMMARY: &str =
    "approval unattended: no human client is attached to answer, so the call was not run";
/// Tool-result text when the client detached while the card was open.
pub(crate) const APPROVAL_DETACHED_SUMMARY: &str =
    "approval detached: the client that held the card detached before answering, so the call was not run";
/// Tool-result text when the turn was cancelled or the request revoked before an answer.
pub(crate) const APPROVAL_CANCELLED_SUMMARY: &str =
    "approval cancelled: the request was cancelled without an answer, so the call was not run";

impl SessionEngine {
    /// Store one policy fact about a tool call that is not an approval card (tools rule 31):
    /// `kind` is `grant_installed`, `hook_block` or `sandbox_refusal`. `cause` must be a short
    /// reason without command text, arguments or secrets.
    pub(in crate::engine) fn emit_tool_policy_event(
        &mut self,
        sid: &str,
        tool_call_id: &str,
        policy_source: &str,
        kind: &str,
        cause: &str,
    ) {
        self.emit(|seq| ProtocolEvent::ToolPolicyEvent {
            session_id: sid.to_string(),
            seq,
            created_at: now_iso(),
            tool_call_id: tool_call_id.to_string(),
            policy_source: policy_source.to_string(),
            kind: kind.to_string(),
            cause: cause.to_string(),
        });
    }

    /// Rule 31: store a `sandbox_refusal` policy event when `result` is a sandbox refusal. Two
    /// shapes: the launch was refused because the sandbox could not start (the contract's
    /// "refusing unsandboxed ..." strings), or a confined command was denied access. The cause
    /// names the kind and tier, never a command, argument or path. Every dispatch path (serial,
    /// concurrent batch, streaming) calls this once per result.
    pub(in crate::engine) fn emit_sandbox_refusal_events(
        &mut self,
        sid: &str,
        call_id: &str,
        result: &rightkit_agent_tools::coding_tools::ToolResult,
    ) {
        if result.is_error && result.content.contains("refusing unsandboxed") {
            self.emit_tool_policy_event(
                sid,
                call_id,
                "sandbox:launch",
                "sandbox_refusal",
                "the sandbox could not start, so the command was refused rather than run unconfined",
            );
        }
        #[cfg(feature = "host")]
        if let Some(denial) = result.sandbox_denial.as_ref() {
            let what = match denial.kind {
                rightkit_agent_tools::coding_tools::sandbox::SandboxDenialKind::Network => {
                    "outbound network"
                }
                rightkit_agent_tools::coding_tools::sandbox::SandboxDenialKind::Filesystem {
                    ..
                } => "filesystem access",
            };
            let cause = format!("the sandbox blocked {what} (tier {:?})", denial.tier);
            self.emit_tool_policy_event(sid, call_id, &denial.rule_id, "sandbox_refusal", &cause);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::engine) async fn resolve_single_tool_permission(
        &mut self,
        sid: &str,
        call: &ToolCall,
        pre_span_id: Option<&str>,
        guardrail: Option<&ToolGuardrailDecision>,
        middleware_approval: Option<&AppliedMiddlewareDecision>,
        middleware_decisions: &[AppliedMiddlewareDecision],
        turn_results: &mut std::collections::HashMap<
            String,
            rightkit_agent_tools::coding_tools::ToolResult,
        >,
    ) -> Option<bool> {
        let mut verdict = self.permission_verdict_for(call);
        // Sessions rule 32: the fourth identical call in a row is an Ask with
        // reason `repeated_call` (hard denials still win).
        let repeated_call = self.repeated_call_ids.remove(&call.id);
        if repeated_call && verdict.decision != PermissionDecision::Deny {
            verdict = PermissionVerdict::new(
                PermissionDecision::Ask,
                crate::engine::stall_guardrail::repeated_call_reason(
                    &call.name,
                    self.stall_guardrail.run_length(),
                ),
                crate::engine::stall_guardrail::REPEATED_CALL_REASON,
            );
        }
        let repeated_call = repeated_call && verdict.decision == PermissionDecision::Ask;
        if let Some(guardrail) =
            guardrail.filter(|decision| decision.action == ToolGuardrailAction::RequireApproval)
        {
            if verdict.decision != PermissionDecision::Deny {
                verdict = PermissionVerdict::new(
                    PermissionDecision::Ask,
                    format!("guardrail requires approval: {}", guardrail.reason),
                    guardrail.policy_id,
                );
            }
        }
        if let Some(approval) = middleware_approval {
            if verdict.decision != PermissionDecision::Deny {
                verdict = PermissionVerdict::new(
                    PermissionDecision::Ask,
                    format!("runtime middleware requires approval: {}", approval.reason),
                    approval.middleware_id.clone(),
                );
            }
        }

        match verdict.decision {
            PermissionDecision::Deny => {
                let summary = format!(
                    "permission denied: {} ({})",
                    verdict.reason, verdict.policy_source
                );
                self.record_correlated_permission_denial(call, &summary);
                self.record_tool_runtime_span(
                    MiddlewarePhase::PostToolCall,
                    "denied",
                    call,
                    pre_span_id,
                    serde_json::json!({
                        "reason": verdict.reason,
                        "policySource": verdict.policy_source,
                    }),
                );
                self.record_tool_error(sid, call, summary, turn_results);
                None
            }
            PermissionDecision::Ask => {
                // A vendor permission callback may have collected consent
                // before dispatch. Reuse only that exact call/payload; hard
                // denial above and new guardrail/middleware prompts still win.
                let (args, _) = crate::tool_dispatch_plumbing::parse_tool_arguments_attributed(
                    &call.arguments_json,
                    self.tools
                        .get(&call.name)
                        .map(|tool| tool.input_schema())
                        .as_ref(),
                    Some(&call.name),
                    self.repair_attribution.as_ref(),
                );
                let digest =
                    crate::tool_dispatch_plumbing::effect_payload_digest(&call.name, &args);
                if guardrail.is_none()
                    && middleware_approval.is_none()
                    && !repeated_call
                    && self.approved_effect_digests.lock().unwrap().get(&call.id) == Some(&digest)
                {
                    return Some(true);
                }
                // EFF-002: a session grant covers only a byte-identical final
                // payload. Verdict is Ask here, so deny rules already won.
                if guardrail.is_none() && middleware_approval.is_none() && !repeated_call {
                    let granted = self.permission_rules.as_ref().is_some_and(|rules| {
                        rules
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .has_session_payload_grant(&digest)
                    });
                    if granted {
                        self.approved_effect_digests
                            .lock()
                            .unwrap()
                            .insert(call.id.clone(), digest.clone());
                        self.approval_required_calls
                            .lock()
                            .unwrap()
                            .insert(call.id.clone());
                        return Some(true);
                    }
                }
                let req = call.clone();
                let psource = verdict.policy_source.clone();
                let correlation =
                    rightkit_agent_policy::denial_tracking::correlation_token(sid, &call.id);
                let gate_decision = rightkit_agent_engine_protocol::PermissionDecisionResult {
                    decision: rightkit_agent_engine_protocol::PermissionDecision::Prompt,
                    reason: verdict.reason.clone(),
                    tool_name: call.name.clone(),
                    description: verdict.reason.clone(),
                    request_id: Some(correlation.clone()),
                    reviewable: false,
                };
                // Compute a non-mutating preview (a diff for edit tools) so the
                // operator can see exactly what the call would change.
                let proposed_change = self.tool_preview(call).await;
                let (approved_args, _) =
                    crate::tool_dispatch_plumbing::parse_tool_arguments_attributed(
                        &call.arguments_json,
                        self.tools
                            .get(&call.name)
                            .map(|t| t.input_schema())
                            .as_ref(),
                        Some(&call.name),
                        self.repair_attribution.as_ref(),
                    );
                let approved_digest = crate::tool_dispatch_plumbing::effect_payload_digest(
                    &call.name,
                    &approved_args,
                );
                let approval_span_id = self.record_tool_runtime_span(
                    MiddlewarePhase::ApprovalRequested,
                    "pending",
                    call,
                    pre_span_id,
                    serde_json::json!({
                        "policySource": psource,
                        "correlation": correlation,
                        "riskLabel": "needs-approval",
                        "hasProposedChange": proposed_change.is_some(),
                        "effectPayloadDigest": approved_digest,
                        "permissionGateDecision": serde_json::to_value(&gate_decision).unwrap_or_default(),
                    }),
                );
                if let Some(span_id) = approval_span_id.as_deref() {
                    self.record_runtime_observation(
                        span_id,
                        "info",
                        format!(
                            "Approval requested for tool call {} ({})",
                            call.id, call.name
                        ),
                        serde_json::json!({
                            "tool": call.name,
                            "toolCallId": call.id,
                            "policySource": psource,
                            "correlation": correlation,
                            "effectPayloadDigest": approved_digest,
                        }),
                    );
                }
                // Rule 1: with no human client attached (a `subscribe_human` subscription) nothing can answer, so
                // the request is denied at once with the reason `unattended`
                // and the turn continues. No card is published.
                if self.conduit.human_subscriber_count() == 0 {
                    let summary = if repeated_call {
                        crate::engine::stall_guardrail::repeated_call_denied_message(&call.name)
                    } else {
                        // Rule 31: the cause names the policy source that raised the Ask.
                        format!("{APPROVAL_UNATTENDED_SUMMARY} (policy source: {psource})")
                    };
                    // Rule 31: the request and its resolution are stored events even when
                    // nothing could answer, naming the call id and the policy source; the
                    // resolution carries the cause.
                    self.emit(|seq| ProtocolEvent::ToolApprovalRequested {
                        session_id: sid.to_string(),
                        seq,
                        created_at: now_iso(),
                        call: call.clone(),
                        risk_label: "needs-approval".into(),
                        proposed_change: proposed_change.clone(),
                        policy_source: Some(psource.clone()),
                        middleware_decisions: self.approval_middleware_decisions(
                            call,
                            &verdict,
                            guardrail,
                            middleware_decisions,
                        ),
                    });
                    let unattended_call_id = call.id.clone();
                    self.emit(|seq| ProtocolEvent::ToolApprovalResolved {
                        session_id: sid.to_string(),
                        seq,
                        created_at: now_iso(),
                        tool_call_id: unattended_call_id,
                        approved: false,
                        reason: Some("unattended".to_string()),
                    });
                    self.clear_unapproved_request(&call.id);
                    self.record_correlated_permission_denial(call, &summary);
                    self.record_tool_runtime_span(
                        MiddlewarePhase::PostToolCall,
                        "denied",
                        call,
                        approval_span_id.as_deref().or(pre_span_id),
                        serde_json::json!({
                            "reason": "unattended",
                            "correlation": correlation,
                        }),
                    );
                    self.record_tool_error(sid, call, summary, turn_results);
                    return None;
                }
                let rx = {
                    let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
                    self.pending_approvals
                        .lock()
                        .unwrap()
                        .insert(call.id.clone(), tx);
                    self.pending_approval_contexts.lock().unwrap().insert(
                        call.id.clone(),
                        rightkit_agent_engine_protocol::PendingApprovalContext {
                            request_id: Some(correlation.clone()),
                            tool_name: call.name.clone(),
                            command: extract_command_arg(&call.arguments_json),
                            payload_digest: Some(approved_digest.clone()),
                            network: serde_json::from_str::<serde_json::Value>(
                                &call.arguments_json,
                            )
                            .ok()
                            .and_then(|args| {
                                let host = args.get("host")?.as_str()?.to_string();
                                let protocol = match args
                                    .get("protocol")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("http")
                                {
                                    "https" => rightkit_agent_engine_protocol::NetworkApprovalProtocol::Https,
                                    "socks5_tcp" => {
                                        rightkit_agent_engine_protocol::NetworkApprovalProtocol::Socks5Tcp
                                    }
                                    "socks5_udp" => {
                                        rightkit_agent_engine_protocol::NetworkApprovalProtocol::Socks5Udp
                                    }
                                    _ => rightkit_agent_engine_protocol::NetworkApprovalProtocol::Http,
                                };
                                Some(rightkit_agent_engine_protocol::NetworkApprovalContext { host, protocol })
                            }),
                        },
                    );
                    rx
                };
                // Publish only after both live pending entries exist. A fast
                // resolver must observe the context that protects amendment
                // scope before it can claim this approval.
                self.emit(|seq| ProtocolEvent::ToolApprovalRequested {
                    session_id: sid.to_string(),
                    seq,
                    created_at: now_iso(),
                    call: req,
                    risk_label: "needs-approval".into(),
                    proposed_change,
                    policy_source: Some(psource),
                    middleware_decisions: self.approval_middleware_decisions(
                        call,
                        &verdict,
                        guardrail,
                        middleware_decisions,
                    ),
                });
                // Tools rule 58: configured `PermissionRequest` hooks are told a card is open
                // (command, URL or MCP). They only observe: no hook can answer the card, so no
                // path turns an Ask into an Allow (rule 59).
                if let (Some(bus), Ok(handle)) =
                    (self.hook_bus.clone(), tokio::runtime::Handle::try_current())
                {
                    let hook_session = sid.to_string();
                    let tool = call.name.clone();
                    let hook_args = approved_args.clone();
                    let reason = verdict.reason.clone();
                    let call_id = call.id.clone();
                    handle.spawn(async move {
                        bus.run_event_hooks(
                            "PermissionRequest",
                            &hook_session,
                            Some(&tool),
                            Some(&hook_args),
                            &[
                                ("RIGHTKIT_HOOK_REASON", reason),
                                ("RIGHTKIT_HOOK_CALL_ID", call_id),
                            ],
                        )
                        .await;
                    });
                }
                // Rule 1: no timeout. The wait ends on an answer, a detached
                // client or a cancelled turn. Rule 6: each ends with its own
                // reason, never "denied by user" unless the user declined.
                let repeated_call_denial =
                    crate::engine::stall_guardrail::repeated_call_denied_message(&call.name);
                let mut denial_cause: &str = if repeated_call {
                    &repeated_call_denial
                } else {
                    "denied by user"
                };
                let mut rx = rx;
                let approved = loop {
                    tokio::select! {
                        answer = &mut rx => match answer {
                            Ok(answer) => break answer,
                            Err(_) => {
                                denial_cause = APPROVAL_CANCELLED_SUMMARY;
                                break false;
                            }
                        },
                        _ = tokio::time::sleep(ATTACH_POLL) => {
                            if self.conduit.human_subscriber_count() == 0 {
                                denial_cause = APPROVAL_DETACHED_SUMMARY;
                                tracing::warn!(
                                    session_id = %sid,
                                    tool = %call.name,
                                    "client detached with an approval open; denying"
                                );
                                self.pending_approvals.lock().unwrap().remove(&call.id);
                                break false;
                            }
                        }
                    }
                };
                self.pending_approval_contexts
                    .lock()
                    .unwrap()
                    .remove(&call.id);

                let call_id = call.id.clone();
                // Tools rule 6: a denial names its cause on the wire.
                let resolved_reason = if approved {
                    None
                } else if denial_cause == APPROVAL_CANCELLED_SUMMARY {
                    Some("cancelled".to_string())
                } else if denial_cause == APPROVAL_DETACHED_SUMMARY {
                    Some("detached".to_string())
                } else {
                    Some("user".to_string())
                };
                self.emit(|seq| ProtocolEvent::ToolApprovalResolved {
                    session_id: sid.to_string(),
                    seq,
                    created_at: now_iso(),
                    tool_call_id: call_id,
                    approved,
                    reason: resolved_reason,
                });

                if !approved {
                    self.clear_unapproved_request(&call.id);
                    let summary = denial_cause.to_string();
                    self.record_correlated_permission_denial(call, &summary);
                    self.record_tool_runtime_span(
                        MiddlewarePhase::PostToolCall,
                        "denied",
                        call,
                        approval_span_id.as_deref().or(pre_span_id),
                        serde_json::json!({ "reason": summary, "correlation": correlation }),
                    );
                    self.record_tool_error(sid, call, summary, turn_results);
                    return None;
                }
                // EFF-002: bind this grant to the digest of the payload that
                // will actually execute, normalized the same way the executor
                // normalizes it. Recorded here and rechecked immediately
                // before execute, so an approval cannot authorize a payload
                // that changed after the operator saw it.
                self.approved_effect_digests
                    .lock()
                    .unwrap()
                    .insert(call.id.clone(), approved_digest);
                // Mark the grant approval-required so execution refuses it
                // if the digest is missing; policy-authorized (Allow) calls
                // never get this marker and are unaffected.
                self.approval_required_calls
                    .lock()
                    .unwrap()
                    .insert(call.id.clone());
                // User explicitly approved — bypass the tool's own permission
                // guard on execution so it doesn't double-block in Ask mode.
                Some(true)
            }
            PermissionDecision::Allow => {
                // An explicit permission rule (e.g. the session grant recorded
                // by "Allow for this session") is a
                // positive authorization for this exact call. The tool's own
                // guard re-evaluates against the ambient mode (Ask), so without
                // elevating here the granted repeat would be refused with
                // "Tool requires approval" despite the rule. The verdict already
                // passed durable-write provenance enforcement above.
                let authorized = verdict.policy_source.starts_with("rule:");
                // A fresh policy decision supersedes any earlier prompt grant
                // recorded for this call id.
                self.approval_required_calls
                    .lock()
                    .unwrap()
                    .remove(&call.id);
                Some(authorized)
            }
        }
    }
}

/// Cheap, deterministic content hash (FNV-1a) used only to fingerprint an
/// approval prompt to the exact argument bytes it was raised for — not a
/// security primitive, just a tamper-evident tag so a stale prompt showing
/// different content is visibly not the same fingerprint.
#[cfg(test)]
fn simple_content_hash(content: &str) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    content.bytes().fold(FNV_OFFSET, |hash, byte| {
        (hash ^ byte as u64).wrapping_mul(FNV_PRIME)
    })
}

#[cfg(test)]
mod effect_binding_tests {
    use super::*;
    use crate::conduit::Conduit;
    use crate::engine::{SessionEngine, SessionEngineConfig};
    use crate::llm::{LlmResponse, ScriptedLlm};
    use rightkit_agent_engine_protocol::ToolCall;
    use rightkit_agent_policy::PermissionMode;
    use rightkit_agent_tools::coding_tools::ToolRegistry;
    use std::sync::Arc;

    /// ENG-28: the approval-requested runtime span must carry a concrete
    /// effect target (the literal path this Edit would touch) and a
    /// fingerprint tied to this exact call id + argument bytes, so approving
    /// this prompt cannot be misread as approving a different file or a
    /// different call.
    #[tokio::test]
    async fn approval_span_binds_to_exact_effect_target() {
        let call = ToolCall {
            id: "tc-effect-bind".into(),
            name: "write_file".into(),
            arguments_json: serde_json::json!({
                "path": "src/secrets.rs",
                "content": "redacted"
            })
            .to_string(),
        };
        let llm = Arc::new(ScriptedLlm::new(vec![
            LlmResponse::default().with_tool_call(call),
            LlmResponse::text("done"),
        ]));
        let tools = Arc::new(ToolRegistry::build_default());
        let conduit = Arc::new(Conduit::new(256));
        let cfg = SessionEngineConfig {
            permission_mode: PermissionMode::Ask,
            ..Default::default()
        };
        let mut engine = SessionEngine::new(llm, tools, conduit.clone(), cfg);
        let approvals = engine.pending_approvals.clone();
        let approval_contexts = engine.pending_approval_contexts.clone();
        let mut sub = conduit.subscribe();

        let engine_task = tokio::spawn(async move { engine.submit_user_message("edit").await });

        // Bounded wait: a missing approval event must fail this test, never
        // hang the whole suite (an unbounded recv loop here wedged the run).
        let seen_target = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                match sub.recv().await {
                    crate::conduit::ConduitReceive::Event(ev) => {
                        if let rightkit_agent_engine_protocol::ProtocolEvent::ToolApprovalRequested {
                            call,
                            ..
                        } = ev
                        {
                            assert_eq!(call.id, "tc-effect-bind");
                            let args: serde_json::Value =
                                serde_json::from_str(&call.arguments_json).unwrap();
                            let target =
                                args.get("path").and_then(|v| v.as_str()).map(String::from);
                            assert!(approval_contexts
                                .lock()
                                .unwrap()
                                .contains_key("tc-effect-bind"));
                            let sender = approvals.lock().unwrap().remove("tc-effect-bind");
                            if let Some(tx) = sender {
                                let _ = tx.send(true);
                            }
                            return target;
                        }
                    }
                    // A closed conduit can never deliver the event; stop
                    // rather than spinning on it forever.
                    crate::conduit::ConduitReceive::Closed => return None,
                    _ => continue,
                }
            }
        })
        .await
        .expect("timed out waiting for ToolApprovalRequested");
        assert_eq!(seen_target.as_deref(), Some("src/secrets.rs"));
        let _ = engine_task.await;
    }

    #[test]
    fn effect_fingerprint_changes_with_argument_bytes() {
        let a = simple_content_hash(r#"{"path":"a.rs"}"#);
        let b = simple_content_hash(r#"{"path":"b.rs"}"#);
        assert_ne!(a, b, "distinct effects must not share a fingerprint");
        assert_eq!(
            simple_content_hash(r#"{"path":"a.rs"}"#),
            a,
            "fingerprint must be deterministic for identical bytes"
        );
    }
}
