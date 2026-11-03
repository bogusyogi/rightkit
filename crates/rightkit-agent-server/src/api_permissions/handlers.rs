/// Change a session's permission mode at runtime. The engine adopts the new mode
/// at the start of its next turn. Accepts `plan` / `ask` / `accept-edits` /
/// `auto-safe` / `bypass`.
async fn set_permission_mode_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<SetPermissionMode>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_policy_mutation_authority(&headers)?;
    let live = match state.get_session(&id) {
        Some(live) => live,
        None => state
            .resume_session(&id)
            .await
            .ok_or(StatusCode::NOT_FOUND)?,
    };
    let mode = rightkit_agent_policy::parse_permission_mode(Some(&body.mode));
    set_permission_mode_and_tighten_descendants(&state, &live, mode);
    Ok(Json(serde_json::json!({ "mode": format!("{mode:?}") })))
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
    let snapshot = {
        let mut store = lock_recover(&state.workspace_trust_store, "state.workspace_trust_store");
        store.projects.insert(key, record);
        store.clone()
    };
    write_workspace_trust_store(&state.workspace_trust_store_path, &snapshot)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
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
    let record = state
        .workspace_trust_store
        .lock()
        .unwrap()
        .projects
        .get(&key)
        .cloned();
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
    let row = {
        let mut store = lock_recover(&state.permission_rule_store, "state.permission_rule_store");
        let row = match body.scope.trim().to_lowercase().as_str() {
            "user" => {
                store.user.push(rule);
                let index = store.user.len().saturating_sub(1);
                persistent_permission_rule_row(index, &store.user[index], "user", None)
            }
            "project" => {
                let workspace = body
                    .workspace_root
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or(StatusCode::BAD_REQUEST)?
                    .to_string();
                let rules = store.projects.entry(workspace.clone()).or_default();
                rules.push(rule);
                let index = rules.len().saturating_sub(1);
                persistent_permission_rule_row(index, &rules[index], "project", Some(&workspace))
            }
            _ => return Err(StatusCode::BAD_REQUEST),
        };
        write_permission_rule_store(&state.permission_rule_store_path, &store)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        row
    };
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
    let removed = {
        let mut store = lock_recover(&state.permission_rule_store, "state.permission_rule_store");
        let removed = match scope {
            "user" => {
                if index >= store.user.len() {
                    None
                } else {
                    Some(persistent_permission_rule_row(
                        index,
                        &store.user.remove(index),
                        "user",
                        None,
                    ))
                }
            }
            "project" => {
                let workspace = query
                    .workspace_root
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or(StatusCode::BAD_REQUEST)?;
                let rules = store
                    .projects
                    .get_mut(workspace)
                    .ok_or(StatusCode::NOT_FOUND)?;
                if index >= rules.len() {
                    None
                } else {
                    Some(persistent_permission_rule_row(
                        index,
                        &rules.remove(index),
                        "project",
                        Some(workspace),
                    ))
                }
            }
            _ => return Err(StatusCode::BAD_REQUEST),
        };
        if removed.is_some() {
            write_permission_rule_store(&state.permission_rule_store_path, &store)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
        removed
    };
    match removed {
        Some(rule) => Ok(Json(serde_json::json!({ "removed": true, "rule": rule }))),
        None => Err(StatusCode::NOT_FOUND),
    }
}

async fn export_permission_rules_handler(
    State(state): State<DaemonState>,
) -> Json<PersistentPermissionRuleStore> {
    Json(lock_recover(&state.permission_rule_store, "state.permission_rule_store").clone())
}

async fn import_permission_rules_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(body): Json<PersistentPermissionRuleStore>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_policy_mutation_authority(&headers)?;
    validate_permission_rule_store(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    {
        let mut store = lock_recover(&state.permission_rule_store, "state.permission_rule_store");
        *store = body.clone();
        write_permission_rule_store(&state.permission_rule_store_path, &store)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
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
