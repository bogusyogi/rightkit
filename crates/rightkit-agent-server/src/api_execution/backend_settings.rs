fn execution_workspace_record_path(
    workspace_root: &std::path::Path,
    record: &ExecutionWorkspaceRecord,
) -> Result<PathBuf, String> {
    let raw_path = PathBuf::from(&record.paths.workspace);
    let path = if raw_path.is_absolute() {
        normalize_lexical(&raw_path)
    } else {
        normalize_lexical(&workspace_root.join(raw_path))
    };
    let normalized_workspace = normalize_lexical(workspace_root);
    if !path.starts_with(&normalized_workspace) {
        return Err("execution workspace path must stay inside workspaceRoot".to_string());
    }
    std::fs::create_dir_all(&path)
        .map_err(|err| format!("create execution workspace path: {err}"))?;
    Ok(path)
}

fn load_execution_backend_settings(
    workspace_root: &std::path::Path,
) -> Result<Option<ExecutionBackendDefaultsRecord>, ExecutionError> {
    let path = execution_backend_default_store_path(workspace_root);
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)
        .map_err(|e| ExecutionError::internal(format!("read execution backend defaults: {e}")))?;
    let record = serde_json::from_str(&content)
        .map_err(|e| ExecutionError::internal(format!("parse execution backend defaults: {e}")))?;
    Ok(Some(record))
}

fn default_execution_backend_settings(default_backend: &str) -> ExecutionBackendDefaultsRecord {
    ExecutionBackendDefaultsRecord {
        default_backend: default_backend.to_string(),
        backend_overrides: Vec::new(),
        network_profile_overrides: Vec::new(),
        updated_at: now_iso(),
    }
}

fn validate_resource_limits(
    limits: &rightkit_sandbox::execution::ResourceLimits,
) -> Result<(), ExecutionError> {
    if limits.cpu == Some(0) || limits.memory_mb == Some(0) || limits.disk_mb == Some(0) {
        return Err(ExecutionError::bad_request(
            "resource limits must be positive values",
        ));
    }
    Ok(())
}

fn validate_network_profile(
    registry: &rightkit_sandbox::execution::ExecutionBackendRegistry,
    network_profile: Option<&Option<String>>,
) -> Result<(), ExecutionError> {
    let Some(Some(profile_id)) = network_profile else {
        return Ok(());
    };
    let profile_id = profile_id.trim();
    if profile_id.is_empty() {
        return Ok(());
    }
    if registry
        .network_profiles
        .iter()
        .any(|profile| profile.id == profile_id)
    {
        return Ok(());
    }
    Err(ExecutionError::bad_request("unknown network profile"))
}

fn upsert_execution_network_profile_override(
    record: &mut ExecutionBackendDefaultsRecord,
    profile: rightkit_sandbox::execution::NetworkProfile,
) {
    if let Some(existing) = record
        .network_profile_overrides
        .iter_mut()
        .find(|existing| existing.id == profile.id)
    {
        *existing = profile;
        return;
    }
    record.network_profile_overrides.push(profile);
}

fn clean_network_profile_id(value: &str) -> Result<String, ExecutionError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ExecutionError::bad_request(
            "network profile id is required",
        ));
    }
    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(ExecutionError::bad_request(
            "network profile id must use letters, numbers, dots, dashes, or underscores",
        ));
    }
    Ok(trimmed.to_string())
}

fn normalize_network_profile_hosts(values: Vec<String>) -> Result<Vec<String>, ExecutionError> {
    let mut hosts = Vec::new();
    for value in values {
        let host = value.trim();
        if host.is_empty() {
            continue;
        }
        if host.contains("://")
            || host.contains('/')
            || host.contains('*')
            || host.chars().any(char::is_whitespace)
        {
            return Err(ExecutionError::bad_request(
                "network profile allow hosts must be hostnames or host:port values",
            ));
        }
        if !hosts.iter().any(|existing| existing == host) {
            hosts.push(host.to_string());
        }
    }
    Ok(hosts)
}

fn upsert_execution_backend_override(
    record: &mut ExecutionBackendDefaultsRecord,
    backend_id: &str,
    request: &UpdateExecutionBackendRequest,
) {
    if let Some(override_record) = record
        .backend_overrides
        .iter_mut()
        .find(|override_record| override_record.id == backend_id)
    {
        if let Some(enabled) = request.enabled {
            override_record.enabled = Some(enabled);
        }
        if let Some(resource_limits) = request.resource_limits.clone() {
            override_record.resource_limits = Some(resource_limits);
        }
        if let Some(network_profile) = request.network_profile.clone() {
            override_record.network_profile = Some(normalize_network_profile(network_profile));
        }
        return;
    }
    record
        .backend_overrides
        .push(ExecutionBackendOverrideRecord {
            id: backend_id.to_string(),
            enabled: request.enabled,
            resource_limits: request.resource_limits.clone(),
            network_profile: request
                .network_profile
                .clone()
                .map(normalize_network_profile),
        });
}

fn normalize_network_profile(network_profile: Option<String>) -> Option<String> {
    network_profile.and_then(|value| {
        let trimmed = value.trim().to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

fn save_execution_backend_settings(
    workspace_root: &std::path::Path,
    record: &ExecutionBackendDefaultsRecord,
) -> Result<(), ExecutionError> {
    let path = execution_backend_default_store_path(workspace_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            ExecutionError::internal(format!("create execution backend defaults dir: {e}"))
        })?;
    }
    let raw = serde_json::to_string_pretty(record)
        .map_err(|e| ExecutionError::internal(format!("serialize backend defaults: {e}")))?;
    crate::store_write::write_store_file(&path, raw.as_bytes())
        .map_err(|e| ExecutionError::internal(format!("write execution backend defaults: {e}")))
}
