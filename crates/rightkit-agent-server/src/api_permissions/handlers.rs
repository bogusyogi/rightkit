/// Change a session's permission mode at runtime. The engine adopts the new mode
/// at the start of its next turn. Accepts `plan` / `ask` / `accept-edits` /
/// `auto-safe` / `bypass`.
async fn set_permission_mode_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<SetPermissionMode>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if let Err(refusal) = enforce_policy_mutation_authority(&headers) {
        return refusal.into_response();
    }
    let live = match state.get_session(&id) {
        Some(live) => live,
        None => match state.resume_session(&id).await {
            Some(live) => live,
            None => return StatusCode::NOT_FOUND.into_response(),
        },
    };
    // Rule 7: a value outside the accepted spellings is a 400, never a silent `Ask`.
    let Some(mode) = parse_requested_permission_mode(&body.mode) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    // Rule 62: `Bypass` is refused while the workspace is untrusted; the error names the state.
    if let Some(message) = permission_mode_refusal(&state, &live, mode) {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": message, "code": "bypass_refused_untrusted" })),
        )
            .into_response();
    }
    set_permission_mode_and_tighten_descendants(&state, &live, mode);
    Json(serde_json::json!({ "mode": format!("{mode:?}") })).into_response()
}

/// Rule 62: why `mode` may not be applied to `live` now, or `None`. Trust is read live, so a
/// workspace whose trust was lowered since the session began is refused too.
pub(crate) fn permission_mode_refusal(
    state: &DaemonState,
    live: &std::sync::Arc<LiveSession>,
    mode: rightkit_agent_policy::PermissionMode,
) -> Option<String> {
    rightkit_agent_policy::TrustPolicy::bypass_refusal(
        mode,
        state.session_workspace_trust(&live.workspace_root),
    )
}

/// The accepted spellings of the `permission-mode` route: the wire names (strict parser) plus the
/// user-facing names `yolo` and `auto` that the CLI `permission mode` command posts verbatim.
pub(crate) fn parse_requested_permission_mode(
    value: &str,
) -> Option<rightkit_agent_policy::PermissionMode> {
    rightkit_agent_policy::parse_permission_mode_strict(value)
        .or_else(|| rightkit_agent_policy::PermissionMode::from_user_name(value))
}

fn set_permission_mode_and_tighten_descendants(
    state: &DaemonState,
    live: &std::sync::Arc<LiveSession>,
    mode: rightkit_agent_policy::PermissionMode,
) {
    if let Some(tx) = &live.permission_mode_tx {
        if *tx.borrow() != mode {
            tx.send_replace(mode);
            notify_compaction_revision(live);
        }
    }

    let descendants = {
        let sessions = lock_recover(&state.sessions, "state.sessions");
        let mut children =
            std::collections::HashMap::<String, Vec<std::sync::Arc<LiveSession>>>::new();
        for session in sessions.values() {
            if let Some(parent_id) = &session.parent_session_id {
                children
                    .entry(parent_id.clone())
                    .or_default()
                    .push(session.clone());
            }
        }

        let mut descendants = Vec::new();
        let mut pending = std::collections::VecDeque::from([live.id.clone()]);
        let mut visited = std::collections::HashSet::from([live.id.clone()]);
        while let Some(parent_id) = pending.pop_front() {
            for child in children.remove(&parent_id).unwrap_or_default() {
                if visited.insert(child.id.clone()) {
                    pending.push_back(child.id.clone());
                    descendants.push(child);
                }
            }
        }
        descendants
    };

    for descendant in descendants {
        if let Some(tx) = &descendant.permission_mode_tx {
            let current = *tx.borrow();
            let tightened = stricter_permission_mode(current, mode);
            if tightened != current {
                tx.send_replace(tightened);
                notify_compaction_revision(&descendant);
            }
        }
    }
}

fn notify_compaction_revision(live: &std::sync::Arc<LiveSession>) {
    if let Some(notifier) = &live.compaction_revision_notifier {
        notifier();
    }
}

fn stricter_permission_mode(
    left: rightkit_agent_policy::PermissionMode,
    right: rightkit_agent_policy::PermissionMode,
) -> rightkit_agent_policy::PermissionMode {
    if permission_mode_rank(left) <= permission_mode_rank(right) {
        left
    } else {
        right
    }
}

fn permission_mode_rank(mode: rightkit_agent_policy::PermissionMode) -> u8 {
    match mode {
        rightkit_agent_policy::PermissionMode::Plan => 0,
        rightkit_agent_policy::PermissionMode::Ask => 1,
        rightkit_agent_policy::PermissionMode::AcceptEdits => 2,
        rightkit_agent_policy::PermissionMode::AutoSafe => 3,
        rightkit_agent_policy::PermissionMode::Bypass => 4,
    }
}

pub(crate) async fn list_permission_rules_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let live = state.get_session(&id).ok_or(StatusCode::NOT_FOUND)?;
    let rules = lock_recover(&live.permission_rules, "live.permission_rules");
    Ok(Json(serde_json::json!({
        "rules": rules
            .all_rules()
            .iter()
            .enumerate()
            .map(|(index, rule)| permission_rule_row(index, rule))
            .collect::<Vec<_>>()
    })))
}

pub(crate) async fn add_permission_rule_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<AddPermissionRule>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_policy_mutation_authority(&headers)?;
    let live = state.get_session(&id).ok_or(StatusCode::NOT_FOUND)?;
    let pattern = body.pattern.trim();
    if pattern.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let effect = parse_permission_decision(&body.effect).ok_or(StatusCode::BAD_REQUEST)?;
    let row = {
        let mut rules = lock_recover(&live.permission_rules, "live.permission_rules");
        rules
            .try_add_all(vec![rightkit_agent_policy::Rule::new(
                pattern,
                effect,
                rightkit_agent_policy::RuleSource::Session,
            )])
            .map_err(|_| StatusCode::BAD_REQUEST)?;
        let index = rules.all_rules().len().saturating_sub(1);
        permission_rule_row(index, &rules.all_rules()[index])
    };
    notify_compaction_revision(&live);
    Ok(Json(serde_json::json!({ "rule": row })))
}

async fn delete_permission_rule_handler(
    State(state): State<DaemonState>,
    Path((id, index)): Path<(String, usize)>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_policy_mutation_authority(&headers)?;
    let live = state.get_session(&id).ok_or(StatusCode::NOT_FOUND)?;
    let removed = lock_recover(&live.permission_rules, "live.permission_rules").remove_at(index);
    match removed {
        Some(rule) => {
            notify_compaction_revision(&live);
            Ok(Json(serde_json::json!({
            "removed": true,
            "rule": permission_rule_row(index, &rule),
            })))
        }
        None => Err(StatusCode::NOT_FOUND),
    }
}

async fn get_workspace_trust_handler(
    State(state): State<DaemonState>,
    Query(query): Query<WorkspaceTrustQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    workspace_trust_json(&state, &query.workspace_root)
}

async fn set_workspace_trust_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(body): Json<SetWorkspaceTrust>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_policy_mutation_authority(&headers)?;
    let key = workspace_trust_key(&body.workspace_root);
    if key.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let trust = parse_workspace_trust_label(&body.trust).ok_or(StatusCode::BAD_REQUEST)?;
    let record = WorkspaceTrustRecord {
        trust: workspace_trust_label(trust).to_string(),
        approved_by: body
            .approved_by
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        updated_at: now_iso(),
    };
    // Read-modify-write under the cross-process lock: a decision made by the CLI or TUI since the
    // daemon started is kept, and a corrupt file is refused instead of replaced (rules 10, 12).
    let merged = upsert_workspace_trust_record(&state.workspace_trust_store_path, key, record)
        .map_err(|error| {
            tracing::error!(%error, "workspace trust update refused");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    *lock_recover(&state.workspace_trust_store, "state.workspace_trust_store") = merged;
    workspace_trust_json(&state, &body.workspace_root)
}

fn workspace_trust_json(
    state: &DaemonState,
    workspace_root: &str,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let key = workspace_trust_key(workspace_root);
    if key.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    // Rule 60: the nearest recorded ancestor decides.
    let record = {
        let store = lock_recover(&state.workspace_trust_store, "state.workspace_trust_store");
        rightkit_agent_config::nearest_trust_record(workspace_root, |candidate| {
            store.projects.get(candidate).cloned()
        })
    };
    let trust = record
        .as_ref()
        .and_then(|record| parse_workspace_trust_label(&record.trust))
        .unwrap_or(rightkit_agent_policy::WorkspaceTrust::Untrusted);
    Ok(Json(serde_json::json!({
        "workspaceRoot": workspace_root,
        "trust": workspace_trust_label(trust),
        "trusted": trust == rightkit_agent_policy::WorkspaceTrust::Trusted,
        "requiresTrust": record.is_none(),
        "approvedBy": record.as_ref().and_then(|record| record.approved_by.clone()),
        "updatedAt": record.as_ref().map(|record| record.updated_at.clone()),
        "storePath": state.workspace_trust_store_path,
    })))
}

pub(crate) async fn list_persistent_permission_rules_handler(
    State(state): State<DaemonState>,
    Query(query): Query<PersistentPermissionRuleQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let scope = query.scope.as_deref().unwrap_or("all");
    // Also picks up rules another host process wrote since this one last read the file.
    let load_error = crate::api_stores::refresh_permission_rule_store(
        &state.permission_rule_store_path,
        &state.permission_rule_store,
    );
    let store = lock_recover(&state.permission_rule_store, "state.permission_rule_store").clone();
    let mut body =
        persistent_permission_rules_json(&state, &store, scope, query.workspace_root.as_deref())?;
    if let (Some(error), Some(object)) = (load_error, body.as_object_mut()) {
        object.insert("loadError".to_string(), error.to_json());
    }
    Ok(Json(body))
}

pub(crate) async fn add_persistent_permission_rule_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(body): Json<AddPersistentPermissionRule>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_policy_mutation_authority(&headers)?;
    let pattern = body.pattern.trim();
    if pattern.is_empty() || parse_permission_decision(&body.effect).is_none() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let rule = PersistentPermissionRule {
        pattern: pattern.to_string(),
        effect: permission_decision_label(
            parse_permission_decision(&body.effect).ok_or(StatusCode::BAD_REQUEST)?,
        )
        .to_string(),
    };
    // A malformed persisted rule is a silently dropped deny: reject before
    // it reaches the store or disk.
    validate_persistent_permission_rule(&rule).map_err(|_| StatusCode::BAD_REQUEST)?;
    // Several processes share the rule file, so this one rule is added to the rules ON DISK
    // (read again under the cross-process lock), never to a copy of this process's store: a rule
    // another process added, a deny above all, is kept. The result becomes live only after the
    // disk write succeeds, so a failed write (500) leaves no rule in memory that the client was
    // told failed.
    let row = update_permission_rule_store(
        &state.permission_rule_store_path,
        &state.permission_rule_store,
        |next, _held| {
            let row = match body.scope.trim().to_lowercase().as_str() {
                "user" => {
                    next.user.push(rule);
                    let index = next.user.len().saturating_sub(1);
                    persistent_permission_rule_row(index, &next.user[index], "user", None)
                }
                "project" => {
                    let workspace = body
                        .workspace_root
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .ok_or(StatusCode::BAD_REQUEST)?
                        .to_string();
                    // Stored under the normalised key, so every spelling of the folder finds it.
                    let key = project_rule_key(&workspace);
                    if key.is_empty() {
                        return Err(StatusCode::BAD_REQUEST);
                    }
                    let rules = next.projects.entry(key.clone()).or_default();
                    rules.push(rule.clone());
                    let local = rules.len().saturating_sub(1);
                    let index = project_rule_slots(next, &workspace)
                        .iter()
                        .position(|(stored, at)| *stored == key && *at == local)
                        .unwrap_or(local);
                    persistent_permission_rule_row(index, &rule, "project", Some(&workspace))
                }
                _ => return Err(StatusCode::BAD_REQUEST),
            };
            Ok(Some(row))
        },
    )
    .map_err(permission_rule_update_status)?
    .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({ "rule": row })))
}

async fn delete_persistent_permission_rule_handler(
    State(state): State<DaemonState>,
    Path(index): Path<usize>,
    Query(query): Query<PersistentPermissionRuleQuery>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_policy_mutation_authority(&headers)?;
    let scope = query.scope.as_deref().unwrap_or("user");
    // The one rule is removed from the rules ON DISK (read again under the cross-process lock),
    // written, and only then made live (see the add handler). An index is a position in a
    // listing, and another process may have changed the file since the client listed it, so the
    // rule now at that index on disk must be the one the client meant: the one it names
    // (`pattern`/`effect`) and, unless it names both, the one this process last showed at that
    // index. Otherwise the delete is refused with 409 (and the next listing shows the file as it
    // is), so a concurrent change can never make this delete remove another rule (a deny).
    let removed = update_permission_rule_store(
        &state.permission_rule_store_path,
        &state.permission_rule_store,
        |next, held| {
            let removed = match scope {
                "user" => match next.user.get(index) {
                    None => None,
                    Some(rule) => {
                        if !persistent_rule_is_expected(rule, &query)
                            || !persistent_rule_is_the_one_shown(rule, held.user.get(index), &query)
                        {
                            return Err(StatusCode::CONFLICT);
                        }
                        Some(persistent_permission_rule_row(
                            index,
                            &next.user.remove(index),
                            "user",
                            None,
                        ))
                    }
                },
                "project" => {
                    let workspace = query
                        .workspace_root
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .ok_or(StatusCode::BAD_REQUEST)?;
                    let slots = project_rule_slots(next, workspace);
                    if slots.is_empty() {
                        return Err(StatusCode::NOT_FOUND);
                    }
                    match slots.get(index) {
                        None => None,
                        Some((stored, local)) => {
                            let shown = project_rules_for(held, workspace);
                            let rules =
                                next.projects.get_mut(stored).ok_or(StatusCode::NOT_FOUND)?;
                            if !persistent_rule_is_expected(&rules[*local], &query)
                                || !persistent_rule_is_the_one_shown(
                                    &rules[*local],
                                    shown.get(index),
                                    &query,
                                )
                            {
                                return Err(StatusCode::CONFLICT);
                            }
                            Some(persistent_permission_rule_row(
                                index,
                                &rules.remove(*local),
                                "project",
                                Some(workspace),
                            ))
                        }
                    }
                }
                _ => return Err(StatusCode::BAD_REQUEST),
            };
            Ok(removed)
        },
    )
    .map_err(permission_rule_update_status)?;
    match removed {
        Some(rule) => Ok(Json(serde_json::json!({ "removed": true, "rule": rule }))),
        None => Err(StatusCode::NOT_FOUND),
    }
}

/// The HTTP status of a refused or failed rule update: the handler's own refusal as it is, a
/// storage failure (file locked too long, unreadable, unwritable) as 500 with the reason logged.
fn permission_rule_update_status(
    error: crate::api_stores::PermissionRuleUpdateError<StatusCode>,
) -> StatusCode {
    match error {
        crate::api_stores::PermissionRuleUpdateError::Rejected(status) => status,
        crate::api_stores::PermissionRuleUpdateError::Storage(error) => {
            tracing::error!(%error, "permission rule update refused");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

/// Whether `rule` (at the client's index in the file) is the rule this process last showed at
/// that index (`shown`). A client that names both `pattern` and `effect` has identified the rule
/// itself and needs no such check. Otherwise the index is all it gave, and it can only mean what
/// this process listed: when another process has changed the file since, the delete is refused.
fn persistent_rule_is_the_one_shown(
    rule: &PersistentPermissionRule,
    shown: Option<&PersistentPermissionRule>,
    query: &PersistentPermissionRuleQuery,
) -> bool {
    if query.pattern.is_some() && query.effect.is_some() {
        return true;
    }
    shown.is_some_and(|shown| shown.pattern == rule.pattern && shown.effect == rule.effect)
}

/// Write `next` to disk and only then make it the live store. On a failed write (including the
/// refusal to replace a rule file that failed to load) the live store is left as it was.
fn commit_permission_rule_store(
    store: &mut PersistentPermissionRuleStore,
    next: PersistentPermissionRuleStore,
    path: &std::path::Path,
) -> Result<(), StatusCode> {
    write_permission_rule_store(path, &next).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    *store = next;
    Ok(())
}

/// Whether `rule` is the one the client named in a delete (`pattern` and `effect` query
/// fields). A field the client left out is not checked.
fn persistent_rule_is_expected(
    rule: &PersistentPermissionRule,
    query: &PersistentPermissionRuleQuery,
) -> bool {
    let pattern_ok = query
        .pattern
        .as_deref()
        .is_none_or(|pattern| pattern.trim() == rule.pattern.trim());
    let effect_ok = query.effect.as_deref().is_none_or(|effect| {
        parse_permission_decision(effect).is_some()
            && parse_permission_decision(effect) == parse_permission_decision(&rule.effect)
    });
    pattern_ok && effect_ok
}

async fn export_permission_rules_handler(
    State(state): State<DaemonState>,
) -> Json<PersistentPermissionRuleStore> {
    // Also picks up rules another host process wrote since this one last read the file.
    crate::api_stores::refresh_permission_rule_store(
        &state.permission_rule_store_path,
        &state.permission_rule_store,
    );
    Json(lock_recover(&state.permission_rule_store, "state.permission_rule_store").clone())
}

async fn import_permission_rules_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(body): Json<PersistentPermissionRuleStore>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_policy_mutation_authority(&headers)?;
    validate_permission_rule_store(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    // An import replaces the whole rule set by design. It takes the same cross-process lock as
    // the single-rule changes, so it never lands in the middle of another process's
    // read-change-write (which would then put the pre-import rules back).
    crate::store_write::with_store_lock(
        &state.permission_rule_store_path,
        "permission-rules",
        || {
            // Written first; the in-memory store changes only once the file holds the import.
            let mut store =
                lock_recover(&state.permission_rule_store, "state.permission_rule_store");
            commit_permission_rule_store(
                &mut store,
                body.clone(),
                &state.permission_rule_store_path,
            )
        },
    )
    .map_err(|error| {
        tracing::error!(%error, "permission rule import refused");
        StatusCode::INTERNAL_SERVER_ERROR
    })??;
    Ok(Json(serde_json::json!({
        "imported": true,
        "userRules": body.user.len(),
        "projectScopes": body.projects.len(),
    })))
}

async fn list_permission_denials_handler(
    State(state): State<DaemonState>,
    Query(query): Query<PermissionDenialQuery>,
) -> Json<serde_json::Value> {
    let limit = query.limit.unwrap_or(50).min(500);
    // Also picks up denials another host process recorded since this one last read the file.
    crate::api_stores::refresh_permission_denial_store(
        &state.permission_denial_store_path,
        &state.permission_denials,
    );
    let mut denials = lock_recover(&state.permission_denials, "state.permission_denials").clone();
    if let Some(session_id) = query.session_id.as_deref() {
        denials.retain(|row| row.session_id == session_id);
    }
    denials.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    denials.truncate(limit);
    Json(serde_json::json!({
        "permissionDenialStorePath": state.permission_denial_store_path.display().to_string(),
        "denials": denials,
    }))
}

#[cfg(test)]
mod persistent_rule_mutation_tests {
    use super::*;

    fn rule(pattern: &str, effect: &str) -> PersistentPermissionRule {
        PersistentPermissionRule {
            pattern: pattern.to_string(),
            effect: effect.to_string(),
        }
    }

    #[test]
    fn failed_write_leaves_the_live_store_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the rule file should be: unreadable, so the write is refused.
        let blocked = dir.path().join("blocked.json");
        std::fs::create_dir_all(&blocked).unwrap();
        let mut live = PersistentPermissionRuleStore::default();
        live.user.push(rule("Bash(rm:*)", "deny"));
        let mut next = live.clone();
        next.user.clear();
        next.user.push(rule("Bash(*)", "allow"));
        assert!(commit_permission_rule_store(&mut live, next.clone(), &blocked).is_err());
        assert_eq!(live.user.len(), 1);
        assert_eq!(live.user[0].effect, "deny");

        let ok = dir.path().join("rules.json");
        commit_permission_rule_store(&mut live, next, &ok).unwrap();
        assert_eq!(live.user[0].pattern, "Bash(*)");
        assert!(std::fs::read_to_string(&ok).unwrap().contains("Bash(*)"));
    }

    #[test]
    fn delete_checks_the_named_rule() {
        let deny = rule("Bash(rm:*)", "deny");
        let unnamed = PersistentPermissionRuleQuery::default();
        assert!(persistent_rule_is_expected(&deny, &unnamed));
        let same = PersistentPermissionRuleQuery {
            pattern: Some(" Bash(rm:*) ".into()),
            effect: Some("DENY".into()),
            ..Default::default()
        };
        assert!(persistent_rule_is_expected(&deny, &same));
        let moved = PersistentPermissionRuleQuery {
            pattern: Some("Read(*)".into()),
            ..Default::default()
        };
        assert!(!persistent_rule_is_expected(&deny, &moved));
        let wrong_effect = PersistentPermissionRuleQuery {
            effect: Some("allow".into()),
            ..Default::default()
        };
        assert!(!persistent_rule_is_expected(&deny, &wrong_effect));
        let bad_effect = PersistentPermissionRuleQuery {
            effect: Some("maybe".into()),
            ..Default::default()
        };
        assert!(!persistent_rule_is_expected(&deny, &bad_effect));
    }
}
