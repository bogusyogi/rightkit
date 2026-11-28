use crate::api::{
    lock_recover, AnswerQuestion, DaemonState, LiveSession, ResolveApproval, ResolvePlan,
};
use crate::api_permissions::enforce_trusted_source_authority;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};

/// Failure of `POST /sessions/{id}/compact`. A bare status keeps the router's generic envelope;
/// a compaction failure carries the engine's reason so the client sees why instead of "Bad Gateway".
/// `Display` keeps the in-process `/compact` command caller (`api_commands.rs`) formatting `{status}`.
pub(crate) struct CompactError {
    status: StatusCode,
    detail: Option<String>,
}

impl From<StatusCode> for CompactError {
    fn from(status: StatusCode) -> Self {
        Self {
            status,
            detail: None,
        }
    }
}

impl std::fmt::Display for CompactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.detail {
            Some(detail) => write!(f, "{}: {detail}", self.status),
            None => write!(f, "{}", self.status),
        }
    }
}

impl IntoResponse for CompactError {
    fn into_response(self) -> Response {
        let Some(detail) = self.detail else {
            return self.status.into_response();
        };
        // Same field set as the router's `error.v1` envelope for a 5xx mutation.
        (
            self.status,
            Json(serde_json::json!({
                "schemaVersion": crate::host_identity::schema("error.v1"),
                "code": "compaction_failed",
                "detail": detail,
                "correlationId": rightkit_agent_engine_protocol::uuid_v7(),
                "retryable": true,
                "effectState": "unknown",
                "remediation": serde_json::Value::Null,
            })),
        )
            .into_response()
    }
}

fn notify_compaction_revision(live: &std::sync::Arc<LiveSession>) {
    if let Some(notifier) = &live.compaction_revision_notifier {
        notifier();
    }
}
/// `POST /sessions/{id}/compact`. The body is optional: `{"instructions": "<focus text>"}` steers this one
/// summarization (the `/compact <instructions>` command); an empty body or `{}` is the plain compaction.
pub(crate) async fn compact_session(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, CompactError> {
    let instructions = parse_compact_instructions(&body)?;
    compact_session_with_instructions(state, id, headers, instructions).await
}

/// Parse the optional compact body. Blank text counts as no instructions; anything but an object with an
/// optional string `instructions` is a 400 (never silently dropped).
fn parse_compact_instructions(body: &[u8]) -> Result<Option<String>, StatusCode> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| StatusCode::BAD_REQUEST)?;
    let object = value.as_object().ok_or(StatusCode::BAD_REQUEST)?;
    if object.keys().any(|key| key != "instructions") {
        return Err(StatusCode::BAD_REQUEST);
    }
    match object.get("instructions") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(text)) => {
            Ok(Some(text.trim().to_string()).filter(|text| !text.is_empty()))
        }
        Some(_) => Err(StatusCode::BAD_REQUEST),
    }
}

/// The compaction entry shared by the route and the `/compact` slash command.
pub(crate) async fn compact_session_with_instructions(
    state: DaemonState,
    id: String,
    headers: HeaderMap,
    instructions: Option<String>,
) -> Result<Json<serde_json::Value>, CompactError> {
    enforce_trusted_source_authority(&headers)?;
    let nonce = super::idempotency::resolve_nonce(None, &headers)?;
    let digest = super::idempotency::request_digest(
        "compact_session",
        &match &instructions {
            Some(text) => serde_json::json!({ "instructions": text }),
            None => serde_json::json!({}),
        },
    );
    let storage = state.storage.clone();
    let scope_id = id.clone();
    // `run_idempotent` only carries a status; the failure reason travels in this side slot.
    let failure = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let slot = failure.clone();
    super::idempotency::run_idempotent(storage, &scope_id, "compact_session", nonce, digest, || {
        compact_session_once(state, id, instructions, slot)
    })
    .await
    .map_err(|status| CompactError {
        status,
        detail: if status == StatusCode::BAD_GATEWAY {
            failure.lock().ok().and_then(|mut slot| slot.take())
        } else {
            None
        },
    })
}

async fn compact_session_once(
    state: DaemonState,
    id: String,
    instructions: Option<String>,
    failure: std::sync::Arc<std::sync::Mutex<Option<String>>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let live = state.get_session(&id).ok_or(StatusCode::NOT_FOUND)?;
    let compact_tx = live.compact_tx.as_ref().ok_or(StatusCode::CONFLICT)?;
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    compact_tx
        .send(rightkit_agent_engine_core::CompactionCommandV1 {
            id: rightkit_agent_engine_core::compactor::next_compaction_command_id(),
            kind: rightkit_agent_engine_core::CompactionCommandKindV1::Manual,
            instructions,
            response: Some(response_tx),
        })
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let outcome = match response_rx
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
    {
        Ok(outcome) => outcome,
        Err(error) if error.starts_with("compaction made no gain") => {
            // A chat too small to shrink is not a gateway failure: the summary would have been
            // larger than the transcript, so the engine kept the transcript. Say so, like the
            // "nothing eligible" case, instead of an opaque 502.
            tracing::info!(session_id = %id, error = %error, "manual compaction skipped: no gain");
            return Ok(Json(serde_json::json!({
                "compacted": false,
                "summary": "",
                "droppedCount": 0,
                "archive": null,
                "reason": error,
                "compactionPolicy": live
                    .compaction_policy_tx
                    .as_ref()
                    .map(|tx| tx.borrow().clone()),
            })));
        }
        Err(error) => {
            tracing::warn!(session_id = %id, error = %error, "manual compaction failed closed");
            if let Ok(mut slot) = failure.lock() {
                *slot = Some(error);
            }
            return Err(StatusCode::BAD_GATEWAY);
        }
    };
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
    guest: Option<axum::Extension<crate::guest_shares::GuestContext>>,
    Json(body): Json<ResolveApproval>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let mut body = body;
    // `remember` is one field for the three scopes the terminal has: this call only (absent),
    // this chat ("session", the same grant as the structured decision) and this workspace
    // ("project", the standing rule the terminal's "always" installs). An unknown value is
    // refused before the pending sender is taken, so the call stays open.
    let remember = match body.remember.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(scope) => match scope.to_ascii_lowercase().as_str() {
            "session" => Some(RememberScope::Session),
            "project" => Some(RememberScope::Project),
            _ => return Err(StatusCode::BAD_REQUEST),
        },
    };
    if remember == Some(RememberScope::Session) && body.decision.is_none() && body.approved {
        body.decision = Some(rightkit_agent_engine_protocol::ReviewDecision::ApprovedForSession);
    }
    // A guest link may approve or refuse a pending call, nothing else: a decision that installs
    // session policy (an approve-for-session grant, a shell prefix grant or a host rule taken
    // from the body) is owner-only. Refused before the pending sender is taken, so the call
    // stays open for the owner.
    if guest.is_some() && remember.is_some() {
        return Err(StatusCode::FORBIDDEN);
    }
    if guest.is_some()
        && matches!(
            body.decision,
            Some(rightkit_agent_engine_protocol::ReviewDecision::ApprovedForSession)
                | Some(
                    rightkit_agent_engine_protocol::ReviewDecision::ApprovedExecpolicyAmendment { .. }
                )
                | Some(
                    rightkit_agent_engine_protocol::ReviewDecision::NetworkPolicyAmendment { .. }
                )
        )
    {
        return Err(StatusCode::FORBIDDEN);
    }
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
    // An execpolicy amendment installs a standing shell grant, so it must name the pending
    // call's own command or a leading part of it. Refused before the sender is taken, so the
    // call stays open and a grant is never stored for a command that never reached a card.
    if let Some(rightkit_agent_engine_protocol::ReviewDecision::ApprovedExecpolicyAmendment {
        proposed_execpolicy_amendment,
    }) = body.decision.as_ref()
    {
        let amended = proposed_execpolicy_amendment.command().join(" ");
        let amended: Vec<&str> = amended.split_whitespace().collect();
        let command = context
            .as_ref()
            .and_then(|c| c.command.as_deref())
            .unwrap_or("");
        let command: Vec<&str> = command.split_whitespace().collect();
        if amended.is_empty() || !command.starts_with(&amended) {
            return Err(StatusCode::BAD_REQUEST);
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
    // "project": work out the standing rule now, while the context is at hand. A call that cannot
    // be covered by a safe rule is still allowed this once, and the answer says why.
    let mut project_patterns: Vec<String> = Vec::new();
    let mut project_rule: Option<ProjectAllowRule> = None;
    let mut not_remembered: Option<String> = None;
    if remember == Some(RememberScope::Project) && body.decision.is_none() && body.approved {
        match context.as_ref() {
            Some(context) => {
                match project_allow_rule(
                    &context.tool_name,
                    context.command.as_deref(),
                    context.policy_source.as_deref(),
                ) {
                    Ok(rule) => {
                        project_patterns = rule.patterns.clone();
                        project_rule = Some(rule);
                    }
                    Err(reason) => not_remembered = Some(reason),
                }
            }
            None => not_remembered = Some("This request is no longer open.".to_string()),
        }
    }
    let mut policy_rule = None;
    let mut payload_grant: Option<String> = None;
    // Tools rule 31: what a grant install is stored as. Names the kind of grant, never the
    // command text or the payload.
    let mut grant_source = "session_grant";
    let approved = match body.decision.as_ref() {
        None => body.approved,
        Some(rightkit_agent_engine_protocol::ReviewDecision::Approved) => true,
        Some(rightkit_agent_engine_protocol::ReviewDecision::Abort) => false,
        Some(rightkit_agent_engine_protocol::ReviewDecision::ApprovedExecpolicyAmendment {
            proposed_execpolicy_amendment,
        }) => {
            // Persist Codex's proposed prefix through the host's existing
            // session-scoped policy authority before releasing this call.
            grant_source = "execpolicy_amendment";
            policy_rule = Some(rightkit_agent_policy::Rule::session_command_grant(
                "shell",
                &proposed_execpolicy_amendment.command().join(" "),
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
                    // Literal, never a wildcard: `*` in the approved command is an ordinary
                    // character, and a grant for one command never allows a chained one.
                    grant_source = "session_command_grant";
                    policy_rule = Some(rightkit_agent_policy::Rule::session_command_grant(
                        &context.tool_name,
                        cmd,
                    ));
                }
                None => {
                    // The digest is the core binding's, or the flat field of a record that has no
                    // binding; with neither, nothing is installed (approved once).
                    let has_digest = !context.binding.args.is_empty()
                        || context
                            .payload_digest
                            .as_deref()
                            .is_some_and(|digest| !digest.is_empty());
                    if has_digest {
                        grant_source = "session_payload_grant";
                        payload_grant = Some(context.payload_digest());
                    } else {
                        tracing::warn!(
                            "session approval has no command or payload digest; approving once without a grant"
                        );
                    }
                }
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
            grant_source = "network_policy_amendment";
            policy_rule = Some(rightkit_agent_policy::Rule::new(
                pattern,
                effect,
                rightkit_agent_policy::RuleSource::Session,
            ));
            // A "deny this host" decision installs the Deny rule and refuses this call too.
            effect == rightkit_agent_policy::PermissionDecision::Allow
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
        if approved {
            emit_grant_installed(&live, &body.tool_call_id, grant_source);
        }
        notify_compaction_revision(&live);
    } else if !project_patterns.is_empty() {
        // Parsed in the live store first (the same Project source the daemon seeds at session
        // start), so the next identical call does not ask again; written to the rules file after.
        let mut rules = lock_recover(&live.permission_rules, "session.permission_rules");
        if sender.send(approved).is_err() {
            return Err(StatusCode::NOT_FOUND);
        }
        let installed = rules
            .try_add_all(
                project_patterns
                    .iter()
                    .map(|pattern| {
                        rightkit_agent_policy::Rule::new(
                            pattern.clone(),
                            rightkit_agent_policy::PermissionDecision::Allow,
                            rightkit_agent_policy::RuleSource::Project,
                        )
                    })
                    .collect(),
            )
            .is_ok();
        drop(rules);
        if !installed {
            not_remembered =
                Some("The rule could not be added; this call was allowed once.".into());
            project_rule = None;
        }
        notify_compaction_revision(&live);
    } else if let Some(rule) = policy_rule {
        let mut rules = lock_recover(&live.permission_rules, "session.permission_rules");
        if sender.send(approved).is_err() {
            return Err(StatusCode::NOT_FOUND);
        }
        // Validate before install: a malformed grant is skipped (this call is
        // still released as approved once) and never enters the store.
        let installed = rules.try_add_all(vec![rule]).is_ok();
        if !installed {
            tracing::warn!("session approval rule rejected as malformed; not installed");
        }
        drop(rules);
        if installed {
            emit_grant_installed(&live, &body.tool_call_id, grant_source);
        }
        notify_compaction_revision(&live);
    } else if sender.send(approved).is_err() {
        return Err(StatusCode::NOT_FOUND);
    }
    let mut response = serde_json::json!({ "resolved": true, "approved": approved, "ok": true });
    let remembered = match (remember, approved) {
        (Some(RememberScope::Session), true) => {
            Some(session_remembered(context.as_ref(), grant_source))
        }
        (Some(RememberScope::Project), true) => match project_rule {
            Some(rule) => persist_project_rule(&state, &headers, &live.workspace_root, rule)
                .await
                .map_err(|reason| not_remembered = Some(reason))
                .ok(),
            None => None,
        },
        _ => None,
    };
    if remember.is_some() {
        response["remembered"] = remembered.unwrap_or(serde_json::Value::Null);
        if let Some(reason) = not_remembered {
            response["notRemembered"] = serde_json::Value::String(reason);
        } else if !approved {
            response["notRemembered"] =
                serde_json::Value::String("A refused call is never remembered.".into());
        }
    }
    Ok(Json(response))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RememberScope {
    Session,
    Project,
}

/// A standing allow rule for one workspace: the patterns stored (each with effect allow) and the
/// plain sentence that says what they cover.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectAllowRule {
    patterns: Vec<String>,
    description: String,
}

/// The standing rule "always allow here" may install for a call, or the plain reason it may not.
/// Only a plain shell command qualifies, and only when it was held by an ordinary ask (not by a
/// deny, an ask rule or a policy); the same words the terminal's "always" uses
/// (`rightkit_terminal_ui::render::allow_rule`), so the two never disagree on what is safe.
fn project_allow_rule(
    tool_name: &str,
    command: Option<&str>,
    policy_source: Option<&str>,
) -> Result<ProjectAllowRule, String> {
    const ONCE: &str = "this call was allowed once.";
    if tool_name != "shell" {
        return Err(format!(
            "Only shell commands can be allowed for the whole project; {ONCE}"
        ));
    }
    match policy_source.map(str::trim) {
        None | Some("") => {}
        Some(source) if source.starts_with("mode:") => {}
        Some(_) => {
            return Err(format!(
                "A rule or policy held this call, so it is not widened; {ONCE}"
            ))
        }
    }
    let command = command.ok_or_else(|| format!("This call has no command; {ONCE}"))?;
    let words =
        rightkit_terminal_ui::render::allow_rule::shell_prefix_words(command).ok_or_else(|| {
            format!("This command is too broad or too risky for a standing rule; {ONCE}")
        })?;
    let prefix = words.join(" ");
    Ok(ProjectAllowRule {
        patterns: vec![format!("shell({prefix})"), format!("shell({prefix} *)")],
        description: format!("Always allow `{prefix}` commands in this project"),
    })
}

/// Write a project rule to the rules file through the same handler the rules route uses, so
/// the file, its checks and its listing are one code path. Returns what was stored.
async fn persist_project_rule(
    state: &DaemonState,
    headers: &HeaderMap,
    workspace_root: &str,
    rule: ProjectAllowRule,
) -> Result<serde_json::Value, String> {
    let mut stored = Vec::new();
    for pattern in &rule.patterns {
        let Json(added) = crate::api_permissions::add_persistent_permission_rule_handler(
            State(state.clone()),
            headers.clone(),
            Json(crate::api_permissions::AddPersistentPermissionRule {
                scope: "project".to_string(),
                pattern: pattern.clone(),
                effect: "allow".to_string(),
                workspace_root: Some(workspace_root.to_string()),
            }),
        )
        .await
        .map_err(|status| {
            format!("The rule could not be saved (HTTP {status}); this call was allowed once.")
        })?;
        stored.push(
            added
                .get("rule")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        );
    }
    let broad = rule.patterns.last().cloned().unwrap_or_default();
    Ok(serde_json::json!({
        "scope": "project",
        "rule": broad,
        "patterns": rule.patterns,
        "description": rule.description,
        "stored": stored,
    }))
}

/// What a "session" answer remembered, in plain words, from the grant that was installed.
fn session_remembered(
    context: Option<&rightkit_agent_engine_protocol::PendingApprovalContext>,
    grant_source: &str,
) -> serde_json::Value {
    let (rule, description) = match (context, grant_source) {
        (Some(c), "session_command_grant") => {
            let command = c.command.clone().unwrap_or_default();
            (
                serde_json::Value::String(format!("{}({command})", c.tool_name)),
                format!("Allowed for the rest of this chat: `{command}`"),
            )
        }
        (Some(c), "session_payload_grant") => (
            serde_json::Value::Null,
            format!(
                "Allowed for the rest of this chat: this exact `{}` call",
                c.tool_name
            ),
        ),
        _ => (
            serde_json::Value::Null,
            "Allowed this once; nothing could be kept for the chat".to_string(),
        ),
    };
    serde_json::json!({ "scope": "session", "rule": rule, "description": description })
}

/// What an open `ask_user` question looked like when it was raised: the engine keeps only the
/// answer channel (`LiveSession::pending_questions`), so the question itself is kept here, beside
/// it, and dropped when the wait ends. Keyed by session id and question id.
type QuestionPayloads = std::collections::HashMap<(String, String), (String, serde_json::Value)>;
static PENDING_QUESTION_PAYLOADS: std::sync::OnceLock<std::sync::Mutex<QuestionPayloads>> =
    std::sync::OnceLock::new();

fn question_payloads() -> std::sync::MutexGuard<'static, QuestionPayloads> {
    PENDING_QUESTION_PAYLOADS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn note_pending_question(
    session_id: &str,
    question_id: &str,
    questions: &[rightkit_agent_engine_protocol::UserQuestion],
) {
    question_payloads().insert(
        (session_id.to_string(), question_id.to_string()),
        (
            crate::api::now_iso(),
            serde_json::to_value(questions).unwrap_or(serde_json::Value::Null),
        ),
    );
}

pub(crate) fn forget_pending_question(session_id: &str, question_id: &str) {
    question_payloads().remove(&(session_id.to_string(), question_id.to_string()));
}

/// The approval cards of one session as the live event drew them, oldest first.
fn approval_rows(
    contexts: &std::collections::HashMap<
        String,
        rightkit_agent_engine_protocol::PendingApprovalContext,
    >,
    open: &std::collections::HashSet<String>,
) -> Vec<serde_json::Value> {
    let mut rows: Vec<(Option<String>, String, serde_json::Value)> = contexts
        .iter()
        .filter(|(id, _)| open.contains(*id))
        .map(|(id, c)| {
            let arguments = c
                .arguments_json
                .as_deref()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok());
            (
                c.requested_at.clone(),
                id.clone(),
                serde_json::json!({
                    "toolCallId": id,
                    "toolName": c.tool_name,
                    "command": c.command,
                    "arguments": arguments,
                    "requestedAt": c.requested_at,
                    "reason": c.reason,
                    "policySource": c.policy_source,
                    "proposedChange": c.proposed_change,
                    "effects": c.effects,
                    "network": c.network,
                }),
            )
        })
        .collect();
    rows.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    rows.into_iter().map(|(_, _, row)| row).collect()
}

/// `GET /sessions/{id}/pending`: what a live session is waiting on, so a client that reloaded can
/// draw the cards again. A session that is not live is 404; nothing pending is empty lists.
pub(crate) async fn list_pending(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let live = state.get_session(&id).ok_or(StatusCode::NOT_FOUND)?;
    let open: std::collections::HashSet<String> =
        lock_recover(&live.pending_approvals, "session.pending_approvals")
            .iter()
            .filter(|(_, sender)| !sender.is_closed())
            .map(|(call_id, _)| call_id.clone())
            .collect();
    let approvals = approval_rows(
        &lock_recover(
            &live.pending_approval_contexts,
            "session.pending_approval_contexts",
        ),
        &open,
    );
    let question_ids: Vec<String> =
        lock_recover(&live.pending_questions, "session.pending_questions")
            .iter()
            .filter(|(_, sender)| !sender.is_closed())
            .map(|(question_id, _)| question_id.clone())
            .collect();
    let mut questions: Vec<(String, serde_json::Value)> = {
        let payloads = question_payloads();
        question_ids
            .iter()
            .map(|question_id| {
                let kept = payloads.get(&(id.clone(), question_id.clone()));
                (
                    kept.map(|(at, _)| at.clone()).unwrap_or_default(),
                    serde_json::json!({
                        "questionId": question_id,
                        "requestedAt": kept.map(|(at, _)| at.clone()),
                        "questions": kept.map(|(_, q)| q.clone()),
                    }),
                )
            })
            .collect()
    };
    questions.sort_by(|a, b| a.0.cmp(&b.0));
    let mut plans: Vec<String> = lock_recover(&live.pending_plans, "session.pending_plans")
        .iter()
        .filter(|(_, sender)| !sender.is_closed())
        .map(|(plan_id, _)| plan_id.clone())
        .collect();
    plans.sort();
    let mut elicitations: Vec<String> =
        lock_recover(&live.pending_elicitations, "session.pending_elicitations")
            .keys()
            .cloned()
            .collect();
    elicitations.sort();
    Ok(Json(serde_json::json!({
        "approvals": approvals,
        "questions": questions.into_iter().map(|(_, q)| q).collect::<Vec<_>>(),
        "plans": plans,
        "elicitations": elicitations,
    })))
}

/// Tools rule 31: store that a session grant was installed for this call's approval. The event
/// names the call and the kind of grant; the cause never repeats the command or the payload.
fn emit_grant_installed(live: &std::sync::Arc<LiveSession>, tool_call_id: &str, source: &str) {
    let session_id = live.id.clone();
    let call_id = tool_call_id.to_string();
    let source = source.to_string();
    live.conduit.emit(
        |seq| rightkit_agent_engine_protocol::ProtocolEvent::ToolPolicyEvent {
            session_id,
            seq,
            created_at: crate::api::now_iso(),
            tool_call_id: call_id,
            policy_source: source,
            kind: "grant_installed".to_string(),
            cause: "the operator approved this call for the session".to_string(),
        },
    );
}
/// `true` when `answers` is a blank string or an object any of whose values is a blank string.
fn answers_has_blank_text(answers: &serde_json::Value) -> bool {
    match answers {
        serde_json::Value::String(text) => text.trim().is_empty(),
        serde_json::Value::Object(map) => map
            .values()
            .any(|value| value.as_str().is_some_and(|text| text.trim().is_empty())),
        _ => false,
    }
}

pub(crate) async fn answer_question(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<AnswerQuestion>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let live = state.get_session(&id).ok_or(StatusCode::NOT_FOUND)?;
    // A blank typed answer is not an answer: refuse it before the waiting sender is taken, so the
    // question stays pending and a real answer can still arrive (an empty `answers` list, which
    // client-api R36 reads as an empty answer, and a decline marker are not affected).
    if answers_has_blank_text(&body.answers) {
        return Err(StatusCode::BAD_REQUEST);
    }
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
        Some(tx) => {
            let outcome =
                crate::plan_exit::PlanOutcome::from_request(body.approved, body.clear_context);
            tx.send(outcome)
                .map(|_| {
                    Json(serde_json::json!({
                        "resolved": true,
                        "approved": body.approved,
                        "clearContext": outcome == crate::plan_exit::PlanOutcome::ApproveClearContext,
                    }))
                })
                .map_err(|_| StatusCode::NOT_FOUND)
        }
        None => Err(StatusCode::NOT_FOUND),
    }
}

/// Answer a Claude `can_use_tool` request (decision #336) through the host's own approval flow.
///
/// The request is sent as a `permission_only` call on the session's governed executor
/// (`LiveSession::driver_tool_tx` -> `SessionEngine::authorize_external_tool_call`), so it raises
/// the same approval card and honours the same session grants (#181 digest and pattern grants) as
/// a native engine call. Returns the answer in the `permission_prompt` shape
/// (`{"behavior":"allow","updatedInput":...}` or `{"behavior":"deny","message":...}`), which
/// `DriverHostRuntime::respond_server_request` turns into the `control_response`. Never allows by
/// default: unknown tools, malformed input, an unavailable executor and a dropped answer all
/// deny. There is no timer (tools rules 1 and 33): the request follows the one approval rule, so
/// it waits while a human client is attached and the engine denies it at once as `unattended`
/// when none is, or as `detached` or `cancelled` when the wait ends without an answer.
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
    // The MCP server the driver sees is named for the host app, so only its own tools pass.
    let tool_prefix = format!("mcp__{}__", crate::host_identity::app());
    let name = params
        .get("tool_name")
        .and_then(serde_json::Value::as_str)
        .and_then(|name| name.strip_prefix(tool_prefix.as_str()))
        .filter(|name| !name.is_empty() && *name != "permission_prompt");
    let Some(name) = name else {
        return deny(&format!(
            "Only {} tools are permitted",
            rightkit_agent_config::app_name()
        ));
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
    match receiver.await {
        Ok(result) if !result.is_error => {
            serde_json::json!({"behavior": "allow", "updatedInput": input})
        }
        Ok(result) => deny(&result.content),
        Err(_) => deny("Governed session executor dropped its answer"),
    }
}

#[cfg(test)]
mod pending_and_remember_tests {
    use super::*;

    #[test]
    fn a_plain_shell_command_gets_a_prefix_rule_and_a_plain_sentence() {
        let rule = project_allow_rule("shell", Some("touch probe.txt"), Some("mode:ask")).unwrap();
        assert_eq!(rule.patterns, vec!["shell(touch)", "shell(touch *)"]);
        assert_eq!(
            rule.description,
            "Always allow `touch` commands in this project"
        );
        let git = project_allow_rule("shell", Some("git status -s"), None).unwrap();
        assert_eq!(
            git.patterns,
            vec!["shell(git status)", "shell(git status *)"]
        );
    }

    #[test]
    fn broad_chained_other_tool_or_policy_held_calls_get_no_rule() {
        for (tool, command, source) in [
            ("shell", Some("bash -c ls"), Some("mode:ask")),
            ("shell", Some("rm -rf x"), None),
            ("shell", Some("ls; rm x"), None),
            ("shell", None, None),
            ("shell", Some("touch a"), Some("rule:deny-ish")),
            ("edit_file", Some("touch a"), None),
        ] {
            let reason = project_allow_rule(tool, command, source).unwrap_err();
            assert!(reason.ends_with("this call was allowed once."), "{reason}");
        }
    }

    #[test]
    fn approval_rows_list_only_open_calls_oldest_first() {
        let ctx =
            |at: &str, command: &str| rightkit_agent_engine_protocol::PendingApprovalContext {
                request_id: None,
                tool_name: "shell".into(),
                binding: Default::default(),
                command: Some(command.into()),
                network: None,
                payload_digest: None,
                effects: None,
                arguments_json: Some(format!("{{\"command\":\"{command}\"}}")),
                requested_at: Some(at.into()),
                reason: None,
                policy_source: None,
                proposed_change: None,
            };
        let mut contexts = std::collections::HashMap::new();
        contexts.insert("b".to_string(), ctx("2026-01-01T00:00:02Z", "ls"));
        contexts.insert("a".to_string(), ctx("2026-01-01T00:00:01Z", "pwd"));
        contexts.insert("gone".to_string(), ctx("2026-01-01T00:00:00Z", "id"));
        let open: std::collections::HashSet<String> =
            ["a", "b"].iter().map(|s| s.to_string()).collect();
        let rows = approval_rows(&contexts, &open);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["toolCallId"], "a");
        assert_eq!(rows[0]["arguments"]["command"], "pwd");
        assert_eq!(rows[1]["toolCallId"], "b");
        assert!(approval_rows(&contexts, &Default::default()).is_empty());
    }

    #[test]
    fn a_blank_typed_answer_is_detected_but_empty_lists_and_declines_are_not() {
        use serde_json::json;
        assert!(answers_has_blank_text(&json!({"0": "   "})));
        assert!(answers_has_blank_text(&json!({"0": "Red", "1": ""})));
        assert!(answers_has_blank_text(&json!("\n")));
        assert!(!answers_has_blank_text(&json!({"0": "Red"})));
        assert!(!answers_has_blank_text(&json!({"0": ["A", "B"]})));
        assert!(!answers_has_blank_text(&json!([])));
        assert!(!answers_has_blank_text(&json!({})));
        assert!(!answers_has_blank_text(
            &json!({"declined": true, "reason": "unattended"})
        ));
    }

    #[test]
    fn a_session_answer_names_the_command_it_kept() {
        let c = rightkit_agent_engine_protocol::PendingApprovalContext {
            request_id: None,
            tool_name: "shell".into(),
            binding: Default::default(),
            command: Some("touch probe.txt".into()),
            network: None,
            payload_digest: None,
            effects: None,
            arguments_json: None,
            requested_at: None,
            reason: None,
            policy_source: None,
            proposed_change: None,
        };
        let got = session_remembered(Some(&c), "session_command_grant");
        assert_eq!(got["scope"], "session");
        assert_eq!(got["rule"], "shell(touch probe.txt)");
    }
}
