pub(crate) fn parse_permission_decision(
    s: &str,
) -> Option<rightkit_agent_policy::PermissionDecision> {
    match s.trim().to_lowercase().as_str() {
        "allow" => Some(rightkit_agent_policy::PermissionDecision::Allow),
        "ask" => Some(rightkit_agent_policy::PermissionDecision::Ask),
        "deny" => Some(rightkit_agent_policy::PermissionDecision::Deny),
        _ => None,
    }
}

fn permission_decision_label(decision: rightkit_agent_policy::PermissionDecision) -> &'static str {
    match decision {
        rightkit_agent_policy::PermissionDecision::Allow => "allow",
        rightkit_agent_policy::PermissionDecision::Ask => "ask",
        rightkit_agent_policy::PermissionDecision::Deny => "deny",
    }
}

fn permission_rule_source_label(source: rightkit_agent_policy::RuleSource) -> &'static str {
    match source {
        rightkit_agent_policy::RuleSource::Builtin => "builtin",
        rightkit_agent_policy::RuleSource::Project => "project",
        rightkit_agent_policy::RuleSource::User => "user",
        rightkit_agent_policy::RuleSource::Session => "session",
    }
}

fn permission_rule_row(index: usize, rule: &rightkit_agent_policy::Rule) -> serde_json::Value {
    serde_json::json!({
        "index": index,
        "pattern": &rule.pattern,
        "effect": permission_decision_label(rule.effect),
        "source": permission_rule_source_label(rule.source),
    })
}

fn persistent_permission_rule_row(
    index: usize,
    rule: &PersistentPermissionRule,
    scope: &str,
    workspace_root: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "index": index,
        "pattern": &rule.pattern,
        "effect": &rule.effect,
        "scope": scope,
        "workspaceRoot": workspace_root,
    })
}

fn persistent_permission_rules_json(
    state: &DaemonState,
    store: &PersistentPermissionRuleStore,
    scope: &str,
    workspace_root: Option<&str>,
) -> Result<serde_json::Value, StatusCode> {
    match scope {
        "all" => Ok(serde_json::json!({
            "permissionRuleStorePath": state.permission_rule_store_path.display().to_string(),
            "user": store
                .user
                .iter()
                .enumerate()
                .map(|(index, rule)| persistent_permission_rule_row(index, rule, "user", None))
                .collect::<Vec<_>>(),
            "projects": store.projects,
        })),
        "user" => Ok(serde_json::json!({
            "permissionRuleStorePath": state.permission_rule_store_path.display().to_string(),
            "scope": "user",
            "rules": store
                .user
                .iter()
                .enumerate()
                .map(|(index, rule)| persistent_permission_rule_row(index, rule, "user", None))
                .collect::<Vec<_>>(),
        })),
        "project" => {
            let workspace = workspace_root
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or(StatusCode::BAD_REQUEST)?;
            let rules = store.projects.get(workspace).cloned().unwrap_or_default();
            Ok(serde_json::json!({
                "permissionRuleStorePath": state.permission_rule_store_path.display().to_string(),
                "scope": "project",
                "workspaceRoot": workspace,
                "rules": rules
                    .iter()
                    .enumerate()
                    .map(|(index, rule)| {
                        persistent_permission_rule_row(index, rule, "project", Some(workspace))
                    })
                    .collect::<Vec<_>>(),
            }))
        }
        _ => Err(StatusCode::BAD_REQUEST),
    }
}

fn validate_permission_rule_store(store: &PersistentPermissionRuleStore) -> Result<(), String> {
    for rule in &store.user {
        validate_persistent_permission_rule(rule)?;
    }
    for rules in store.projects.values() {
        for rule in rules {
            validate_persistent_permission_rule(rule)?;
        }
    }
    Ok(())
}

fn validate_persistent_permission_rule(rule: &PersistentPermissionRule) -> Result<(), String> {
    if rule.pattern.trim().is_empty() {
        return Err("empty permission rule pattern".to_string());
    }
    let effect = parse_permission_decision(&rule.effect)
        .ok_or_else(|| "invalid permission rule effect".to_string())?;
    rightkit_agent_policy::Rule::new(
        rule.pattern.clone(),
        effect,
        rightkit_agent_policy::RuleSource::User,
    )
    .validate()
    .map_err(|e| e.to_string())
}

fn permission_mode_label(mode: rightkit_agent_policy::PermissionMode) -> &'static str {
    match mode {
        rightkit_agent_policy::PermissionMode::Plan => "Plan",
        rightkit_agent_policy::PermissionMode::Ask => "Ask",
        rightkit_agent_policy::PermissionMode::AcceptEdits => "AcceptEdits",
        rightkit_agent_policy::PermissionMode::AutoSafe => "AutoSafe",
        rightkit_agent_policy::PermissionMode::Bypass => "Bypass",
    }
}

pub(crate) fn live_session_permission_mode(live: &LiveSession) -> String {
    live.permission_mode_tx
        .as_ref()
        .map(|tx| permission_mode_label(*tx.borrow()).to_string())
        .unwrap_or_else(|| "No live mode channel".into())
}
