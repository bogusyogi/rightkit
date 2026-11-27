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

/// What the model can do instead, for the refusals that are routine in an unattended run. On the
/// task-set baseline of build 92 the bare refusal was repeated: 71 shell and 26 test-file refusals
/// in 50 runs, with no word on what would be accepted.
fn unattended_hint(policy_source: &str) -> &'static str {
    match policy_source {
        "shell-command:posix" => ". Unattended, the shell runs routine programs (interpreters, test runners, checkers, file and text tools) with variables, pipes, loops and redirects. What needs approval: deleting (rm), git stash, a program that is not routine, running in the background (&), or a variable used as the program name. Use the file tools to delete or rewrite files",
        "destructive:escalated" => ". A deletion needs approval: leave temporary files in place, or delete a workspace file with the file tools",
        "test-file:protected" => ". Changing or removing existing text in a test file needs approval. Adding is allowed: make an edit whose new_string keeps the whole old_string and adds to it, or put new tests in a new file",
        _ => "",
    }
}

/// The call a permission verdict is resolved for, with what the dispatch already learned about it.
pub(in crate::engine) struct PermissionRequest<'a> {
    /// The session the call belongs to.
    pub sid: &'a str,
    /// The call, its arguments already normalised.
    pub call: &'a ToolCall,
    /// The span recorded before the call, when one was.
    pub pre_span_id: Option<&'a str>,
    /// The shell or tool guardrail's decision for the call, when one applies.
    pub guardrail: Option<&'a ToolGuardrailDecision>,
    /// A runtime middleware decision that asks for approval, when one applies.
    pub middleware_approval: Option<&'a AppliedMiddlewareDecision>,
    /// Every middleware decision made for the call.
    pub middleware_decisions: &'a [AppliedMiddlewareDecision],
    /// The argument bytes the provider sent, kept on the stored approval fact when they parse to
    /// the same value as the normalised call.
    pub original_arguments_json: Option<&'a str>,
}

impl SessionEngine {
    /// The facts an approval card shows for `call`: what it changes, whether it leaves the
    /// sandbox and whether it can be undone. Derived from the tool's declared capability, the
    /// call arguments and the sandbox tier the call would run under (`approval_effects.rs`).
    fn approval_effects_for(
        &self,
        call: &ToolCall,
        args: &serde_json::Value,
    ) -> rightkit_agent_engine_protocol::ApprovalEffects {
        // Issue 437: the card claims "the network is off" only where an OS backend enforces the
        // tier (Linux seccomp, macOS Seatbelt, Windows AppContainer). Elsewhere the shell runs
        // unconfined, so the card must not claim isolation that is not there.
        #[cfg(all(
            feature = "host",
            any(target_os = "linux", target_os = "macos", target_os = "windows")
        ))]
        let sandbox: Option<crate::approval_effects::SandboxFact> = Some(
            rightkit_agent_coding_tools::shell::effective_sandbox_tier(
                &self.workspace_root,
                Some(self.permission_mode),
            )
            .into(),
        );
        #[cfg(all(
            feature = "host",
            not(any(target_os = "linux", target_os = "macos", target_os = "windows"))
        ))]
        let sandbox: Option<crate::approval_effects::SandboxFact> = None;
        #[cfg(not(feature = "host"))]
        let sandbox: Option<crate::approval_effects::SandboxFact> = None;
        #[cfg(feature = "host")]
        let sandbox_grant = self.sandbox_retry_grants.contains_key(&call.id);
        #[cfg(not(feature = "host"))]
        let sandbox_grant = false;
        crate::approval_effects::approval_effects(&crate::approval_effects::EffectInputs {
            tool_name: &call.name,
            capability: self.capability_for_call(call),
            args,
            workspace_root: &self.workspace_root,
            sandbox,
            sandbox_grant,
        })
    }

    /// Store one policy fact about a tool call that is not an approval card (tools rule 31):
    /// `kind` is `grant_installed`, `hook_block`, `sandbox_refusal`, `asked`, `approved` or `denied`. `cause` must be a short
    /// reason without command text, arguments or secrets.
    pub(in crate::engine) fn emit_tool_policy_event(
        &mut self,
        sid: &str,
        tool_call_id: &str,
        policy_source: &str,
        kind: &str,
        cause: &str,
    ) {
        // A rule source names its layer, never its command pattern.
        let policy_source = if policy_source.starts_with("rule:") {
            policy_source
                .splitn(3, ':')
                .take(2)
                .collect::<Vec<_>>()
                .join(":")
        } else {
            policy_source.to_string()
        };
        self.emit(|seq| ProtocolEvent::ToolPolicyEvent {
            session_id: sid.to_string(),
            seq,
            created_at: now_iso(),
            tool_call_id: tool_call_id.to_string(),
            policy_source,
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
        result: &rightkit_agent_coding_tools::ToolResult,
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
                rightkit_agent_coding_tools::sandbox::SandboxDenialKind::Network => {
                    "outbound network"
                }
                rightkit_agent_coding_tools::sandbox::SandboxDenialKind::Filesystem { .. } => {
                    "filesystem access"
                }
            };
            let cause = format!("the sandbox blocked {what} (tier {:?})", denial.tier);
            self.emit_tool_policy_event(sid, call_id, &denial.rule_id, "sandbox_refusal", &cause);
        }
    }

    pub(in crate::engine) async fn resolve_single_tool_permission(
        &mut self,
        request: PermissionRequest<'_>,
        turn_results: &mut std::collections::HashMap<
            String,
            rightkit_agent_coding_tools::ToolResult,
        >,
    ) -> Option<bool> {
        let PermissionRequest {
            sid,
            call,
            pre_span_id,
            guardrail,
            middleware_approval,
            middleware_decisions,
            original_arguments_json,
        } = request;
        // The provider saw these exact argument bytes: when they parse to the same value as the
        // normalised call, the stored approval fact keeps them so a resumed request repeats them.
        let stored_call = {
            let mut stored = call.clone();
            if let Some(original) = original_arguments_json {
                let same = matches!(
                    (
                        serde_json::from_str::<serde_json::Value>(original),
                        serde_json::from_str::<serde_json::Value>(&call.arguments_json),
                    ),
                    (Ok(a), Ok(b)) if a == b
                );
                if same {
                    stored.arguments_json = original.to_string();
                }
            }
            stored
        };
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
                crate::tool_policy::run_permission_denied_hooks(
                    self.hook_bus.clone(),
                    sid,
                    call,
                    &summary,
                )
                .await;
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
                let req = stored_call.clone();
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
                let effects = self.approval_effects_for(call, &approved_args);
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
                        // A hook's own reason (for a `defer` decision: "deferred by PreToolUse hook: <reason>")
                        // is part of the cause: the model is told why the call was held.
                        let hook_reason =
                            if psource.starts_with("hook.") && !verdict.reason.trim().is_empty() {
                                format!("; {}", verdict.reason.trim())
                            } else {
                                String::new()
                            };
                        format!(
                            "{APPROVAL_UNATTENDED_SUMMARY} (policy source: {psource}){hook_reason}{}",
                            unattended_hint(&psource)
                        )
                    };
                    // Rule 31: the request and its resolution are stored events even when
                    // nothing could answer, naming the call id and the policy source; the
                    // resolution carries the cause.
                    self.emit(|seq| ProtocolEvent::ToolApprovalRequested {
                        session_id: sid.to_string(),
                        seq,
                        created_at: now_iso(),
                        call: stored_call.clone(),
                        risk_label: "needs-approval".into(),
                        proposed_change: proposed_change.clone(),
                        policy_source: Some(psource.clone()),
                        middleware_decisions: self.approval_middleware_decisions(
                            call,
                            &verdict,
                            guardrail,
                            middleware_decisions,
                        ),
                        effects: Some(effects.clone()),
                    });
                    // POL-07: the ask and its refusal are stored policy facts as well.
                    self.emit_tool_policy_event(
                        sid,
                        &call.id,
                        &psource,
                        "asked",
                        "an approval card was raised",
                    );
                    self.emit_tool_policy_event(
                        sid,
                        &call.id,
                        &psource,
                        "denied",
                        "unattended: nothing could answer the card",
                    );
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
                    let network = serde_json::from_str::<serde_json::Value>(&call.arguments_json)
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
                        });
                    // The binding shape bounds the grant to the digest of the exact final
                    // payload and to what the call reaches (its network host, else the tool); the
                    // flat shape (a host that set `WireShape::Flat`) carries the digest as one
                    // string and leaves the binding empty.
                    let (binding, payload_digest) = if rightkit_agent_engine_protocol::wire_shape()
                        == rightkit_agent_engine_protocol::WireShape::Flat
                    {
                        (Default::default(), Some(approved_digest.clone()))
                    } else {
                        (
                            rightkit_agent_engine_protocol::effect_binding(
                                &approved_digest,
                                network
                                    .as_ref()
                                    .map_or(call.name.as_str(), |n| n.host.as_str()),
                            ),
                            None,
                        )
                    };
                    self.pending_approval_contexts.lock().unwrap().insert(
                        call.id.clone(),
                        rightkit_agent_engine_protocol::PendingApprovalContext {
                            request_id: Some(correlation.clone()),
                            tool_name: call.name.clone(),
                            binding,
                            command: extract_command_arg(&call.arguments_json),
                            payload_digest,
                            effects: Some(effects.clone()),
                            arguments_json: Some(call.arguments_json.clone()),
                            requested_at: Some(now_iso()),
                            reason: Some(verdict.reason.clone()),
                            policy_source: Some(psource.clone()),
                            proposed_change: proposed_change.clone(),
                            network,
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
                    policy_source: Some(psource.clone()),
                    middleware_decisions: self.approval_middleware_decisions(
                        call,
                        &verdict,
                        guardrail,
                        middleware_decisions,
                    ),
                    effects: Some(effects),
                });
                // POL-07: the ask is a stored policy fact (no command text, no arguments).
                self.emit_tool_policy_event(
                    sid,
                    &call.id,
                    &psource,
                    "asked",
                    "an approval card was raised",
                );
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
                    reason: resolved_reason.clone(),
                });
                // POL-07: the answer is a stored policy fact too, with a short cause.
                self.emit_tool_policy_event(
                    sid,
                    &call.id,
                    &psource,
                    if approved { "approved" } else { "denied" },
                    match resolved_reason.as_deref() {
                        None => "the operator approved the card",
                        Some("cancelled") => "the card was cancelled",
                        Some("detached") => "no client was attached to answer the card",
                        _ => "the operator refused the card",
                    },
                );

                if !approved {
                    self.clear_unapproved_request(&call.id);
                    let summary = denial_cause.to_string();
                    crate::tool_policy::run_permission_denied_hooks(
                        self.hook_bus.clone(),
                        sid,
                        call,
                        &summary,
                    )
                    .await;
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
                // A host that sets `abac_allow_authorizes` counts an ABAC Allow as the same positive
                // authorization (policy contract: the first matching rule's effect is returned at
                // once); the clamps that must still ask already turned it into an Ask before this
                // point. Otherwise only a permission rule or grant authorizes.
                let authorized = verdict.policy_source.starts_with("rule:")
                    || (crate::host_options::abac_allow_authorizes()
                        && verdict.policy_source.starts_with("abac:"));
                // An allow that came from an explicit authority (an ABAC rule, a permission
                // rule or grant, the auto-approval seam) is stored, so the record of why a
                // call ran without a card is not only the denials. A rule source names its
                // layer, never its command pattern.
                let source = verdict.policy_source.as_str();
                if source.starts_with("abac:")
                    || source.starts_with("rule:")
                    || source.starts_with("auto_approval:")
                {
                    let public_source = if source.starts_with("rule:") {
                        source.splitn(3, ':').take(2).collect::<Vec<_>>().join(":")
                    } else {
                        source.to_string()
                    };
                    self.emit_tool_policy_event(
                        sid,
                        &call.id,
                        &public_source,
                        "allowed",
                        "allowed without a card by an explicit rule",
                    );
                }
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
