use super::*;

/// How long a tool may wait for a human approval before it is denied.
///
/// An interactive user answers well inside this; a sub-agent — whose approval
/// events reach no UI — would otherwise wait forever and wedge the whole
/// conductor run.
const APPROVAL_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

impl SessionEngine {
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
                    && self.approved_effect_digests.lock().unwrap().get(&call.id) == Some(&digest)
                {
                    return Some(true);
                }
                // EFF-002: a session grant covers only a byte-identical final
                // payload. Verdict is Ask here, so deny rules already won.
                if guardrail.is_none() && middleware_approval.is_none() {
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
                // Bounded wait. This used to be a bare `rx.await`, which never
                // returns when nothing can answer the request — exactly the
                // case for a sub-agent, whose approval events are published on
                // its own conduit that no UI subscribes to. A conductor run
                // therefore hung on its first `shell` call forever, the
                // orchestrator timed out after five minutes and spawned a
                // replacement, and that repeated without bound.
                //
                // Timing out DENIES (fail closed) and the agent receives a
                // normal denial it can react to, instead of stalling silently.
                let approved = match tokio::time::timeout(APPROVAL_WAIT_TIMEOUT, rx).await {
                    Ok(result) => result.unwrap_or(false),
                    Err(_) => {
                        tracing::warn!(
                            session_id = %sid,
                            tool = %call.name,
                            timeout_secs = APPROVAL_WAIT_TIMEOUT.as_secs(),
                            "tool approval timed out with no responder; denying"
                        );
                        self.pending_approvals.lock().unwrap().remove(&call.id);
                        self.pending_approval_contexts
                            .lock()
                            .unwrap()
                            .remove(&call.id);
                        false
                    }
                };
                self.pending_approval_contexts
                    .lock()
                    .unwrap()
                    .remove(&call.id);

                let call_id = call.id.clone();
                self.emit(|seq| ProtocolEvent::ToolApprovalResolved {
                    session_id: sid.to_string(),
                    seq,
                    created_at: now_iso(),
                    tool_call_id: call_id,
                    approved,
                });

                if !approved {
                    self.clear_unapproved_request(&call.id);
                    let summary = "denied by user".to_string();
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
                // by "Allow for this session") or an auto-approval allow is a
                // positive authorization for this exact call. The tool's own
                // guard re-evaluates against the ambient mode (Ask), so without
                // elevating here the granted repeat would be refused with
                // "Tool requires approval" despite the rule. The verdict already
                // passed durable-write provenance enforcement above.
                let authorized = verdict.policy_source.starts_with("rule:")
                    || verdict.policy_source.starts_with("auto_approval:");
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
