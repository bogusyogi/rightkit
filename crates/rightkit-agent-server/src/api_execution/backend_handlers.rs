async fn list_execution_backends(
    State(state): State<DaemonState>,
    Query(query): Query<BackendRegistryQuery>,
) -> Result<Json<rightkit_sandbox::execution::ExecutionBackendRegistry>, ExecutionError> {
    let mut registry = lock_recover(&state.execution_backends, "state.execution_backends").clone();
    if let Some(root) = optional_workspace_root(query.workspace_root.as_deref()) {
        apply_execution_backend_default_override(&root, &mut registry)?;
    }
    Ok(Json(registry))
}

async fn set_execution_backend_default(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<BackendRegistryQuery>,
    Json(request): Json<SetExecutionBackendDefaultRequest>,
) -> Result<Json<rightkit_sandbox::execution::ExecutionBackendRegistry>, ExecutionError> {
    enforce_execution_trusted_source_authority(&headers)?;
    enforce_execution_backend_license(&state)?;
    let backend_id = request.backend_id.trim();
    if backend_id.is_empty() {
        return Err(ExecutionError::bad_request("backendId is required"));
    }
    let root = optional_workspace_root(query.workspace_root.as_deref())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let mut registry = lock_recover(&state.execution_backends, "state.execution_backends").clone();
    apply_execution_backend_default_override(&root, &mut registry)?;
    let backend = registry
        .get(backend_id)
        .ok_or_else(|| ExecutionError::not_found("execution backend not found"))?;
    if !backend.enabled {
        return Err(ExecutionError::conflict(
            "execution backend must be enabled before it can be default",
        ));
    }
    let mut record = load_execution_backend_settings(&root)?
        .unwrap_or_else(|| default_execution_backend_settings(&registry.default_backend));
    record.default_backend = backend_id.to_string();
    record.updated_at = now_iso();
    save_execution_backend_settings(&root, &record)?;
    registry.default_backend = backend_id.to_string();
    Ok(Json(registry))
}

async fn update_execution_backend(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<BackendRegistryQuery>,
    Json(request): Json<UpdateExecutionBackendRequest>,
) -> Result<Json<rightkit_sandbox::execution::ExecutionBackendRegistry>, ExecutionError> {
    enforce_execution_trusted_source_authority(&headers)?;
    enforce_execution_backend_license(&state)?;
    if request.enabled.is_none()
        && request.resource_limits.is_none()
        && request.network_profile.is_none()
    {
        return Err(ExecutionError::bad_request(
            "enabled, resourceLimits, or networkProfile is required",
        ));
    }
    if let Some(limits) = request.resource_limits.as_ref() {
        validate_resource_limits(limits)?;
    }
    let root = optional_workspace_root(query.workspace_root.as_deref())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let mut registry = lock_recover(&state.execution_backends, "state.execution_backends").clone();
    apply_execution_backend_default_override(&root, &mut registry)?;
    validate_network_profile(&registry, request.network_profile.as_ref())?;
    if registry.get(&id).is_none() {
        return Err(ExecutionError::not_found("execution backend not found"));
    }
    if request.enabled == Some(false) && registry.default_backend == id {
        return Err(ExecutionError::conflict(
            "default execution backend cannot be disabled",
        ));
    }
    let mut record = load_execution_backend_settings(&root)?
        .unwrap_or_else(|| default_execution_backend_settings(&registry.default_backend));
    upsert_execution_backend_override(&mut record, &id, &request);
    record.updated_at = now_iso();
    save_execution_backend_settings(&root, &record)?;
    if let Some(backend) = registry
        .backends
        .iter_mut()
        .find(|backend| backend.id == id)
    {
        if let Some(enabled) = request.enabled {
            backend.enabled = enabled;
        }
        if let Some(limits) = request.resource_limits {
            backend.resource_limits = limits;
        }
        if let Some(network_profile) = request.network_profile {
            backend.network_profile = network_profile.map(|value| value.trim().to_string());
        }
    }
    Ok(Json(registry))
}

async fn save_execution_network_profile(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<BackendRegistryQuery>,
    Json(request): Json<SaveExecutionNetworkProfileRequest>,
) -> Result<Json<rightkit_sandbox::execution::ExecutionBackendRegistry>, ExecutionError> {
    enforce_execution_trusted_source_authority(&headers)?;
    enforce_execution_backend_license(&state)?;
    let profile_id = clean_network_profile_id(&id)?;
    let allow_hosts = normalize_network_profile_hosts(request.allow_hosts.unwrap_or_default())?;
    let name = request
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(profile_id.as_str())
        .to_string();
    let profile = rightkit_sandbox::execution::NetworkProfile {
        id: profile_id.clone(),
        name,
        default: request
            .default
            .unwrap_or(rightkit_sandbox::execution::NetworkDefault::Deny),
        allow_hosts,
        allow_private_networks: request.allow_private_networks.unwrap_or(false),
        deny_private_networks: request.deny_private_networks.unwrap_or(true),
    };
    let root = optional_workspace_root(query.workspace_root.as_deref())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let mut registry = lock_recover(&state.execution_backends, "state.execution_backends").clone();
    apply_execution_backend_default_override(&root, &mut registry)?;
    let mut record = load_execution_backend_settings(&root)?
        .unwrap_or_else(|| default_execution_backend_settings(&registry.default_backend));
    upsert_execution_network_profile_override(&mut record, profile);
    record.updated_at = now_iso();
    save_execution_backend_settings(&root, &record)?;
    let mut registry = lock_recover(&state.execution_backends, "state.execution_backends").clone();
    apply_execution_backend_default_override(&root, &mut registry)?;
    Ok(Json(registry))
}

async fn delete_execution_network_profile(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<BackendRegistryQuery>,
) -> Result<Json<rightkit_sandbox::execution::ExecutionBackendRegistry>, ExecutionError> {
    enforce_execution_trusted_source_authority(&headers)?;
    enforce_execution_backend_license(&state)?;
    let profile_id = clean_network_profile_id(&id)?;
    let root = optional_workspace_root(query.workspace_root.as_deref())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let mut registry = lock_recover(&state.execution_backends, "state.execution_backends").clone();
    apply_execution_backend_default_override(&root, &mut registry)?;
    let Some(mut record) = load_execution_backend_settings(&root)? else {
        if registry
            .network_profiles
            .iter()
            .any(|profile| profile.id == profile_id)
        {
            return Err(ExecutionError::conflict(
                "built-in network profiles cannot be deleted",
            ));
        }
        return Ok(Json(registry));
    };
    let had_override = record
        .network_profile_overrides
        .iter()
        .any(|profile| profile.id == profile_id);
    if !had_override
        && registry
            .network_profiles
            .iter()
            .any(|profile| profile.id == profile_id)
    {
        return Err(ExecutionError::conflict(
            "built-in network profiles cannot be deleted",
        ));
    }
    if registry
        .backends
        .iter()
        .any(|backend| backend.network_profile.as_deref() == Some(profile_id.as_str()))
    {
        return Err(ExecutionError::conflict(
            "network profile is still assigned to an execution backend",
        ));
    }
    record
        .network_profile_overrides
        .retain(|profile| profile.id != profile_id);
    record.updated_at = now_iso();
    save_execution_backend_settings(&root, &record)?;
    let mut registry = lock_recover(&state.execution_backends, "state.execution_backends").clone();
    apply_execution_backend_default_override(&root, &mut registry)?;
    Ok(Json(registry))
}
