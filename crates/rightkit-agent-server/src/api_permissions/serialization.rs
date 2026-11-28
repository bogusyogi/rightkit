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
            let rules = project_rules_for(store, workspace);
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

/// Key for a workspace's project permission rules, so one folder spelled differently
/// (`C:\Repo` / `c:/repo/`, `/Users/a/proj/` / `/Users/a/proj`, a symlinked path) finds the same
/// rules. An existing folder is canonicalised (links resolved; the Windows `\\?\` prefix
/// dropped), then the trust-key rules apply: forward slashes, no trailing separator. Case is
/// folded on Windows and macOS, whose default file systems ignore case.
pub(crate) fn project_rule_key(workspace_root: &str) -> String {
    let trimmed = workspace_root.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let resolved = std::fs::canonicalize(trimmed)
        .ok()
        .map(|path| strip_verbatim_prefix(&path.to_string_lossy()))
        .unwrap_or_else(|| trimmed.to_string());
    project_rule_key_with(&resolved, cfg!(any(windows, target_os = "macos")))
}

fn project_rule_key_with(workspace_root: &str, fold_case: bool) -> String {
    let mut key = workspace_root.trim().replace('\\', "/");
    let drive_root =
        |k: &str| k.len() == 3 && k.as_bytes()[0].is_ascii_alphabetic() && k.ends_with(":/");
    while key.len() > 1 && key.ends_with('/') && !drive_root(&key) {
        key.pop();
    }
    if fold_case {
        key = key.to_lowercase();
    }
    key
}

/// `\\?\C:\x` -> `C:\x`, `\\?\UNC\host\share` -> `\\host\share` (what `canonicalize` returns on
/// Windows); any other path is returned unchanged.
fn strip_verbatim_prefix(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        path.to_string()
    }
}

/// Every stored rule for `workspace_root`, as `(stored key, index in that key's list)`, in a
/// stable order (stored keys sorted). Stored keys are compared by [`project_rule_key`], so
/// entries written under older spellings still apply. The list route, session seeding and
/// deletion all use this one view, so an index means the same rule everywhere.
pub(crate) fn project_rule_slots(
    store: &PersistentPermissionRuleStore,
    workspace_root: &str,
) -> Vec<(String, usize)> {
    let key = project_rule_key(workspace_root);
    if key.is_empty() {
        return Vec::new();
    }
    let mut stored: Vec<&String> = store
        .projects
        .keys()
        .filter(|stored| project_rule_key(stored) == key)
        .collect();
    stored.sort();
    stored
        .into_iter()
        .flat_map(|stored| {
            (0..store.projects[stored].len()).map(move |index| (stored.clone(), index))
        })
        .collect()
}

/// The project rules that apply to `workspace_root` (see [`project_rule_slots`]).
pub(crate) fn project_rules_for(
    store: &PersistentPermissionRuleStore,
    workspace_root: &str,
) -> Vec<PersistentPermissionRule> {
    project_rule_slots(store, workspace_root)
        .into_iter()
        .map(|(stored, index)| store.projects[&stored][index].clone())
        .collect()
}

#[cfg(test)]
mod project_rule_key_tests {
    use super::*;

    fn rule(pattern: &str, effect: &str) -> PersistentPermissionRule {
        PersistentPermissionRule {
            pattern: pattern.to_string(),
            effect: effect.to_string(),
        }
    }

    #[test]
    fn spellings_of_one_folder_share_a_key() {
        assert_eq!(
            project_rule_key_with("/Users/a/proj/", false),
            "/Users/a/proj"
        );
        assert_eq!(project_rule_key_with(" C:\\Repo\\ ", true), "c:/repo");
        assert_eq!(project_rule_key_with("c:/repo", true), "c:/repo");
        assert_eq!(project_rule_key_with("C:\\", true), "c:/");
        assert_eq!(project_rule_key_with("/", false), "/");
        // Case is kept where the platform rule says so.
        assert_ne!(
            project_rule_key_with("/a/Proj", false),
            project_rule_key_with("/a/proj", false)
        );
        assert_eq!(strip_verbatim_prefix(r"\\?\C:\Repo"), r"C:\Repo");
        assert_eq!(
            strip_verbatim_prefix(r"\\?\UNC\host\share"),
            r"\\host\share"
        );
        assert_eq!(project_rule_key(""), "");
    }

    #[test]
    fn existing_folder_matches_through_trailing_separator_and_link() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("proj");
        std::fs::create_dir_all(&real).unwrap();
        let plain = real.to_string_lossy().to_string();
        let trailing = format!("{plain}{}", std::path::MAIN_SEPARATOR);
        assert_eq!(project_rule_key(&plain), project_rule_key(&trailing));
        #[cfg(unix)]
        {
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            assert_eq!(
                project_rule_key(&link.to_string_lossy()),
                project_rule_key(&plain)
            );
        }
    }

    #[test]
    fn rules_stored_under_another_spelling_still_apply() {
        let mut store = PersistentPermissionRuleStore::default();
        store.projects.insert(
            "/nonexistent/ws/".to_string(),
            vec![rule("Bash(rm:*)", "deny")],
        );
        store.projects.insert(
            "/nonexistent/ws".to_string(),
            vec![rule("Read(*)", "allow")],
        );
        store.projects.insert(
            "/nonexistent/other".to_string(),
            vec![rule("Edit(*)", "deny")],
        );
        let rules = project_rules_for(&store, "/nonexistent/ws");
        assert_eq!(rules.len(), 2);
        assert!(rules.iter().any(|r| r.pattern == "Bash(rm:*)"));
        assert!(!rules.iter().any(|r| r.pattern == "Edit(*)"));
        // Stable order: the slots view is sorted by stored key.
        let slots = project_rule_slots(&store, "/nonexistent/ws/");
        assert_eq!(slots[0], ("/nonexistent/ws".to_string(), 0));
        assert_eq!(slots[1], ("/nonexistent/ws/".to_string(), 0));
    }
}
