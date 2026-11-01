//! Agent runtime lifecycle handlers (native CLI-executor sessions).
//!
//! Decomposed out of `api_drivers.rs` (ra-016): this module owns the
//! open/prompt/interrupt/resume/recover/stop/status handlers for the native
//! `AgentRuntimeExecutor`-backed agent runtime. `api_drivers.rs` re-exports
//! everything here (`pub(crate) use agent_runtime::*;`) and keeps calling
//! these functions unqualified from its route table exactly as before the
//! split, so route paths, wire formats, and authorization boundaries are
//! unchanged — only the item's source file moved.

use super::*;

pub(crate) async fn agent_binding_id(
    state: &DaemonState,
    session_id: &str,
    agent_id: &str,
) -> Result<String, StatusCode> {
    if agent_id.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let storage = state
        .storage
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    storage
        .lock()
        .await
        .list_driver_bindings_for_session(session_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_iter()
        .find(|binding| binding.coderight_agent_id.as_deref() == Some(agent_id))
        .map(|binding| binding.id)
        .ok_or(StatusCode::NOT_FOUND)
}

pub(crate) async fn open_agent_runtime(
    State(state): State<DaemonState>,
    Path((session_id, agent_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<OpenAgentRuntimeRequest>,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let _authority = state.driver_authority_lock.lock().await;
    if body.runtime.kind != "authenticated_cli"
        || agent_id.trim().is_empty()
        || body.role.trim().is_empty()
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let live = state
        .get_session(&session_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let provider_id = rightkit_agents::driver_host::cli_runtime::CliProviderId::new(
        body.runtime.provider_id.clone(),
    );
    let adapter = state
        .cli_registry
        .adapter(&provider_id)
        .ok_or(StatusCode::BAD_REQUEST)?;
    let profile = launchable_cli_profile(&state, provider_id.clone()).await?;
    if profile.id.as_str() != body.runtime.account_profile_id {
        return Err(StatusCode::CONFLICT);
    }
    let resolved = resolve_cli_launch_route(
        &state,
        &provider_id,
        profile,
        body.runtime.model_id.as_deref(),
    )
    .await
    .map_err(|_| StatusCode::CONFLICT)?;
    let profile = resolved.launch_profile.clone();
    let discovery = adapter
        .discover(&profile)
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let executable = discovery.executable.as_ref().ok_or(StatusCode::CONFLICT)?;
    let digest = discovery
        .executable_sha256
        .as_deref()
        .ok_or(StatusCode::CONFLICT)?;
    if !state.cli_registry.is_ready(&provider_id, digest) {
        return Err(StatusCode::CONFLICT);
    }
    let matched = matching_route_attestation(
        &state,
        &provider_id,
        &discovery,
        adapter.as_ref(),
        &resolved,
    )
    .await
    .map_err(|_| StatusCode::CONFLICT)?;
    let storage = state
        .storage
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let attestation = matched.row;
    let tools = effective_ceiling(&live, &body.tool_ceiling)?;
    let binding_id = format!("binding-{agent_id}");
    if runtime_contains_binding(&state, &binding_id).await {
        return Err(StatusCode::CONFLICT);
    }
    let lease_id = format!("lease-{}", crate::startup::generate_token());
    let nonce = crate::startup::generate_token();
    let secret = format!(
        "{}{}",
        crate::startup::generate_token(),
        crate::startup::generate_token()
    );
    let capability_token = format!("{lease_id}.{nonce}.{secret}");
    let process_instance_secret = format!(
        "{}{}",
        crate::startup::generate_token(),
        crate::startup::generate_token()
    );
    let created_at = unix_key(0);
    let workspace = canonical_workspace(&live.workspace_root)?;
    let binding = rightkit_agent_engine_storage::DriverBindingRow {
        id: binding_id.clone(),
        driver_kind: provider_id.as_str().to_string(),
        session_id: session_id.clone(),
        vendor_session_id: None,
        vendor_thread_id: None,
        vendor_turn_id: None,
        binary_version: discovery.binary_version.clone().unwrap_or_default(),
        schema_hash: attestation.tool_schema_hash.clone(),
        status: "starting".to_string(),
        last_event_key: None,
        last_event_cursor: None,
        coderight_agent_id: Some(agent_id.clone()),
        parent_agent_id: body.parent_agent_id.clone().unwrap_or_default(),
        provider_id: Some(provider_id.as_str().to_string()),
        transport_kind: Some(adapter.transport_kind().label().to_string()),
        account_profile_id: Some(profile.id.as_str().to_string()),
        auth_mode: Some(profile.auth_mode.label().to_string()),
        account_fingerprint: profile.account_fingerprint.clone(),
        model_id: resolved.model_id.clone(),
        executable_path: Some(executable.to_string_lossy().into_owned()),
        executable_sha256: Some(digest.to_string()),
        protocol_version: Some(attestation.protocol_version.clone()),
        profile_hash: Some(resolved.effective_profile_hash.clone()),
        policy_hash: Some(attestation.policy_hash.clone()),
        tool_schema_hash: Some(attestation.tool_schema_hash.clone()),
        harness_attestation_id: Some(attestation.id.clone()),
        isolation_mode: Some(attestation.isolation_mode.clone()),
        resume_strategy: None,
        context_epoch: 0,
        process_generation: 1,
        created_at: created_at.clone(),
        updated_at: created_at.clone(),
    };
    let lease = rightkit_agent_engine_storage::SessionCapabilityLeaseRow {
        id: lease_id,
        binding_id: binding_id.clone(),
        token_hash: token_hash(&capability_token),
        nonce,
        session_id: session_id.clone(),
        canonical_workspace: workspace.to_string_lossy().into_owned(),
        tool_ceiling_json: serde_json::to_string(&tools)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        created_at: created_at.clone(),
        expires_at: unix_key(DEFAULT_LEASE_TTL_SECONDS),
        revoked_at: None,
        process_instance_hash: Some(token_hash(&process_instance_secret)),
    };
    {
        let store = storage.lock().await;
        store
            .upsert_driver_binding(&binding)
            .map_err(|_| StatusCode::CONFLICT)?;
        if let Err(error) = store.upsert_session_capability_lease(&lease) {
            let _ = store.delete_driver_binding(&binding_id);
            tracing::warn!(%error, %binding_id, "failed to persist runtime lease");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }
    let current_exe = std::env::current_exe().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let context = rightkit_agents::driver_host::cli_runtime::CliExecutorLaunchContext {
        executable: executable.clone(),
        profile: profile.clone(),
        mcp_server: rightkit_agents::driver_host::cli_runtime::CliMcpServerSpec {
            name: "coderight".to_string(),
            command: current_exe,
            args: vec![
                "mcp-server".to_string(),
                "--session-id".to_string(),
                session_id.clone(),
            ],
            env: vec![
                (
                    "RIGHTKIT_DAEMON_URL".to_string(),
                    format!("http://127.0.0.1:{}", state.bound_port),
                ),
                ("RIGHTKIT_GATEWAY_TOKEN".to_string(), capability_token),
                (
                    "RIGHTKIT_DRIVER_INSTANCE_SECRET".to_string(),
                    process_instance_secret,
                ),
            ],
        },
        policy: rightkit_agents::driver_host::RUN_SCOPED_POLICY.to_string(),
        attestation_inputs: matched.inputs,
    };
    let launch_contexts = state
        .cli_launch_contexts
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    launch_contexts
        .insert(
            binding_id.clone(),
            session_id.clone(),
            agent_id.clone(),
            context,
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let executor = state
        .cli_executor
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let agent_kind = if body.parent_agent_id.is_some() {
        rightkit_agents::driver_host::cli_runtime::AgentKind::Child
    } else {
        rightkit_agents::driver_host::cli_runtime::AgentKind::Root
    };
    let handle = match executor
        .open_agent(rightkit_agents::driver_host::cli_runtime::OpenAgentRequest {
            session_id: session_id.clone(),
            agent_id: agent_id.clone(),
            role: body.role,
            parent_agent_id: body.parent_agent_id,
            backend: rightkit_agents::driver_host::cli_runtime::AgentRuntimeBackend::AuthenticatedCli {
                agent_kind,
                provider_id: provider_id.clone(),
                model_id: resolved.model_id.clone(),
                account_profile_id: profile.id.clone(),
            },
            tool_ceiling: tools,
            worktree_id: body.worktree_id,
        })
        .await
    {
        Ok(handle) => handle,
        Err(error) => {
            launch_contexts.remove(&binding_id);
            let store = storage.lock().await;
            let _ = store.revoke_session_capability_lease(&lease.id, &unix_key(0));
            let _ = store.delete_driver_binding(&binding_id);
            tracing::warn!(%error, %binding_id, "authenticated CLI open failed");
            return Err(StatusCode::BAD_GATEWAY);
        }
    };
    let mut connected = binding;
    connected.vendor_session_id = handle.vendor_session_id.clone();
    connected.status = "connected".to_string();
    connected.updated_at = unix_key(0);
    state
        .cli_recovery_live_generations
        .set(&binding_id, connected.process_generation);
    storage
        .lock()
        .await
        .upsert_driver_binding(&connected)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({
        "agentId": agent_id,
        "bindingId": binding_id,
        "providerId": provider_id.as_str(),
        "modelId": body.runtime.model_id,
        "vendorSessionId": handle.vendor_session_id,
        "lifecycle": "connected",
    })))
}

pub(crate) async fn prompt_agent_runtime(
    State(state): State<DaemonState>,
    Path((session_id, agent_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<DriverPromptRequest>,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let binding_id = agent_binding_id(&state, &session_id, &agent_id).await?;
    let executor = state
        .cli_executor
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let handle = executor
        .handle_for(&agent_id)
        .map_err(|_| StatusCode::CONFLICT)?;
    let result = executor
        .run_turn(
            &handle,
            rightkit_agents::driver_host::cli_runtime::AgentTurnRequest {
                agent_kind: handle.backend.agent_kind(),
                prompt: body.text,
                continuation_token: None,
            },
        )
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let current = executor
        .handle_for(&agent_id)
        .map_err(|_| StatusCode::CONFLICT)?;
    if let Some(storage) = state.storage.as_ref() {
        let mut binding = storage
            .lock()
            .await
            .get_driver_binding(&binding_id)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .ok_or(StatusCode::NOT_FOUND)?;
        binding.vendor_session_id = current.vendor_session_id.clone();
        binding.vendor_turn_id = current.vendor_turn_id.clone();
        binding.status = match result.status {
            rightkit_agents::driver_host::cli_runtime::AgentTurnStatus::Completed => "completed",
            rightkit_agents::driver_host::cli_runtime::AgentTurnStatus::Interrupted => {
                "interrupted"
            }
            rightkit_agents::driver_host::cli_runtime::AgentTurnStatus::WaitingForApproval => {
                "waiting_for_approval"
            }
            rightkit_agents::driver_host::cli_runtime::AgentTurnStatus::WaitingForChild => {
                "waiting_for_child"
            }
            rightkit_agents::driver_host::cli_runtime::AgentTurnStatus::Failed => "failed",
        }
        .to_string();
        binding.updated_at = unix_key(0);
        storage
            .lock()
            .await
            .upsert_driver_binding(&binding)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    Ok(Json(json!({
        "agentId": agent_id,
        "bindingId": binding_id,
        "status": result.status,
        "finalText": result.final_text,
        "vendorSessionId": current.vendor_session_id,
        "vendorTurnId": current.vendor_turn_id,
    })))
}

pub(crate) async fn interrupt_agent_runtime(
    State(state): State<DaemonState>,
    Path((session_id, agent_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(_body): Json<DriverInterruptRequest>,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    agent_binding_id(&state, &session_id, &agent_id).await?;
    let executor = state
        .cli_executor
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let handle = executor
        .handle_for(&agent_id)
        .map_err(|_| StatusCode::CONFLICT)?;
    executor
        .interrupt(&handle)
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    Ok(Json(json!({"interrupted": true, "agentId": agent_id})))
}

pub(crate) async fn resume_agent_runtime(
    State(state): State<DaemonState>,
    Path((session_id, agent_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(_body): Json<DriverBindingRequest>,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let _authority = state.driver_authority_lock.lock().await;
    let binding_id = agent_binding_id(&state, &session_id, &agent_id).await?;
    let executor = state
        .cli_executor
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let handle = match executor.handle_for(&agent_id) {
        Ok(handle) => handle,
        Err(_) => {
            recover_missing_agent_runtime(&state, &session_id, &agent_id, &binding_id).await?
        }
    };
    let resumed = executor
        .resume(&handle)
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    Ok(Json(json!({
        "resumed": true,
        "agentId": agent_id,
        "vendorSessionId": resumed.vendor_session_id,
    })))
}

/// Rebuild only the volatile pieces of a persisted CLI binding. Launch secrets
/// are deliberately not durable, so recovery rotates them into a fresh lease
/// after validating every durable identity and attestation input.
pub(crate) async fn recover_missing_agent_runtime(
    state: &DaemonState,
    session_id: &str,
    agent_id: &str,
    binding_id: &str,
) -> Result<rightkit_agents::driver_host::cli_runtime::AgentRuntimeHandle, StatusCode> {
    let live = state.get_session(session_id).ok_or(StatusCode::NOT_FOUND)?;
    let storage = state
        .storage
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let launch_contexts = state
        .cli_launch_contexts
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let executor = state
        .cli_executor
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let (binding, profile_row, attestation, previous_lease) = {
        let store = storage.lock().await;
        let binding = store
            .get_driver_binding(binding_id)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .filter(|binding| {
                binding.session_id == session_id
                    && binding.coderight_agent_id.as_deref() == Some(agent_id)
            })
            .ok_or(StatusCode::NOT_FOUND)?;
        let profile_id = binding
            .account_profile_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .ok_or(StatusCode::CONFLICT)?
            .to_string();
        let attestation_id = binding
            .harness_attestation_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .ok_or(StatusCode::CONFLICT)?
            .to_string();
        (
            binding,
            store
                .get_cli_account_profile(&profile_id)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            store
                .get_cli_harness_attestation(&attestation_id)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
                .ok_or(StatusCode::CONFLICT)?,
            store
                .get_latest_session_capability_lease_for_binding(binding_id)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
                .ok_or(StatusCode::CONFLICT)?,
        )
    };
    let provider_id = rightkit_agents::driver_host::cli_runtime::CliProviderId::new(
        binding
            .provider_id
            .clone()
            .unwrap_or_else(|| binding.driver_kind.clone()),
    );
    let adapter = state
        .cli_registry
        .adapter(&provider_id)
        .ok_or(StatusCode::CONFLICT)?;
    if binding.status == "completed" || binding.status == "failed" {
        return Err(StatusCode::CONFLICT);
    }
    let default_profile = default_cli_profile(provider_id.clone());
    let profile = match profile_row {
        Some(profile_row) => rightkit_agents::driver_host::cli_runtime::CliAccountProfile {
            id: rightkit_agents::driver_host::cli_runtime::CliAccountProfileId::new(profile_row.id),
            provider_id: provider_id.clone(),
            label: profile_row.label,
            managed_home: PathBuf::from(profile_row.managed_home),
            auth_mode: cli_auth_mode(&profile_row.auth_mode),
            signed_in: profile_row.signed_in,
            account_fingerprint: profile_row.account_fingerprint,
            last_verified_at: profile_row.last_verified_at,
        },
        None if missing_default_profile_is_allowed_current(
            &provider_id,
            binding.account_profile_id.as_deref().unwrap_or_default(),
        )
        .map_err(|_| StatusCode::CONFLICT)? =>
        {
            default_profile
        }
        None => return Err(StatusCode::CONFLICT),
    };
    let resolved =
        resolve_cli_launch_route(state, &provider_id, profile, binding.model_id.as_deref())
            .await
            .map_err(|_| StatusCode::CONFLICT)?;
    let profile = resolved.launch_profile.clone();
    if binding.account_profile_id.as_deref() != Some(profile.id.as_str())
        || binding.auth_mode.as_deref() != Some(profile.auth_mode.label())
        || binding.account_fingerprint != profile.account_fingerprint
        || !profile.managed_home.is_absolute()
        || !profile.managed_home.is_dir()
    {
        return Err(StatusCode::CONFLICT);
    }
    let discovery = adapter
        .discover(&profile)
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let executable = discovery.executable.as_ref().ok_or(StatusCode::CONFLICT)?;
    let digest = discovery
        .executable_sha256
        .as_deref()
        .ok_or(StatusCode::CONFLICT)?;
    let matched =
        matching_route_attestation(state, &provider_id, &discovery, adapter.as_ref(), &resolved)
            .await
            .map_err(|_| StatusCode::CONFLICT)?;
    if binding.executable_path.as_deref() != Some(executable.to_string_lossy().as_ref())
        || binding.executable_sha256.as_deref() != Some(digest)
        || binding.binary_version != discovery.binary_version.clone().unwrap_or_default()
        || !state.cli_registry.is_ready(&provider_id, digest)
        || attestation.result != "passed"
        || attestation.id != matched.row.id
        || attestation.provider_id != provider_id.as_str()
        || attestation.executable_sha256 != digest
        || attestation.binary_version != binding.binary_version
        || binding.protocol_version.as_deref() != Some(attestation.protocol_version.as_str())
        || !binding_route_tuple_matches(&binding, &resolved)
        || binding.policy_hash.as_deref() != Some(attestation.policy_hash.as_str())
        || binding.tool_schema_hash.as_deref() != Some(attestation.tool_schema_hash.as_str())
        || binding.isolation_mode.as_deref() != Some(attestation.isolation_mode.as_str())
        || binding.model_id != resolved.model_id
    {
        return Err(StatusCode::CONFLICT);
    }
    let vendor_session_id = binding
        .vendor_session_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .ok_or(StatusCode::CONFLICT)?
        .to_string();
    let workspace = canonical_workspace(&live.workspace_root)?;
    if previous_lease.session_id != session_id
        || previous_lease.binding_id != binding_id
        || previous_lease.canonical_workspace != workspace.to_string_lossy().as_ref()
    {
        return Err(StatusCode::CONFLICT);
    }
    let stored_tools: Vec<String> = serde_json::from_str(&previous_lease.tool_ceiling_json)
        .map_err(|_| StatusCode::CONFLICT)?;
    let tools = effective_ceiling(&live, &stored_tools)?;
    let lease_id = format!("lease-{}", crate::startup::generate_token());
    let nonce = crate::startup::generate_token();
    let secret = format!(
        "{}{}",
        crate::startup::generate_token(),
        crate::startup::generate_token()
    );
    let capability_token = format!("{lease_id}.{nonce}.{secret}");
    let process_instance_secret = format!(
        "{}{}",
        crate::startup::generate_token(),
        crate::startup::generate_token()
    );
    let created_at = unix_key(0);
    let lease = rightkit_agent_engine_storage::SessionCapabilityLeaseRow {
        id: lease_id.clone(),
        binding_id: binding_id.to_string(),
        token_hash: token_hash(&capability_token),
        nonce,
        session_id: session_id.to_string(),
        canonical_workspace: workspace.to_string_lossy().into_owned(),
        tool_ceiling_json: serde_json::to_string(&tools)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        created_at: created_at.clone(),
        expires_at: unix_key(DEFAULT_LEASE_TTL_SECONDS),
        revoked_at: None,
        process_instance_hash: Some(token_hash(&process_instance_secret)),
    };
    let mut recovering = binding.clone();
    recovering.status = "resuming".to_string();
    recovering.updated_at = created_at.clone();
    {
        let store = storage.lock().await;
        store
            .revoke_session_capability_lease(&previous_lease.id, &created_at)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        if let Err(error) = store.upsert_session_capability_lease(&lease) {
            tracing::warn!(%error, %binding_id, "failed to rotate recovered runtime lease");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
        if let Err(error) = store.upsert_driver_binding(&recovering) {
            let _ = store.revoke_session_capability_lease(&lease_id, &unix_key(0));
            tracing::warn!(%error, %binding_id, "failed to mark recovered runtime binding");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }
    let current_exe = std::env::current_exe().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let context = rightkit_agents::driver_host::cli_runtime::CliExecutorLaunchContext {
        executable: executable.clone(),
        profile: profile.clone(),
        mcp_server: rightkit_agents::driver_host::cli_runtime::CliMcpServerSpec {
            name: "coderight".to_string(),
            command: current_exe,
            args: vec![
                "mcp-server".to_string(),
                "--session-id".to_string(),
                session_id.to_string(),
            ],
            env: vec![
                (
                    "RIGHTKIT_DAEMON_URL".to_string(),
                    format!("http://127.0.0.1:{}", state.bound_port),
                ),
                ("RIGHTKIT_GATEWAY_TOKEN".to_string(), capability_token),
                (
                    "RIGHTKIT_DRIVER_INSTANCE_SECRET".to_string(),
                    process_instance_secret,
                ),
            ],
        },
        policy: rightkit_agents::driver_host::RUN_SCOPED_POLICY.to_string(),
        attestation_inputs: matched.inputs,
    };
    if launch_contexts
        .insert(
            binding_id.to_string(),
            session_id.to_string(),
            agent_id.to_string(),
            context,
        )
        .is_err()
    {
        let store = storage.lock().await;
        let _ = store.revoke_session_capability_lease(&lease_id, &unix_key(0));
        let _ = store.upsert_driver_binding(&binding);
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    state
        .cli_recovery_live_generations
        .set(binding_id, recovering.process_generation);
    let agent_kind = if binding.parent_agent_id.is_empty() {
        rightkit_agents::driver_host::cli_runtime::AgentKind::Root
    } else {
        rightkit_agents::driver_host::cli_runtime::AgentKind::Child
    };
    let recovered = executor
        .recover_agent(
            rightkit_agents::driver_host::cli_runtime::OpenAgentRequest {
                session_id: session_id.to_string(),
                agent_id: agent_id.to_string(),
                role: "recovered".to_string(),
                parent_agent_id: (!binding.parent_agent_id.is_empty())
                    .then_some(binding.parent_agent_id.clone()),
                backend:
                    rightkit_agents::driver_host::cli_runtime::AgentRuntimeBackend::AuthenticatedCli {
                        agent_kind,
                        provider_id,
                        model_id: binding.model_id.clone(),
                        account_profile_id: profile.id,
                    },
                tool_ceiling: tools,
                worktree_id: Some(workspace.to_string_lossy().into_owned()),
            },
            &vendor_session_id,
        )
        .await;
    match recovered {
        Ok(handle) => Ok(handle),
        Err(error) => {
            launch_contexts.remove(binding_id);
            let mut failed = binding;
            failed.status = "recovery_required".to_string();
            failed.updated_at = unix_key(0);
            let store = storage.lock().await;
            let _ = store.revoke_session_capability_lease(&lease_id, &failed.updated_at);
            let _ = store.upsert_driver_binding(&failed);
            tracing::warn!(%error, %binding_id, "authenticated CLI recovery failed");
            Err(StatusCode::BAD_GATEWAY)
        }
    }
}

pub(crate) async fn stop_agent_runtime(
    State(state): State<DaemonState>,
    Path((session_id, agent_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(_body): Json<DriverBindingRequest>,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let _authority = state.driver_authority_lock.lock().await;
    let binding_id = agent_binding_id(&state, &session_id, &agent_id).await?;
    let executor = state
        .cli_executor
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    executor.shutdown_agent(&agent_id).await.map_err(|error| {
        tracing::warn!(
            session_id = %session_id,
            agent_id = %agent_id,
            binding_id = %binding_id,
            error = %error,
            "agent runtime shutdown failed; the driver process may still be running"
        );
        StatusCode::BAD_GATEWAY
    })?;
    if let Some(contexts) = state.cli_launch_contexts.as_ref() {
        contexts.remove(&binding_id);
    }
    if let Some(storage) = state.storage.as_ref() {
        let at = unix_key(0);
        let store = storage.lock().await;
        store
            .revoke_active_session_capability_leases_for_session(&session_id, &at)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        if let Some(mut binding) = store
            .get_driver_binding(&binding_id)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        {
            binding.status = "completed".to_string();
            binding.updated_at = at;
            store
                .upsert_driver_binding(&binding)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
    }
    Ok(Json(json!({"stopped": true, "agentId": agent_id})))
}

pub(crate) async fn agent_runtime_status(
    State(state): State<DaemonState>,
    Path((session_id, agent_id)): Path<(String, String)>,
) -> Result<Json<Value>, StatusCode> {
    let binding_id = agent_binding_id(&state, &session_id, &agent_id).await?;
    let storage = state
        .storage
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let binding = storage
        .lock()
        .await
        .get_driver_binding(&binding_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let provider_id = binding
        .provider_id
        .clone()
        .unwrap_or_else(|| binding.driver_kind.clone());
    let provider =
        rightkit_agents::driver_host::cli_runtime::CliProviderId::new(provider_id.clone());
    let harness_eligibility = match (
        binding.executable_sha256.as_deref(),
        state.cli_registry.adapter(&provider),
    ) {
        (Some(digest), Some(_)) if state.cli_registry.is_ready(&provider, digest) => "ready",
        _ => "harness_verification_required",
    };
    Ok(Json(json!({
        "agentId": agent_id,
        "providerId": provider_id,
        "modelId": binding.model_id,
        "vendorSessionId": binding.vendor_session_id,
        "vendorTurnId": binding.vendor_turn_id,
        "lifecycle": driver_lifecycle(&binding.status),
        "process": {"generation": binding.process_generation, "pid": Value::Null, "connected": runtime_contains_binding(&state, &binding_id).await},
        "auth": {"mode": binding.auth_mode},
        "harness": {"eligibility": harness_eligibility, "isolationMode": binding.isolation_mode},
        "contextEpoch": binding.context_epoch,
        "diagnostic": driver_diagnostic(&binding.status),
    })))
}
