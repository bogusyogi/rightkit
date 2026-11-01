#[derive(Debug)]
struct ExecutionError {
    status: StatusCode,
    message: String,
}

impl ExecutionError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
}

fn enforce_execution_trusted_source_authority(headers: &HeaderMap) -> Result<(), ExecutionError> {
    enforce_trusted_source_authority(headers).map_err(|status| ExecutionError {
        status,
        message: "source is not authorized for execution control-plane mutations".to_string(),
    })
}

fn enforce_execution_backend_license(state: &DaemonState) -> Result<(), ExecutionError> {
    require_license_feature(state, "execution_backends").map_err(|status| ExecutionError {
        status,
        message: "execution backends require a Pro license".to_string(),
    })
}

fn allows_free_terminal_control(process: &ExecutionProcessRow) -> bool {
    process.kind == "terminalSession" && process.backend_id == "local"
}

impl IntoResponse for ExecutionError {
    fn into_response(self) -> axum::response::Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

fn execution_workspace_root(query: &ExecutionWorkspaceQuery) -> PathBuf {
    optional_workspace_root(query.workspace_root.as_deref())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

fn optional_workspace_root(value: Option<&str>) -> Option<PathBuf> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn execution_workspace_store_path(workspace_root: &std::path::Path) -> PathBuf {
    workspace_root
        .join(".coderight")
        .join("execution-workspaces.json")
}

fn execution_backend_default_store_path(workspace_root: &std::path::Path) -> PathBuf {
    workspace_root
        .join(".coderight")
        .join("execution-backends.json")
}

fn execution_process_store_path(workspace_root: &std::path::Path) -> PathBuf {
    workspace_root
        .join(".coderight")
        .join("execution-processes.json")
}

fn apply_execution_backend_default_override(
    workspace_root: &std::path::Path,
    registry: &mut rightkit_sandbox::execution::ExecutionBackendRegistry,
) -> Result<(), ExecutionError> {
    let Some(record) = load_execution_backend_settings(workspace_root)? else {
        return Ok(());
    };
    if registry.get(&record.default_backend).is_some() {
        registry.default_backend = record.default_backend;
    }
    for profile in record.network_profile_overrides {
        if let Some(existing) = registry
            .network_profiles
            .iter_mut()
            .find(|existing| existing.id == profile.id)
        {
            *existing = profile;
        } else {
            registry.network_profiles.push(profile);
        }
    }
    registry
        .network_profiles
        .sort_by(|left, right| left.id.cmp(&right.id));
    for override_record in record.backend_overrides {
        if let Some(backend) = registry
            .backends
            .iter_mut()
            .find(|backend| backend.id == override_record.id)
        {
            if let Some(enabled) = override_record.enabled {
                backend.enabled = enabled;
            }
            if let Some(resource_limits) = override_record.resource_limits {
                backend.resource_limits = resource_limits;
            }
            if let Some(network_profile) = override_record.network_profile {
                backend.network_profile = network_profile;
            }
        }
    }
    Ok(())
}

pub(crate) fn resolve_execution_backend_network_profile(
    workspace_root: &std::path::Path,
    base_registry: &rightkit_sandbox::execution::ExecutionBackendRegistry,
    backend_id: Option<&str>,
) -> Result<Option<String>, String> {
    let Some(backend_id) = backend_id.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let mut registry = base_registry.clone();
    apply_execution_backend_default_override(workspace_root, &mut registry)
        .map_err(|err| err.message)?;
    Ok(registry
        .get(backend_id)
        .and_then(|backend| normalize_network_profile(backend.network_profile.clone())))
}

pub(crate) fn resolve_execution_backend_resource_limits(
    workspace_root: &std::path::Path,
    base_registry: &rightkit_sandbox::execution::ExecutionBackendRegistry,
    backend_id: Option<&str>,
) -> Result<Option<rightkit_agent_tools::coding_tools::ExecutionResourceLimits>, String> {
    let Some(backend_id) = backend_id.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let mut registry = base_registry.clone();
    apply_execution_backend_default_override(workspace_root, &mut registry)
        .map_err(|err| err.message)?;
    Ok(registry.get(backend_id).map(|backend| {
        let limits = &backend.resource_limits;
        rightkit_agent_tools::coding_tools::ExecutionResourceLimits {
            cpu: limits.cpu,
            memory_mb: limits.memory_mb,
            disk_mb: limits.disk_mb,
        }
    }))
}

pub(crate) fn resolve_execution_backend_for_workspace(
    workspace_root: &std::path::Path,
    base_registry: &rightkit_sandbox::execution::ExecutionBackendRegistry,
    requested: Option<&str>,
) -> Result<Option<String>, String> {
    let mut registry = base_registry.clone();
    apply_execution_backend_default_override(workspace_root, &mut registry)
        .map_err(|err| err.message)?;
    let backend_id = requested
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(registry.default_backend.as_str());
    if backend_id.is_empty() {
        return Ok(None);
    }
    let Some(backend) = registry.get(backend_id) else {
        return Err("execution backend not found".to_string());
    };
    if !backend.enabled {
        return Err("execution backend is disabled".to_string());
    }
    Ok(Some(backend_id.to_string()))
}

pub(crate) fn resolve_execution_workspace_id_for_session(
    workspace_root: &std::path::Path,
    requested: Option<&str>,
    execution_backend: Option<&str>,
) -> Result<Option<String>, String> {
    let Some(raw_workspace_id) = requested.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let workspace_id = clean_workspace_id(Some(raw_workspace_id)).map_err(|err| err.message)?;
    let records = load_execution_workspaces(workspace_root).map_err(|err| err.message)?;
    let Some(record) = records.iter().find(|record| record.id == workspace_id) else {
        return Err("execution workspace not found".to_string());
    };
    if let Some(backend_id) = execution_backend
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if record.backend_id != backend_id {
            return Err("execution workspace belongs to a different backend".to_string());
        }
    }
    Ok(Some(workspace_id))
}

pub(crate) fn resolve_execution_workspace_path_for_session(
    workspace_root: &std::path::Path,
    requested: Option<&str>,
    execution_backend: Option<&str>,
) -> Result<Option<PathBuf>, String> {
    let Some(raw_workspace_id) = requested.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let workspace_id = clean_workspace_id(Some(raw_workspace_id)).map_err(|err| err.message)?;
    let records = load_execution_workspaces(workspace_root).map_err(|err| err.message)?;
    let Some(record) = records.iter().find(|record| record.id == workspace_id) else {
        return Err("execution workspace not found".to_string());
    };
    if let Some(backend_id) = execution_backend
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if record.backend_id != backend_id {
            return Err("execution workspace belongs to a different backend".to_string());
        }
    }
    execution_workspace_record_path(workspace_root, record).map(Some)
}
