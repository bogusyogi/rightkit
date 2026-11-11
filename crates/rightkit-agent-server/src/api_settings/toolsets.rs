fn apply_budget_to_store(
    store: &mut crate::api::DaemonSettingsStore,
    budget: &rightkit_agent_config::BudgetConfig,
) {
    store.per_task_max_usd = budget.per_task_max_usd;
    store.per_goal_max_usd = budget.per_goal_max_usd;
    store.per_session_max_usd = budget.per_session_max_usd;
    store.per_agent_call_max_usd = budget.per_agent_call_max_usd;
    store.max_retries_per_role = budget.max_retries_per_role;
    store.max_wall_clock_minutes = budget.max_wall_clock_minutes;
    store.max_iterations = budget.max_iterations;
    store.max_workers = budget.max_workers;
}

fn configured_toolset_profiles(
    config: &rightkit_agent_config::HostConfig,
) -> Vec<ToolsetSettingsProfile> {
    config
        .toolsets
        .iter()
        .collect::<BTreeMap<_, _>>()
        .into_iter()
        .map(|(id, profile)| toolset_settings_profile(id, profile))
        .collect()
}

fn toolset_settings_profile(
    id: &str,
    profile: &rightkit_agent_config::ToolsetConfig,
) -> ToolsetSettingsProfile {
    ToolsetSettingsProfile {
        id: id.to_string(),
        name: profile.name.clone(),
        description: profile.description.clone(),
        allowed_tools: profile.allowed_tools.clone(),
        disallowed_tools: profile.disallowed_tools.clone(),
        read_only: profile.read_only,
        default_behavioral_skills: profile.default_behavioral_skills.clone(),
    }
}

fn clean_list(values: Vec<String>) -> Vec<String> {
    let mut cleaned = Vec::new();
    for value in values {
        let value = value.trim();
        if !value.is_empty() && !cleaned.iter().any(|existing| existing == value) {
            cleaned.push(value.to_string());
        }
    }
    cleaned
}

fn validate_toolset_id(id: &str) -> Result<(), String> {
    let trimmed = id.trim();
    if trimmed.is_empty() || trimmed != id {
        return Err("toolset id must be non-empty and trimmed".to_string());
    }
    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(
            "toolset id may contain only ASCII letters, digits, '.', '-', or '_'".to_string(),
        );
    }
    Ok(())
}
