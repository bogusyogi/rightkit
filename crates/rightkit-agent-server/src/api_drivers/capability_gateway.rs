//! Capability leases and the governed tool gateway.
//!
//! Decomposed out of `api_drivers.rs` (ra-016): this module owns capability
//! lease issuance, capability listing/calling, the governed tool-call
//! dispatcher (`memory_recall`/`delegate_task`/`delegate_parallel`/`verify`),
//! lease authorization, and the capability JSON schemas. `api_drivers.rs`
//! re-exports everything here (`pub(crate) use capability_gateway::*;`) and
//! keeps calling these functions unqualified exactly as before the split, so
//! route paths, wire formats, lease headers, and authorization boundaries
//! are unchanged — only the item's source file moved.

use super::*;

pub(crate) async fn issue_capability_lease(
    State(state): State<DaemonState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<IssueLeaseRequest>,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let _authority = state.driver_authority_lock.lock().await;
    let live = state
        .get_session(&session_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let driver_kind = body.driver_kind.trim().to_ascii_lowercase();
    let provider_id =
        rightkit_agents::driver_host::cli_runtime::CliProviderId::new(driver_kind.clone());
    if state.cli_registry.adapter(&provider_id).is_none()
        || body.binary_version.trim().is_empty()
        || body.schema_hash.trim().is_empty()
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let profile = launchable_cli_profile(&state, provider_id.clone()).await?;
    let adapter = state
        .cli_registry
        .adapter(&provider_id)
        .ok_or(StatusCode::BAD_REQUEST)?;
    let discovery = adapter
        .discover(&profile)
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let executable_sha256 = discovery
        .executable_sha256
        .as_deref()
        .ok_or(StatusCode::CONFLICT)?;
    if !state.cli_registry.is_ready(&provider_id, executable_sha256) {
        return Err(StatusCode::CONFLICT);
    }
    if *lock_recover(&live.driver_kind, "session.driver_kind") != driver_kind {
        return Err(StatusCode::CONFLICT);
    }
    let storage = state
        .storage
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let workspace = canonical_workspace(&live.workspace_root)?;
    let tools = effective_ceiling(&live, &body.tools)?;
    let ttl = body
        .ttl_seconds
        .unwrap_or(DEFAULT_LEASE_TTL_SECONDS)
        .clamp(1, MAX_LEASE_TTL_SECONDS);
    let lease_id = format!("lease-{}", crate::startup::generate_token());
    let existing_binding = storage
        .lock()
        .await
        .get_driver_binding_for_session(&driver_kind, &session_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let binding_id = existing_binding
        .as_ref()
        .map(|binding| binding.id.clone())
        .unwrap_or_else(|| format!("binding-{lease_id}"));
    let harness_attestation_id = storage
        .lock()
        .await
        .latest_cli_harness_attestation(provider_id.as_str())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(|row| row.id)
        .ok_or(StatusCode::CONFLICT)?;
    if runtime_contains_binding(&state, &binding_id).await {
        return Err(StatusCode::CONFLICT);
    }
    let nonce = crate::startup::generate_token();
    let secret = format!(
        "{}{}",
        crate::startup::generate_token(),
        crate::startup::generate_token()
    );
    let plaintext_token = format!("{lease_id}.{nonce}.{secret}");
    let token_hash = token_hash(&plaintext_token);
    let created_at = unix_key(0);
    let expires_at = unix_key(ttl);
    let binding = if let Some(mut binding) = existing_binding.clone() {
        binding.binary_version = body.binary_version;
        binding.schema_hash = body.schema_hash;
        binding.host_agent_id = body
            .agent_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .or(binding.host_agent_id);
        binding.account_profile_id = Some(profile.id.as_str().to_string());
        binding.auth_mode = Some(profile.auth_mode.label().to_string());
        binding.account_fingerprint = profile.account_fingerprint.clone();
        binding.executable_path = discovery
            .executable
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());
        binding.executable_sha256 = Some(executable_sha256.to_string());
        binding.harness_attestation_id = Some(harness_attestation_id.clone());
        binding.isolation_mode = Some(adapter.capabilities().isolation_mode.label().to_string());
        if binding.status != "recovery_required" {
            binding.status = "leased".to_string();
        }
        binding.updated_at = created_at.clone();
        binding
    } else {
        rightkit_agent_engine_storage::DriverBindingRow {
            id: binding_id.clone(),
            driver_kind: driver_kind.clone(),
            session_id: session_id.clone(),
            vendor_session_id: None,
            vendor_thread_id: None,
            vendor_turn_id: None,
            binary_version: body.binary_version,
            schema_hash: body.schema_hash,
            status: "leased".to_string(),
            last_event_key: None,
            last_event_cursor: None,
            host_agent_id: body
                .agent_id
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string)
                .or_else(|| Some(session_id.clone())),
            parent_agent_id: String::new(),
            provider_id: Some(provider_id.as_str().to_string()),
            transport_kind: Some(adapter.transport_kind().label().to_string()),
            account_profile_id: Some(profile.id.as_str().to_string()),
            auth_mode: Some(profile.auth_mode.label().to_string()),
            account_fingerprint: profile.account_fingerprint.clone(),
            model_id: None,
            executable_path: discovery
                .executable
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            executable_sha256: Some(executable_sha256.to_string()),
            protocol_version: None,
            profile_hash: None,
            policy_hash: None,
            tool_schema_hash: None,
            harness_attestation_id: Some(harness_attestation_id),
            isolation_mode: Some(adapter.capabilities().isolation_mode.label().to_string()),
            resume_strategy: None,
            context_epoch: 0,
            process_generation: 0,
            created_at: created_at.clone(),
            updated_at: created_at.clone(),
        }
    };
    let lease = rightkit_agent_engine_storage::SessionCapabilityLeaseRow {
        id: lease_id.clone(),
        binding_id: binding_id.clone(),
        token_hash,
        nonce,
        session_id: session_id.clone(),
        canonical_workspace: workspace.to_string_lossy().into_owned(),
        tool_ceiling_json: serde_json::to_string(&tools).map_err(|_| StatusCode::BAD_REQUEST)?,
        created_at: created_at.clone(),
        expires_at: expires_at.clone(),
        revoked_at: None,
        process_instance_hash: None,
    };
    {
        let store = storage.lock().await;
        store
            .upsert_driver_binding(&binding)
            .map_err(|_| StatusCode::CONFLICT)?;
        store
            .revoke_active_session_capability_leases_for_session(&session_id, &created_at)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        if let Err(error) = store.upsert_session_capability_lease(&lease) {
            if let Some(previous) = existing_binding.as_ref() {
                let _ = store.upsert_driver_binding(previous);
            } else {
                let _ = store.delete_driver_binding(&binding_id);
            }
            tracing::warn!(session_id, error = %error, "failed to persist driver capability lease");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }
    Ok(Json(json!({
        "leaseId": lease_id,
        "bindingId": binding_id,
        "sessionId": session_id,
        "driverKind": driver_kind,
        "expiresAt": expires_at,
        "tools": tools,
        "capabilityToken": plaintext_token,
        "mcp": {
            "command": crate::host_identity::app(),
            "args": ["mcp-server", "--session-id", session_id],
            "secretEnvironment": DRIVER_MCP_SECRET_ENVIRONMENT,
        }
    })))
}

pub(crate) async fn list_capabilities(
    State(state): State<DaemonState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    let (lease, tools, _) = authorize_gateway_lease(&state, &session_id, &headers).await?;
    if !runtime_contains_binding(&state, &lease.binding_id).await {
        return Err(StatusCode::GONE);
    }
    let registry = ToolRegistry::build_default();
    let schemas = tools
        .iter()
        .filter_map(|name| capability_schema(&registry, name))
        .collect::<Vec<_>>();
    Ok(Json(json!({ "tools": schemas })))
}

pub(crate) async fn call_capability(
    State(state): State<DaemonState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<CapabilityCallRequest>,
) -> Result<Json<Value>, StatusCode> {
    let (lease, tools, _instance_key) =
        authorize_gateway_lease(&state, &session_id, &headers).await?;
    if !runtime_contains_binding(&state, &lease.binding_id).await {
        return Err(StatusCode::GONE);
    }
    if !tools.iter().any(|tool| tool == &body.name) {
        return Err(StatusCode::FORBIDDEN);
    }
    let call_id = match body.call_id {
        Value::String(value) if !value.trim().is_empty() => value,
        Value::Number(value) => value.to_string(),
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    let binding_id = lease.binding_id.clone();
    let event_key = format!("mcp:{binding_id}:{call_id}");
    let inbox = rightkit_agent_engine_storage::DriverEventInboxRow {
        binding_id: binding_id.clone(),
        event_key: event_key.clone(),
        event_cursor: None,
        event_kind: format!("tool.call.{}", body.name),
        payload_json: serde_json::to_string(&body.arguments)
            .map_err(|_| StatusCode::BAD_REQUEST)?,
        status: "claimed".to_string(),
        received_at: unix_key(0),
        processed_at: None,
        result_json: None,
    };
    let storage = state
        .storage
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    {
        let mut store = storage.lock().await;
        match store
            .claim_driver_event(&inbox)
            .map_err(|_| StatusCode::CONFLICT)?
        {
            rightkit_agent_engine_storage::DriverEventClaim::Claimed => {}
            rightkit_agent_engine_storage::DriverEventClaim::Duplicate(existing) => {
                if existing.event_kind != format!("tool.call.{}", body.name)
                    || existing.payload_json != inbox.payload_json
                {
                    return Err(StatusCode::CONFLICT);
                }
                if let Some(result) = existing
                    .result_json
                    .as_deref()
                    .and_then(|value| serde_json::from_str::<Value>(value).ok())
                {
                    return Ok(Json(result));
                }
                return Ok(Json(json!({
                    "content": [{"type": "text", "text": "duplicate driver tool call has no committed result; RightKit refused to re-run the side effect"}],
                    "isError": true,
                    "_meta": {"duplicate": true, "callId": call_id, "priorStatus": existing.status},
                })));
            }
        }
    }

    let result = match body.name.as_str() {
        "memory_recall" => memory_recall(&state, &session_id, &body.arguments).await?,
        "delegate_task" => delegate_task(&state, &session_id, &body.arguments).await,
        "delegate_parallel" => delegate_parallel(&state, &session_id, &body.arguments).await,
        "verify" => {
            mark_driver_lifecycle(&state, &binding_id, "waiting_for_approval").await;
            let result = if body
                .arguments
                .get("run_in_background")
                .and_then(Value::as_bool)
                == Some(true)
            {
                ToolResult::error("verification commands cannot run in the background")
            } else {
                governed_tool_call(
                    &state,
                    &session_id,
                    call_id.clone(),
                    "shell".to_string(),
                    body.arguments,
                )
                .await
            };
            mark_driver_lifecycle(&state, &binding_id, "running").await;
            publish_verification_result(&state, &session_id, &result);
            result
        }
        _ => {
            governed_tool_call(
                &state,
                &session_id,
                call_id.clone(),
                body.name,
                body.arguments,
            )
            .await
        }
    };
    let response_json = tool_result_json(result.clone());
    let response_payload =
        serde_json::to_string(&response_json).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let processed_at = unix_key(0);
    {
        let store = storage.lock().await;
        store
            .mark_driver_event_processed_with_result(
                &binding_id,
                &event_key,
                if result.is_error {
                    "failed"
                } else {
                    "processed"
                },
                &processed_at,
                Some(&response_payload),
            )
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    Ok(Json(response_json))
}

pub(crate) async fn governed_tool_call(
    state: &DaemonState,
    session_id: &str,
    call_id: String,
    name: String,
    arguments: Value,
) -> ToolResult {
    let Some(live) = state.get_session(session_id) else {
        return ToolResult::error("session is no longer live");
    };
    let Some(tx) = &live.driver_tool_tx else {
        return ToolResult::error("governed session executor is unavailable");
    };
    let (response, receiver) = tokio::sync::oneshot::channel();
    let call = ToolCall {
        id: format!("driver:{session_id}:{call_id}"),
        name,
        arguments_json: arguments.to_string(),
    };
    if tx
        .send(DriverToolRequest {
            call,
            permission_only: false,
            response,
        })
        .await
        .is_err()
    {
        return ToolResult::error("governed session executor stopped");
    }
    receiver
        .await
        .unwrap_or_else(|_| ToolResult::error("governed session executor dropped its result"))
}

pub(crate) async fn memory_recall(
    state: &DaemonState,
    session_id: &str,
    args: &Value,
) -> Result<ToolResult, StatusCode> {
    let live = state.get_session(session_id).ok_or(StatusCode::NOT_FOUND)?;
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(5)
        .clamp(1, 10) as usize;
    let injection = crate::api::retrieve_session_memory(
        state,
        session_id,
        &live.workspace_root,
        query,
        limit,
        "driver_gateway",
    )
    .await;
    Ok(ToolResult::ok(match injection {
        Some(value) => json!({
            "context": value.context,
            "receipt": {
                "source": value.receipt.source,
                "workspaceScope": value.receipt.workspace_scope,
                "scopeChain": value.receipt.scope_chain,
                "items": value.receipt.items.into_iter().map(|item| json!({
                    "id": item.id,
                    "scope": item.scope,
                    "score": item.score,
                    "provenance": item.provenance,
                })).collect::<Vec<_>>(),
            }
        })
        .to_string(),
        None => json!({"context": "", "receipt": {"source": "memright", "items": []}}).to_string(),
    }))
}

pub(crate) async fn delegate_task(
    state: &DaemonState,
    session_id: &str,
    args: &Value,
) -> ToolResult {
    let Some(role) = args.get("role").and_then(Value::as_str) else {
        return ToolResult::error("delegate_task requires role");
    };
    let Some(task) = args.get("task").and_then(Value::as_str) else {
        return ToolResult::error("delegate_task requires task");
    };
    let options = match build_delegate_spawn_options(args, "delegate_task") {
        Ok(options) => options,
        Err(error) => return ToolResult::error(error),
    };
    match state.spawn(role, task, session_id, options).await {
        Ok(output) => ToolResult::ok(output),
        Err(error) => ToolResult::error(error),
    }
}

pub(crate) async fn delegate_parallel(
    state: &DaemonState,
    session_id: &str,
    args: &Value,
) -> ToolResult {
    let Some(tasks) = args.get("tasks").and_then(Value::as_array) else {
        return ToolResult::error("delegate_parallel requires tasks");
    };
    if tasks.is_empty() || tasks.len() > 4 {
        return ToolResult::error("delegate_parallel requires 1-4 tasks");
    }
    // Parse per-task options up front so any schema problems fail before
    // we kick off the join set. `index` is reported back in the response
    // payload so callers can correlate.
    let mut prepared: Vec<(usize, String, String, SubagentSpawnOptions)> =
        Vec::with_capacity(tasks.len());
    for (index, task) in tasks.iter().enumerate() {
        let Some(role) = task.get("role").and_then(Value::as_str) else {
            return ToolResult::error(format!("tasks[{index}] requires role"));
        };
        let Some(prompt) = task.get("task").and_then(Value::as_str) else {
            return ToolResult::error(format!("tasks[{index}] requires task"));
        };
        let options = match build_delegate_spawn_options(task, "delegate_parallel") {
            Ok(options) => options,
            Err(error) => return ToolResult::error(format!("tasks[{index}]: {error}")),
        };
        prepared.push((index, role.to_string(), prompt.to_string(), options));
    }
    let mut join_set = tokio::task::JoinSet::new();
    for (index, role, prompt, options) in prepared {
        let state = state.clone();
        let session_id = session_id.to_string();
        join_set.spawn(async move {
            let result = state.spawn(&role, &prompt, &session_id, options).await;
            (index, role, result)
        });
    }
    let mut results = Vec::with_capacity(tasks.len());
    while let Some(joined) = join_set.join_next().await {
        match joined {
            Ok((index, role, result)) => results.push(json!({
                "index": index,
                "role": role,
                "success": result.is_ok(),
                "output": result.unwrap_or_else(|error| error),
            })),
            Err(error) => results.push(json!({"success": false, "output": error.to_string()})),
        }
    }
    results.sort_by_key(|row| row["index"].as_u64().unwrap_or(u64::MAX));
    ToolResult::ok(json!({"results": results}).to_string())
}

/// Build a `SubagentSpawnOptions` from the `delegate_task` or
/// `delegate_parallel` JSON arguments. PR 5 threads the optional
/// `runtime` and `continuity` selectors through unchanged so the
/// spawner can route through `AuthenticatedCliExecutor` when the
/// caller asked for an authenticated vendor CLI.
pub(crate) fn build_delegate_spawn_options(
    args: &Value,
    surface: &str,
) -> Result<SubagentSpawnOptions, String> {
    let max_iterations = match args.get("maxTurns") {
        Some(value) => {
            let raw = value
                .as_u64()
                .ok_or_else(|| format!("{surface}: 'maxTurns' must be a positive integer"))?;
            if raw == 0 || raw > u32::MAX as u64 {
                return Err(format!(
                    "{surface}: 'maxTurns' must be between 1 and 4294967295"
                ));
            }
            Some(raw as u32)
        }
        None => None,
    };
    let runtime = match args.get("runtime") {
        Some(value) if !value.is_null() => SpawnRuntimeKind::from_tool_args(value)
            .map_err(|error| format!("{surface}: {error}"))?,
        _ => flat_delegate_runtime(args, surface)?,
    };
    let continuity =
        SubagentContinuity::from_tool_args(args.get("continuity").unwrap_or(&Value::Null))
            .map_err(|error| format!("{surface}: {error}"))?;
    Ok(SubagentSpawnOptions {
        max_iterations,
        runtime,
        continuity,
        ..SubagentSpawnOptions::default()
    })
}

pub(crate) fn flat_delegate_runtime(
    args: &Value,
    surface: &str,
) -> Result<Option<SpawnRuntimeKind>, String> {
    let provider_id = match args.get("providerHint") {
        Some(value) => value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("{surface}: 'providerHint' must be a non-empty string"))?,
        None => {
            if args.get("modelHint").is_some() {
                return Err(format!(
                    "{surface}: 'modelHint' requires a non-empty 'providerHint'"
                ));
            }
            return Ok(None);
        }
    };
    let model_id = match args.get("modelHint") {
        Some(value) => Some(
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("{surface}: 'modelHint' must be a non-empty string"))?
                .to_string(),
        ),
        None => None,
    };
    Ok(Some(SpawnRuntimeKind::AuthenticatedCli {
        provider_id: provider_id.to_string(),
        model_id,
        account_profile_id: format!("{provider_id}/default"),
    }))
}

pub(crate) async fn authorize_lease(
    state: &DaemonState,
    session_id: &str,
    headers: &HeaderMap,
) -> Result<
    (
        rightkit_agent_engine_storage::SessionCapabilityLeaseRow,
        Vec<String>,
    ),
    StatusCode,
> {
    let token = headers
        .get(CAPABILITY_HEADER.as_str())
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let mut parts = token.split('.');
    let lease_id = parts.next().ok_or(StatusCode::UNAUTHORIZED)?;
    let nonce = parts.next().ok_or(StatusCode::UNAUTHORIZED)?;
    let secret = parts.next().ok_or(StatusCode::UNAUTHORIZED)?;
    if parts.next().is_some() || secret.len() < 64 {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let live = state.get_session(session_id).ok_or(StatusCode::NOT_FOUND)?;
    let workspace = canonical_workspace(&live.workspace_root)?;
    let storage = state
        .storage
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let lease = storage
        .lock()
        .await
        .get_active_session_capability_lease(
            &token_hash(token),
            nonce,
            session_id,
            &workspace.to_string_lossy(),
            &unix_key(0),
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .filter(|row| row.id == lease_id)
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let tools: Vec<String> = serde_json::from_str(&lease.tool_ceiling_json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((lease, tools))
}

pub(crate) async fn authorize_gateway_lease(
    state: &DaemonState,
    session_id: &str,
    headers: &HeaderMap,
) -> Result<
    (
        rightkit_agent_engine_storage::SessionCapabilityLeaseRow,
        Vec<String>,
        String,
    ),
    StatusCode,
> {
    let (lease, tools) = authorize_lease(state, session_id, headers).await?;
    let instance_secret = headers
        .get(DRIVER_INSTANCE_HEADER.as_str())
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.len() >= 64)
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let instance_key = token_hash(instance_secret);
    if lease.process_instance_hash.as_deref() != Some(instance_key.as_str())
        || !runtime_contains_binding(state, &lease.binding_id).await
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok((lease, tools, instance_key))
}

pub(crate) async fn runtime_contains_binding(state: &DaemonState, binding_id: &str) -> bool {
    state.driver_host.contains(binding_id).await
        || match state.cli_bridge.as_ref() {
            Some(bridge) => bridge.contains(binding_id).await,
            None => false,
        }
        || state
            .qualification_bindings
            .read()
            .map(|bindings| bindings.contains(binding_id))
            .unwrap_or(false)
}

pub(crate) fn effective_ceiling(
    live: &crate::api::LiveSession,
    requested: &[String],
) -> Result<Vec<String>, StatusCode> {
    effective_ceiling_for(
        live.safe_mode,
        &live.allowed_tools,
        &live.disallowed_tools,
        requested,
    )
}

pub(crate) fn effective_ceiling_for(
    safe_mode: bool,
    allowed_tools: &[String],
    disallowed_tools: &[String],
    requested: &[String],
) -> Result<Vec<String>, StatusCode> {
    let registry = ToolRegistry::build_default();
    let known = |name: &str| registry.get(name).is_some() || SPECIAL_TOOLS.contains(&name);
    let mut tools = if requested.is_empty() {
        if safe_mode {
            registry
                .capabilities()
                .into_iter()
                .filter(|capability| capability.read_only)
                .map(|capability| capability.name)
                .chain(
                    READ_ONLY_SPECIAL_TOOLS
                        .iter()
                        .map(|name| (*name).to_string()),
                )
                .collect::<Vec<_>>()
        } else {
            registry
                .all()
                .iter()
                .map(|tool| tool.name().to_string())
                .chain(SPECIAL_TOOLS.iter().map(|name| (*name).to_string()))
                .collect::<Vec<_>>()
        }
    } else {
        if requested
            .iter()
            .any(|name| !known(name) || DRIVER_BLOCKED_TOOLS.contains(&name.as_str()))
        {
            return Err(StatusCode::BAD_REQUEST);
        }
        requested.to_vec()
    };
    if safe_mode {
        let read_only_builtins = registry
            .capabilities()
            .into_iter()
            .filter(|capability| capability.read_only)
            .map(|capability| capability.name)
            .collect::<HashSet<_>>();
        tools.retain(|tool| {
            read_only_builtins.contains(tool) || READ_ONLY_SPECIAL_TOOLS.contains(&tool.as_str())
        });
    }
    if !allowed_tools.is_empty() {
        let allowed = allowed_tools.iter().collect::<HashSet<_>>();
        tools.retain(|tool| {
            if safe_mode {
                READ_ONLY_SPECIAL_TOOLS.contains(&tool.as_str()) || allowed.contains(tool)
            } else {
                SPECIAL_TOOLS.contains(&tool.as_str()) || allowed.contains(tool)
            }
        });
    }
    let denied = disallowed_tools.iter().collect::<HashSet<_>>();
    tools.retain(|tool| !denied.contains(tool) && !DRIVER_BLOCKED_TOOLS.contains(&tool.as_str()));
    tools.sort();
    tools.dedup();
    Ok(tools)
}

pub(crate) fn capability_schema(registry: &ToolRegistry, name: &str) -> Option<Value> {
    match name {
        "memory_recall" => Some(json!({
            "name": name,
            "description": "Recall session-scoped MemRight context with RightKit provenance.",
            "inputSchema": {"type": "object", "properties": {"query": {"type": "string"}, "limit": {"type": "integer", "minimum": 1, "maximum": 10}}, "required": ["query"]},
            "annotations": capability_annotations(true),
        })),
        "delegate_task" => Some(json!({
            "name": name,
            "description": "Delegate one bounded task to a RightKit-managed model/agent.",
            "inputSchema": delegate_task_schema(false),
            "annotations": capability_annotations(false),
        })),
        "delegate_parallel" => Some(json!({
            "name": name,
            "description": "Delegate 1-4 independent tasks through RightKit in parallel.",
            "inputSchema": {"type": "object", "properties": {"tasks": {"type": "array", "minItems": 1, "maxItems": 4, "items": delegate_task_schema(true)}}, "required": ["tasks"]},
            "annotations": capability_annotations(false),
        })),
        "verify" => Some(json!({
            "name": name,
            "description": "Run a verification command through RightKit's governed shell and emit typed verification evidence.",
            "inputSchema": {"type": "object", "properties": {"command": {"type": "string"}, "timeout_ms": {"type": "integer", "minimum": 1, "maximum": 300000}}, "required": ["command"], "additionalProperties": false},
            "annotations": capability_annotations(false),
        })),
        _ => registry.get(name).map(|tool| {
            json!({
                "name": tool.name(),
                "description": tool.description(),
                "inputSchema": tool.input_schema(),
                "annotations": capability_annotations(false),
            })
        }),
    }
}

pub(crate) fn delegate_task_schema(_parallel_item: bool) -> Value {
    json!({
        "type": "object",
        "properties": {
            "role": {"type": "string"},
            "task": {"type": "string"},
            "providerHint": {"type": "string", "description": "Optional CLI provider hint (for example codex or claude)."},
            "modelHint": {"type": "string", "description": "Optional model hint for providerHint."},
            "maxTurns": {"type": "integer", "minimum": 1},
            "runtime": {"type": "object", "description": "Legacy nested runtime selector; providerHint/modelHint are preferred."},
            "continuity": {"type": "object"}
        },
        "required": ["role", "task"]
    })
}

pub(crate) fn capability_annotations(read_only: bool) -> Value {
    json!({
        "readOnlyHint": read_only,
        "destructiveHint": false,
        "idempotentHint": read_only,
        "openWorldHint": false,
    })
}
