use crate::api::{
    lock_recover, AnswerQuestion, DaemonState, LiveSession, ResolveApproval, ResolvePlan,
};
use crate::api_permissions::enforce_trusted_source_authority;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};

fn notify_compaction_revision(live: &std::sync::Arc<LiveSession>) {
    if let Some(notifier) = &live.compaction_revision_notifier {
        notifier();
    }
}
pub(crate) async fn compact_session(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let nonce = super::idempotency::resolve_nonce(None, &headers)?;
    let digest = super::idempotency::request_digest("compact_session", &serde_json::json!({}));
    let storage = state.storage.clone();
    let scope_id = id.clone();
    super::idempotency::run_idempotent(storage, &scope_id, "compact_session", nonce, digest, || {
        compact_session_once(state, id)
    })
    .await
}

async fn compact_session_once(
    state: DaemonState,
    id: String,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let live = state.get_session(&id).ok_or(StatusCode::NOT_FOUND)?;
    let compact_tx = live.compact_tx.as_ref().ok_or(StatusCode::CONFLICT)?;
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    compact_tx
        .send(rightkit_agent_engine_core::CompactionCommandV1 {
            id: rightkit_agent_engine_core::compactor::next_compaction_command_id(),
            kind: rightkit_agent_engine_core::CompactionCommandKindV1::Manual,
            response: Some(response_tx),
        })
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let outcome = response_rx
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .map_err(|error| {
            tracing::warn!(session_id = %id, error = %error, "manual compaction failed closed");
            StatusCode::BAD_GATEWAY
        })?;
    let compaction_policy = live
        .compaction_policy_tx
        .as_ref()
        .map(|tx| tx.borrow().clone());
    Ok(Json(serde_json::json!({
        "compacted": outcome.compacted,
        "summary": outcome.summary,
        "droppedCount": outcome.dropped_count,
        "archive": outcome.archive.map(|archive| serde_json::json!({
            "id": archive.archive_id,
            "sha256": archive.sha256,
            "byteLength": archive.byte_length,
            "messageCount": archive.message_count,
        })),
        "compactionPolicy": compaction_policy,
    })))
}
pub(crate) async fn resolve_tool_approval(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<ResolveApproval>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let live = state.get_session(&id).ok_or(StatusCode::NOT_FOUND)?;
    let context = lock_recover(
        &live.pending_approval_contexts,
        "session.pending_approval_contexts",
    )
    .get(&body.tool_call_id)
    .cloned();
    if body.decision.is_some() && context.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    if let Some(request_id) = body.request_id.as_deref() {
        if context.as_ref().and_then(|c| c.request_id.as_deref()) != Some(request_id) {
            return Err(StatusCode::NOT_FOUND);
        }
    }
    // Validate identity while the sender is still pending. A malformed
    // response must leave the live request available for a correct retry.
    let sender = {
        let mut pending = lock_recover(&live.pending_approvals, "session.pending_approvals");
        if pending
            .get(&body.tool_call_id)
            .ok_or(StatusCode::NOT_FOUND)?
            .is_closed()
        {
            return Err(StatusCode::NOT_FOUND);
        }
        pending
            .remove(&body.tool_call_id)
            .ok_or(StatusCode::NOT_FOUND)?
    };
    let gate_response = body.decision.as_ref().map(|decision| {
        rightkit_agent_engine_protocol::PermissionGateResponse {
            allow: decision.is_approved(),
            reason: format!("{decision:?}"),
        }
    });
    let mut policy_rule = None;
    let mut payload_grant: Option<String> = None;
    let approved = match body.decision.as_ref() {
        None => body.approved,
        Some(rightkit_agent_engine_protocol::ReviewDecision::Approved) => true,
        Some(rightkit_agent_engine_protocol::ReviewDecision::Abort) => false,
        Some(rightkit_agent_engine_protocol::ReviewDecision::ApprovedExecpolicyAmendment {
            proposed_execpolicy_amendment,
        }) => {
            // Persist Codex's proposed prefix through RightKit's existing
            // session-scoped policy authority before releasing this call.
            let pattern = format!(
                "shell({})",
                proposed_execpolicy_amendment.command().join(" ")
            );
            policy_rule = Some(rightkit_agent_policy::Rule::new(
                pattern,
                rightkit_agent_policy::PermissionDecision::Allow,
                rightkit_agent_policy::RuleSource::Session,
            ));
            gate_response.as_ref().map(|r| r.allow).unwrap_or(true)
        }
        Some(rightkit_agent_engine_protocol::ReviewDecision::ApprovedForSession) => {
            // A call with a command keeps the session pattern grant so
            // "approve for session" stays usable. Only a tool with no command
            // (where a pattern would allow the whole tool) gets an EFF-002
            // grant bound to the digest of the final post-transform payload;
            // with no digest either, nothing is installed (approved once).
            // The pre-execute digest re-check applies to every approval.
            let Some(context) = context.as_ref() else {
                return Err(StatusCode::NOT_FOUND);
            };
            match context.command.as_ref() {
                Some(cmd) => {
                    policy_rule = Some(rightkit_agent_policy::Rule::new(
                        format!("{}({})", context.tool_name, cmd),
                        rightkit_agent_policy::PermissionDecision::Allow,
                        rightkit_agent_policy::RuleSource::Session,
                    ));
                }
                None => match context.payload_digest.as_ref() {
                    Some(digest) => payload_grant = Some(digest.clone()),
                    None => tracing::warn!(
                        "session approval has no command or payload digest; approving once without a grant"
                    ),
                },
            }
            gate_response.as_ref().map(|r| r.allow).unwrap_or(true)
        }
        Some(rightkit_agent_engine_protocol::ReviewDecision::NetworkPolicyAmendment {
            network_policy_amendment,
        }) => {
            // Network policy is carried by the same session RuleStore seam;
            // qualified targets preserve host-specific allow/deny behavior.
            let tool = context
                .as_ref()
                .map(|c| c.tool_name.as_str())
                .unwrap_or("web_fetch");
            let pattern = format!("{}({})", tool, network_policy_amendment.host);
            let effect = match network_policy_amendment.action {
                rightkit_agent_engine_protocol::NetworkPolicyRuleAction::Allow => {
                    rightkit_agent_policy::PermissionDecision::Allow
                }
                rightkit_agent_engine_protocol::NetworkPolicyRuleAction::Deny => {
                    rightkit_agent_policy::PermissionDecision::Deny
                }
            };
            policy_rule = Some(rightkit_agent_policy::Rule::new(
                pattern,
                effect,
                rightkit_agent_policy::RuleSource::Session,
            ));
            true
        }
    };

    // The approver's identity is established by the transport, not per-message
    // signing: the web client sits behind Cloudflare Access (email-gated), and
    // the Telegram bot only accepts messages from its allowed-user allowlist.
    // So a request that reaches this handler is already from the authorized
    // operator — no device pairing or envelope signature is required.
    if let Some(digest) = payload_grant {
        let mut rules = lock_recover(&live.permission_rules, "session.permission_rules");
        if sender.send(approved).is_err() {
            return Err(StatusCode::NOT_FOUND);
        }
        if approved {
            rules.grant_session_payload(&digest);
        }
        drop(rules);
        notify_compaction_revision(&live);
    } else if let Some(rule) = policy_rule {
        let mut rules = lock_recover(&live.permission_rules, "session.permission_rules");
        if sender.send(approved).is_err() {
            return Err(StatusCode::NOT_FOUND);
        }
        // Validate before install: a malformed grant is skipped (this call is
        // still released as approved once) and never enters the store.
        if rules.try_add_all(vec![rule]).is_err() {
            tracing::warn!("session approval rule rejected as malformed; not installed");
        }
        drop(rules);
        notify_compaction_revision(&live);
    } else if sender.send(approved).is_err() {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(Json(
        serde_json::json!({ "resolved": true, "approved": approved }),
    ))
}
pub(crate) async fn answer_question(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<AnswerQuestion>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let live = state.get_session(&id).ok_or(StatusCode::NOT_FOUND)?;
    let sender = lock_recover(&live.pending_questions, "session.pending_questions")
        .remove(&body.question_id);
    match sender {
        Some(tx) => tx
            .send(body.answers)
            .map(|_| Json(serde_json::json!({ "resolved": true })))
            .map_err(|_| StatusCode::NOT_FOUND),
        None => Err(StatusCode::NOT_FOUND),
    }
}
pub(crate) async fn resolve_plan(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<ResolvePlan>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let live = state.get_session(&id).ok_or(StatusCode::NOT_FOUND)?;
    let sender = lock_recover(&live.pending_plans, "session.pending_plans").remove(&body.plan_id);
    match sender {
        Some(tx) => tx
            .send(body.approved)
            .map(|_| Json(serde_json::json!({ "resolved": true, "approved": body.approved })))
            .map_err(|_| StatusCode::NOT_FOUND),
        None => Err(StatusCode::NOT_FOUND),
    }
}

/// Wait cap for one Claude `can_use_tool` prompt; the engine applies the same bound to its own
/// approval wait, so both layers give up together and the request is denied.
const DRIVER_PERMISSION_WAIT: std::time::Duration = std::time::Duration::from_secs(310);

/// Answer a Claude `can_use_tool` request (decision #336) through RightKit's own approval flow.
///
/// The request is sent as a `permission_only` call on the session's governed executor
/// (`LiveSession::driver_tool_tx` -> `SessionEngine::authorize_external_tool_call`), so it raises
/// the same approval card and honours the same session grants (#181 digest and pattern grants) as
/// a native engine call. Returns the answer in the `permission_prompt` shape
/// (`{"behavior":"allow","updatedInput":...}` or `{"behavior":"deny","message":...}`), which
/// `DriverHostRuntime::respond_server_request` turns into the `control_response`. Never allows by
/// default: unknown tools, malformed input, an unavailable executor, a dropped answer or a
/// timeout all deny. Dropping the receiver on timeout makes the engine revoke the pending request.
pub(crate) async fn answer_claude_can_use_tool(
    state: &DaemonState,
    session_id: &str,
    params: &serde_json::Value,
) -> serde_json::Value {
    let deny = |message: &str| serde_json::json!({"behavior": "deny", "message": message});
    let Some(live) = state.get_session(session_id) else {
        return deny("Session is no longer live");
    };
    let Some(tx) = live.driver_tool_tx.as_ref() else {
        return deny("Governed session executor is unavailable");
    };
    let input = params
        .get("input")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let name = params
        .get("tool_name")
        .and_then(serde_json::Value::as_str)
        .and_then(|name| name.strip_prefix("mcp__coderight__"))
        .filter(|name| !name.is_empty() && *name != "permission_prompt");
    let Some(name) = name else {
        return deny("Only RightKit tools are permitted");
    };
    if !input.is_object() {
        return deny("Invalid tool input");
    }
    let tool_use_id = params
        .get("tool_use_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(rightkit_agent_engine_protocol::uuid_v7);
    let (response, receiver) = tokio::sync::oneshot::channel();
    let request = crate::api::DriverToolRequest {
        permission_only: true,
        call: rightkit_agent_engine_protocol::ToolCall {
            // Same id shape as the capability gateway so a later governed call for this tool use
            // can reuse the digest-bound consent.
            id: format!("driver:{session_id}:{tool_use_id}"),
            name: name.to_string(),
            arguments_json: input.to_string(),
        },
        response,
    };
    if tx.send(request).await.is_err() {
        return deny("Governed session executor stopped");
    }
    match tokio::time::timeout(DRIVER_PERMISSION_WAIT, receiver).await {
        Ok(Ok(result)) if !result.is_error => {
            serde_json::json!({"behavior": "allow", "updatedInput": input})
        }
        Ok(Ok(result)) => deny(&result.content),
        Ok(Err(_)) => deny("Governed session executor dropped its answer"),
        Err(_) => deny("Approval timed out; denied by RightKit"),
    }
}
