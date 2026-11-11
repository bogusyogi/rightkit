async fn get_settings(
    State(state): State<DaemonState>,
    Query(q): Query<WorkspaceQuery>,
) -> Json<serde_json::Value> {
    Json(settings_response(&state, &q))
}

async fn get_config_status(Query(q): Query<WorkspaceQuery>) -> Json<ConfigStatus> {
    Json(config_status(&q))
}

fn inference_error(code: &'static str, status: StatusCode) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"code": code, "detail": code})))
}

fn direct_row_wire(row: &rightkit_agent_engine_storage::DirectInferenceConnectionRow) -> Value {
    json!({
        "id": row.id, "name": row.name, "protocol": row.protocol,
        "normalizedBaseUrl": row.base_url, "authKind": row.auth_kind,
        "authHeaderName": row.auth_header_name, "publicHeaders": serde_json::from_str::<Value>(&row.public_headers_json).unwrap_or_else(|_| json!({})),
        "manualModelIds": serde_json::from_str::<Value>(&row.manual_models_json).unwrap_or_else(|_| json!([])),
        "trustPrivateNetwork": row.trust_private_network, "credentialConfigured": row.credential_configured,
        "credentialRevision": row.credential_revision, "configurationRevision": row.configuration_revision,
        "createdAt": row.created_at, "updatedAt": row.updated_at,
    })
}

fn direct_connection_from_row(
    row: &rightkit_agent_engine_storage::DirectInferenceConnectionRow,
) -> Result<crate::inference_runtime::DirectConnection, &'static str> {
    Ok(crate::inference_runtime::DirectConnection {
        id: row.id.clone(),
        name: row.name.clone(),
        protocol: serde_json::from_str(&format!("\"{}\"", row.protocol))
            .map_err(|_| "invalid_connection")?,
        normalized_base_url: row.base_url.clone(),
        auth_kind: serde_json::from_str(&format!("\"{}\"", row.auth_kind))
            .map_err(|_| "invalid_connection")?,
        auth_header_name: row.auth_header_name.clone(),
        public_headers: serde_json::from_str(&row.public_headers_json)
            .map_err(|_| "invalid_connection")?,
        manual_model_ids: serde_json::from_str(&row.manual_models_json)
            .map_err(|_| "invalid_connection")?,
        trust_private_network: row.trust_private_network,
        credential_configured: row.credential_configured,
        credential_revision: row.credential_revision,
        configuration_revision: row.configuration_revision,
        created_at: row.created_at.clone(),
        updated_at: row.updated_at.clone(),
    })
}

async fn probe_direct_connection(
    row: &rightkit_agent_engine_storage::DirectInferenceConnectionRow,
) -> Result<Value, (StatusCode, Json<Value>)> {
    let connection = direct_connection_from_row(row)
        .map_err(|code| inference_error(code, StatusCode::BAD_REQUEST))?;
    let auth_header = if connection.auth_kind != crate::inference_runtime::DirectAuthKind::None {
        if !connection.credential_configured {
            return Err(inference_error("secret_required", StatusCode::BAD_REQUEST));
        }
        let secret = rightkit_llm::agent_providers::KeyStore::get(
            "coderight",
            &format!("direct-connection:{}", connection.id),
        )
        .map_err(|_| inference_error("keychain_unavailable", StatusCode::SERVICE_UNAVAILABLE))?;
        Some(match connection.auth_kind {
            crate::inference_runtime::DirectAuthKind::Bearer => {
                ("Authorization".into(), format!("Bearer {secret}"))
            }
            crate::inference_runtime::DirectAuthKind::XApiKey => ("x-api-key".into(), secret),
            crate::inference_runtime::DirectAuthKind::ApiKeyHeader => {
                let name = connection
                    .auth_header_name
                    .as_deref()
                    .ok_or_else(|| inference_error("invalid_header", StatusCode::BAD_REQUEST))?;
                (name.to_string(), secret)
            }
            crate::inference_runtime::DirectAuthKind::None => unreachable!(),
        })
    } else {
        None
    };
    let models = crate::inference_runtime::discover_direct_models(&connection, auth_header)
        .await
        .map_err(|code| inference_error(code, StatusCode::BAD_GATEWAY))?;
    Ok(
        json!({"authority":"direct", "connectionId": connection.id, "state":"ready", "qualified":true, "models":models}),
    )
}

#[derive(Clone)]
enum CachedDirectCatalog {
    Ready(Value),
    Failed(StatusCode, Value),
}

struct DirectCatalogCacheEntry {
    expires_at: std::time::Instant,
    result: CachedDirectCatalog,
}

fn direct_catalog_cache_key(
    id: &str,
    configuration_revision: u64,
    credential_revision: u64,
) -> String {
    format!("direct:{id}:{configuration_revision}:{credential_revision}")
}

fn catalog_cache_ttl(ready: bool) -> std::time::Duration {
    std::time::Duration::from_secs(if ready { 30 } else { 2 })
}

async fn cached_direct_catalog(
    row: &rightkit_agent_engine_storage::DirectInferenceConnectionRow,
) -> Result<Value, (StatusCode, Json<Value>)> {
    static CACHE: std::sync::OnceLock<
        tokio::sync::Mutex<std::collections::HashMap<String, DirectCatalogCacheEntry>>,
    > = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let key =
        direct_catalog_cache_key(&row.id, row.configuration_revision, row.credential_revision);
    let now = std::time::Instant::now();
    if let Some(cached) = cache
        .lock()
        .await
        .get(&key)
        .filter(|entry| entry.expires_at > now)
        .map(|entry| entry.result.clone())
    {
        return match cached {
            CachedDirectCatalog::Ready(value) => Ok(value),
            CachedDirectCatalog::Failed(status, value) => Err((status, Json(value))),
        };
    }

    let result = probe_direct_connection(row).await;
    let (cached, ttl) = match &result {
        Ok(value) => (
            CachedDirectCatalog::Ready(value.clone()),
            catalog_cache_ttl(true),
        ),
        Err((status, Json(value))) => (
            CachedDirectCatalog::Failed(*status, value.clone()),
            catalog_cache_ttl(false),
        ),
    };
    let mut entries = cache.lock().await;
    entries.retain(|_, entry| entry.expires_at > now);
    entries.insert(
        key,
        DirectCatalogCacheEntry {
            expires_at: now + ttl,
            result: cached,
        },
    );
    result
}

fn direct_uuid() -> String {
    let now = format!("{}:{}", now_iso(), std::process::id());
    let digest = rightkit_crypto::sha256(now.as_bytes());
    format!("{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-4{:01x}{:02x}-8{:01x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}", digest[0],digest[1],digest[2],digest[3],digest[4],digest[5],digest[6]&0x0f,digest[7],digest[8]&0x0f,digest[9],digest[10],digest[11],digest[12],digest[13],digest[14],digest[15])
}

fn direct_row_from_save(
    id: String,
    body: SaveDirectInferenceConnection,
    existing: Option<&rightkit_agent_engine_storage::DirectInferenceConnectionRow>,
) -> Result<rightkit_agent_engine_storage::DirectInferenceConnectionRow, (StatusCode, Json<Value>)>
{
    let normalized_base_url =
        crate::inference_runtime::normalize_base_url(&body.normalized_base_url)
            .map_err(|e| inference_error(e.code(), StatusCode::BAD_REQUEST))?;
    let now = now_iso();
    let row = rightkit_agent_engine_storage::DirectInferenceConnectionRow {
        id,
        name: body.name,
        protocol: serde_json::to_value(body.protocol)
            .unwrap_or_default()
            .as_str()
            .unwrap_or_default()
            .to_string(),
        base_url: normalized_base_url,
        auth_kind: serde_json::to_value(body.auth_kind)
            .unwrap_or_default()
            .as_str()
            .unwrap_or_default()
            .to_string(),
        auth_header_name: body.auth_header_name,
        public_headers_json: serde_json::to_string(&body.public_headers)
            .unwrap_or_else(|_| "{}".into()),
        manual_models_json: serde_json::to_string(&body.manual_model_ids)
            .unwrap_or_else(|_| "[]".into()),
        trust_private_network: body.trust_private_network,
        // `none` has no keychain identity.  Keeping a stale item would let a
        // later configuration silently inherit a credential.
        credential_configured: !matches!(
            body.auth_kind,
            crate::inference_runtime::DirectAuthKind::None
        ) && existing.is_some_and(|row| row.credential_configured),
        credential_revision: existing.map_or(0, |row| {
            row.credential_revision.saturating_add(u64::from(
                row.auth_kind != "none"
                    && body.auth_kind == crate::inference_runtime::DirectAuthKind::None,
            ))
        }),
        configuration_revision: existing
            .map_or(1, |row| row.configuration_revision.saturating_add(1)),
        created_at: existing.map_or_else(|| now.clone(), |row| row.created_at.clone()),
        updated_at: now,
    };
    let wire = crate::inference_runtime::DirectConnection {
        id: row.id.clone(),
        name: row.name.clone(),
        protocol: body.protocol,
        normalized_base_url: row.base_url.clone(),
        auth_kind: body.auth_kind,
        auth_header_name: row.auth_header_name.clone(),
        public_headers: body.public_headers,
        manual_model_ids: body.manual_model_ids,
        trust_private_network: row.trust_private_network,
        credential_configured: row.credential_configured,
        credential_revision: row.credential_revision,
        configuration_revision: row.configuration_revision,
        created_at: row.created_at.clone(),
        updated_at: row.updated_at.clone(),
    };
    crate::inference_runtime::validate_direct_connection(&wire)
        .map_err(|e| inference_error(e.code(), StatusCode::BAD_REQUEST))?;
    Ok(row)
}

async fn list_inference_connections(
    State(state): State<DaemonState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let storage = state
        .storage
        .as_ref()
        .ok_or_else(|| inference_error("authority_unavailable", StatusCode::SERVICE_UNAVAILABLE))?
        .clone();
    let rows = storage
        .lock()
        .await
        .list_direct_inference_connections()
        .map_err(|_| inference_error("authority_unavailable", StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(
        json!({"connections": rows.iter().map(direct_row_wire).collect::<Vec<_>>() }),
    ))
}

async fn create_direct_connection(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(body): Json<SaveDirectInferenceConnection>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    enforce_trusted_source_authority(&headers)
        .map_err(|s| inference_error("invalid_connection", s))?;
    let row = direct_row_from_save(direct_uuid(), body, None)?;
    let wire = direct_connection_from_row(&row)
        .map_err(|code| inference_error(code, StatusCode::BAD_REQUEST))?;
    crate::inference_runtime::validate_direct_request_target(&wire)
        .await
        .map_err(|error| inference_error(error.code(), StatusCode::BAD_REQUEST))?;
    let storage = state
        .storage
        .as_ref()
        .ok_or_else(|| inference_error("authority_unavailable", StatusCode::SERVICE_UNAVAILABLE))?
        .clone();
    storage
        .lock()
        .await
        .upsert_direct_inference_connection(&row)
        .map_err(|_| inference_error("authority_unavailable", StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(direct_row_wire(&row)))
}

async fn update_direct_connection(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<SaveDirectInferenceConnection>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    enforce_trusted_source_authority(&headers)
        .map_err(|s| inference_error("invalid_connection", s))?;
    let storage = state
        .storage
        .as_ref()
        .ok_or_else(|| inference_error("authority_unavailable", StatusCode::SERVICE_UNAVAILABLE))?
        .clone();
    let old = storage
        .lock()
        .await
        .direct_inference_connection(&id)
        .map_err(|_| inference_error("authority_unavailable", StatusCode::INTERNAL_SERVER_ERROR))?
        .ok_or_else(|| inference_error("connection_not_found", StatusCode::NOT_FOUND))?;
    let row = direct_row_from_save(id.clone(), body, Some(&old))?;
    let wire = direct_connection_from_row(&row)
        .map_err(|code| inference_error(code, StatusCode::BAD_REQUEST))?;
    crate::inference_runtime::validate_direct_request_target(&wire)
        .await
        .map_err(|error| inference_error(error.code(), StatusCode::BAD_REQUEST))?;
    if old.auth_kind != "none" && row.auth_kind == "none" && old.credential_configured {
        match rightkit_llm::agent_providers::KeyStore::delete(
            "coderight",
            &format!("direct-connection:{id}"),
        ) {
            Ok(()) | Err(rightkit_llm::agent_providers::KeyStoreError::NotFound { .. }) => {}
            Err(_) => {
                return Err(inference_error(
                    "keychain_unavailable",
                    StatusCode::SERVICE_UNAVAILABLE,
                ))
            }
        }
    }
    storage
        .lock()
        .await
        .upsert_direct_inference_connection(&row)
        .map_err(|_| inference_error("authority_unavailable", StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(direct_row_wire(&row)))
}

async fn delete_direct_connection(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    enforce_trusted_source_authority(&headers)
        .map_err(|s| inference_error("invalid_connection", s))?;
    let storage = state
        .storage
        .as_ref()
        .ok_or_else(|| inference_error("authority_unavailable", StatusCode::SERVICE_UNAVAILABLE))?
        .clone();
    let removed = storage
        .lock()
        .await
        .delete_direct_inference_connection(&id)
        .map_err(|_| inference_error("authority_unavailable", StatusCode::INTERNAL_SERVER_ERROR))?;
    if !removed {
        return Err(inference_error(
            "connection_not_found",
            StatusCode::NOT_FOUND,
        ));
    }
    let _ = rightkit_llm::agent_providers::KeyStore::delete(
        "coderight",
        &format!("direct-connection:{id}"),
    );
    Ok(Json(json!({"deleted": true, "id": id})))
}

async fn put_direct_connection_secret(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<SaveDirectInferenceSecret>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    enforce_trusted_source_authority(&headers)
        .map_err(|s| inference_error("invalid_connection", s))?;
    if body.secret.is_empty() || body.secret.len() > 16_384 {
        return Err(inference_error("secret_required", StatusCode::BAD_REQUEST));
    }
    let storage = state
        .storage
        .as_ref()
        .ok_or_else(|| inference_error("authority_unavailable", StatusCode::SERVICE_UNAVAILABLE))?
        .clone();
    let mut row = storage
        .lock()
        .await
        .direct_inference_connection(&id)
        .map_err(|_| inference_error("authority_unavailable", StatusCode::INTERNAL_SERVER_ERROR))?
        .ok_or_else(|| inference_error("connection_not_found", StatusCode::NOT_FOUND))?;
    rightkit_llm::agent_providers::KeyStore::set(
        "coderight",
        &format!("direct-connection:{id}"),
        &body.secret,
    )
    .map_err(|_| inference_error("keychain_unavailable", StatusCode::SERVICE_UNAVAILABLE))?;
    row.credential_configured = true;
    row.credential_revision = row.credential_revision.saturating_add(1);
    row.updated_at = now_iso();
    storage
        .lock()
        .await
        .upsert_direct_inference_connection(&row)
        .map_err(|_| inference_error("authority_unavailable", StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(
        json!({"credentialConfigured":true,"credentialRevision":row.credential_revision}),
    ))
}

async fn delete_direct_connection_secret(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    enforce_trusted_source_authority(&headers)
        .map_err(|s| inference_error("invalid_connection", s))?;
    let storage = state
        .storage
        .as_ref()
        .ok_or_else(|| inference_error("authority_unavailable", StatusCode::SERVICE_UNAVAILABLE))?
        .clone();
    let mut row = storage
        .lock()
        .await
        .direct_inference_connection(&id)
        .map_err(|_| inference_error("authority_unavailable", StatusCode::INTERNAL_SERVER_ERROR))?
        .ok_or_else(|| inference_error("connection_not_found", StatusCode::NOT_FOUND))?;
    rightkit_llm::agent_providers::KeyStore::delete(
        "coderight",
        &format!("direct-connection:{id}"),
    )
    .map_err(|_| inference_error("keychain_unavailable", StatusCode::SERVICE_UNAVAILABLE))?;
    row.credential_configured = false;
    row.credential_revision = row.credential_revision.saturating_add(1);
    row.updated_at = now_iso();
    storage
        .lock()
        .await
        .upsert_direct_inference_connection(&row)
        .map_err(|_| inference_error("authority_unavailable", StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(
        json!({"credentialConfigured":false,"credentialRevision":row.credential_revision}),
    ))
}

async fn probe_inference_connection(
    State(state): State<DaemonState>,
    Path((authority, id)): Path<(String, String)>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match authority.as_str() {
        "direct" => {
            let storage = state
                .storage
                .as_ref()
                .ok_or_else(|| {
                    inference_error("authority_unavailable", StatusCode::SERVICE_UNAVAILABLE)
                })?
                .clone();
            let row = storage
                .lock()
                .await
                .direct_inference_connection(&id)
                .map_err(|_| {
                    inference_error("authority_unavailable", StatusCode::INTERNAL_SERVER_ERROR)
                })?
                .ok_or_else(|| inference_error("connection_not_found", StatusCode::NOT_FOUND))?;
            Ok(Json(probe_direct_connection(&row).await?))
        }
        _ => Err(inference_error(
            "authority_unavailable",
            StatusCode::BAD_REQUEST,
        )),
    }
}
async fn get_inference_catalog(State(state): State<DaemonState>) -> Json<Value> {
    let mut groups = Vec::new();
    if let Some(storage) = state.storage.as_ref() {
        let rows = storage
            .lock()
            .await
            .list_direct_inference_connections()
            .unwrap_or_default();
        for row in rows {
            let (state_name, models) = match cached_direct_catalog(&row).await {
                Ok(value) => (
                    "ready",
                    value.get("models").cloned().unwrap_or_else(|| json!([])),
                ),
                Err((_, Json(error))) => (
                    if error.get("code").and_then(Value::as_str) == Some("secret_required") {
                        "needs_secret"
                    } else {
                        "unreachable"
                    },
                    json!([]),
                ),
            };
            groups.push(json!({
                "authority":"direct",
                "connectionId":row.id,
                "name":row.name,
                "protocol":row.protocol,
                "state":state_name,
                "models":models,
            }));
        }
    }
    Json(json!({"groups":groups}))
}
async fn get_inference_load(
    State(state): State<DaemonState>,
) -> Json<crate::inference_runtime::LoadSnapshot> {
    Json(state.inference_scheduler.snapshot().await)
}

async fn reload_settings(
    State(state): State<DaemonState>,
    Query(q): Query<WorkspaceQuery>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let workspace = workspace_path(&q);
    let cfg = rightkit_agent_config::ConfigLoader::new()
        .load_files_for_workspace(&workspace);
    {
        let mut store = lock_recover(&state.settings, "state.settings");
        if !cfg.orchestration.boss_model.trim().is_empty() {
            store.primary_orchestrator_model_id = cfg.orchestration.boss_model.clone();
        }
        apply_budget_to_store(&mut store, &cfg.orchestration.budget);
    }

    Ok(Json(serde_json::json!({
        "reloaded": true,
        "reloadedAt": now_iso(),
        "configStatus": config_status(&q),
        "settings": settings_response(&state, &q),
    })))
}

async fn list_settings_toolsets(
    Query(q): Query<WorkspaceQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let workspace = workspace_path(&q);
    let workspace_config =
        load_workspace_config_only(&workspace).map_err(|_| StatusCode::BAD_REQUEST)?;
    let effective_config = rightkit_agent_config::ConfigLoader::new()
        .load_files_for_workspace(&workspace);
    let configured = configured_toolset_profiles(&workspace_config);
    let effective = toolset_profiles_for_config(&effective_config);
    Ok(Json(serde_json::json!({
        "workspaceRoot": workspace.display().to_string(),
        "path": workspace_config_path(&workspace).display().to_string(),
        "configured": configured,
        "effective": effective,
    })))
}

async fn save_settings_toolset(
    Path(id): Path<String>,
    Query(q): Query<WorkspaceQuery>,
    headers: HeaderMap,
    Json(body): Json<SaveToolsetConfig>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    validate_toolset_id(&id).map_err(|_| StatusCode::BAD_REQUEST)?;
    let workspace = workspace_path(&q);
    let profile = rightkit_agent_config::ToolsetConfig {
        name: body.name.unwrap_or_else(|| id.clone()).trim().to_string(),
        description: body.description.unwrap_or_default().trim().to_string(),
        allowed_tools: clean_list(body.allowed_tools.unwrap_or_default()),
        disallowed_tools: clean_list(body.disallowed_tools.unwrap_or_default()),
        read_only: body.read_only.unwrap_or(false),
        default_behavioral_skills: clean_list(body.default_behavioral_skills.unwrap_or_default()),
    };
    // Read-modify-write under the workspace config lock, on a blocking thread (the lock wait can last
    // five seconds). A read failure is a bad request, a write or lock failure a server error.
    let (write_id, write_profile) = (id.clone(), profile.clone());
    update_workspace_config_blocking(workspace.clone(), move |config| {
        config.toolsets.insert(write_id, write_profile);
    })
    .await?;

    Ok(Json(serde_json::json!({
        "saved": true,
        "toolset": toolset_settings_profile(&id, &profile),
        "configStatus": config_status(&q),
    })))
}

async fn delete_settings_toolset(
    Path(id): Path<String>,
    Query(q): Query<WorkspaceQuery>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    validate_toolset_id(&id).map_err(|_| StatusCode::BAD_REQUEST)?;
    let workspace = workspace_path(&q);
    let removed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let removed_flag = removed.clone();
    let remove_id = id.clone();
    update_workspace_config_blocking(workspace.clone(), move |config| {
        removed_flag.store(
            config.toolsets.remove(&remove_id).is_some(),
            std::sync::atomic::Ordering::Relaxed,
        );
    })
    .await?;
    let removed = removed.load(std::sync::atomic::Ordering::Relaxed);

    Ok(Json(serde_json::json!({
        "deleted": removed,
        "id": id,
        "configStatus": config_status(&q),
    })))
}

fn settings_response(state: &DaemonState, q: &WorkspaceQuery) -> serde_json::Value {
    let store = lock_recover(&state.settings, "state.settings").clone();
    let registry = &state.agent_registry;
    let persisted_config = rightkit_agent_config::ConfigLoader::new()
        .load_files_for_workspace(&workspace_path(q));
    let role_model_split_available =
        crate::model_policy::split_entitled(|feature| state.has_license_feature(feature));
    let effective_role_model_mode =
        crate::model_policy::role_model_mode(&persisted_config, role_model_split_available);

    // Build model catalog from the agent registry (one entry per agent), plus
    // curated selectable OpenRouter models that are not launch-default roles.
    let mut models: Vec<serde_json::Value> = registry
        .all()
        .iter()
        .map(|a| {
            // Reflect the effective model: a persisted per-agent override wins over
            // the built-in default, so the Settings UI shows the user's choice.
            let effective_slug = persisted_config
                .orchestration
                .agents
                .get(&a.name)
                .map(|o| o.model_id.clone())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| a.model_id.clone());
            let display = effective_slug
                .splitn(2, '/')
                .last()
                .unwrap_or(&effective_slug)
                .to_string();
            serde_json::json!({
                "id": a.name,
                "displayName": display,
                // Derived from the EFFECTIVE slug, not the descriptor's stored
                // provider: a per-agent model override changes the slug, and a
                // stale provider here is what told the user "openrouter has no
                // saved key" for a MiniMax model whose key was saved.
                "provider": crate::model_policy::provider_id_for_model_slug(&effective_slug),
                "modelSlug": effective_slug,
                "contextWindow": 128000,
                "supportsVision": false,
                "supportsTools": true,
                "enabled": true,
            })
        })
        .collect();
    models.push(serde_json::json!({
        "id": "x-ai/grok-build-0.1",
        "displayName": "Grok Build 0.1",
        "provider": "openrouter",
        "modelSlug": "x-ai/grok-build-0.1",
        "contextWindow": 256000,
        "inputCostPer1M": 1.0,
        "outputCostPer1M": 2.0,
        "supportsVision": true,
        "supportsTools": true,
        "enabled": true,
    }));

    let agents: Vec<serde_json::Value> = registry
        .all()
        .iter()
        .map(|a| {
            serde_json::json!({
                "id": a.name,
                "name": a.name,
                "role": a.role,
                "modelId": a.name,
                "enabled": true,
            })
        })
        .collect();

    serde_json::json!({
        "workspaceRoot": workspace_root_string(q),
        "defaultModel": store.primary_orchestrator_model_id,
        "primaryOrchestratorModelId": store.primary_orchestrator_model_id,
        "roleModelMode": effective_role_model_mode,
        "roleModelSplitAvailable": role_model_split_available,
        "models": models,
        "agents": agents,
        "budget": {
            "perTaskMaxUsd": store.per_task_max_usd,
            "perGoalMaxUsd": store.per_goal_max_usd,
            "perSessionMaxUsd": store.per_session_max_usd,
            "perAgentCallMaxUsd": store.per_agent_call_max_usd,
            "globalMonthlyMaxUsd": persisted_config.orchestration.budget.global_monthly_max_usd,
            "warningAt": persisted_config.orchestration.budget.warning_at,
            "maxJuryCallsPerRun": persisted_config.orchestration.budget.max_jury_calls_per_run,
            "maxEscalationDepth": persisted_config.orchestration.budget.max_escalation_depth,
            "maxRetriesPerRole": store.max_retries_per_role,
            "maxWallClockMinutes": store.max_wall_clock_minutes,
            "maxIterations": store.max_iterations,
            "maxWorkers": store.max_workers,
        },
        "benchmarks": {
            "dailyHealthChecksEnabled": false,
            "promotionMinQualityDelta": 0.05,
        },
        "remote": {
            "telegramTokenConfigured": persisted_config.remote.telegram_token.is_some(),
            "telegramAllowedUsers": persisted_config.remote.telegram_allowed_users,
            "webEnabled": persisted_config.remote.web_enabled(),
            "webEnabledConfigured": persisted_config.remote.web_enabled,
            "webEnabledSource": rightkit_agent_config::ConfigSource::effective_for_workspace(
                &workspace_path(q),
                rightkit_agent_config::BooleanSetting::RemoteWebEnabled,
            ),
        },
        "orchestration": {
            "conductorRouting": persisted_config.orchestration.conductor_routing(),
            "conductorRoutingConfigured": persisted_config.orchestration.conductor_routing,
            "conductorRoutingSource": rightkit_agent_config::ConfigSource::effective_for_workspace(
                &workspace_path(q),
                rightkit_agent_config::BooleanSetting::ConductorRouting,
            ),
        },
        "routing": {
            "capabilityRouter": persisted_config.orchestration.routing.capability_router(),
            "capabilityRouterConfigured": persisted_config.orchestration.routing.capability_router,
            "capabilityRouterSource": rightkit_agent_config::ConfigSource::effective_for_workspace(
                &workspace_path(q),
                rightkit_agent_config::BooleanSetting::CapabilityRouter,
            ),
            "aggressiveness": if persisted_config.orchestration.routing.aggressiveness.trim().is_empty() {
                "conservative".to_string()
            } else {
                persisted_config.orchestration.routing.aggressiveness.clone()
            },
        },
        "keepDaemonResident": store.keep_daemon_resident,
    })
}

/// Applies only the budget fields present in `patch` onto the freshly loaded config, so a save
/// that names one field cannot overwrite the others with a stale in-memory copy.
fn apply_budget_patch(
    patch: &serde_json::Value,
    budget: &mut rightkit_agent_config::BudgetConfig,
) {
    let f = |k: &str| patch.get(k).and_then(|v| v.as_f64());
    let u = |k: &str| patch.get(k).and_then(|v| v.as_u64()).map(|v| v as u32);
    if let Some(v) = f("perTaskMaxUsd") {
        budget.per_task_max_usd = v;
    }
    if let Some(v) = f("perGoalMaxUsd") {
        budget.per_goal_max_usd = v;
    }
    if let Some(v) = f("perSessionMaxUsd") {
        budget.per_session_max_usd = v;
    }
    if let Some(v) = f("perAgentCallMaxUsd") {
        budget.per_agent_call_max_usd = v;
    }
    if let Some(v) = f("globalMonthlyMaxUsd") {
        budget.global_monthly_max_usd = v;
    }
    if let Some(v) = f("warningAt") {
        budget.warning_at = v;
    }
    if let Some(v) = u("maxJuryCallsPerRun") {
        budget.max_jury_calls_per_run = v;
    }
    if let Some(v) = u("maxEscalationDepth") {
        budget.max_escalation_depth = v;
    }
    if let Some(v) = u("maxRetriesPerRole") {
        budget.max_retries_per_role = v;
    }
    if let Some(v) = u("maxWallClockMinutes") {
        budget.max_wall_clock_minutes = v;
    }
    if let Some(v) = u("maxIterations") {
        budget.max_iterations = v;
    }
    if let Some(v) = u("maxWorkers") {
        budget.max_workers = v;
    }
}

async fn save_settings(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(body): Json<SaveSettings>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let new_boss_model = body.primary_orchestrator_model_id.clone();

    // Update in-memory store (lock held briefly, released before any I/O).
    let (previous_settings, _snapshot) = {
        let mut store = lock_recover(&state.settings, "state.settings");
        let previous = store.clone();
        if let Some(ref id) = new_boss_model {
            store.primary_orchestrator_model_id = id.clone();
        }
        if let Some(b) = &body.budget {
            let f = |k: &str| b.get(k).and_then(|v| v.as_f64());
            let u = |k: &str| b.get(k).and_then(|v| v.as_u64()).map(|v| v as u32);
            if let Some(v) = f("perTaskMaxUsd") {
                store.per_task_max_usd = v;
            }
            if let Some(v) = f("perGoalMaxUsd") {
                store.per_goal_max_usd = v;
            }
            if let Some(v) = f("perSessionMaxUsd") {
                store.per_session_max_usd = v;
            }
            if let Some(v) = f("perAgentCallMaxUsd") {
                store.per_agent_call_max_usd = v;
            }
            if let Some(v) = u("maxRetriesPerRole") {
                store.max_retries_per_role = v;
            }
            if let Some(v) = u("maxWallClockMinutes") {
                store.max_wall_clock_minutes = v;
            }
            if let Some(v) = u("maxIterations") {
                store.max_iterations = v;
            }
            if let Some(v) = u("maxWorkers") {
                store.max_workers = v;
            }
        }
        (previous, store.clone())
    };

    // Persist the requested fields to ~/.coderight/config.toml so budgets and the
    // orchestrator model all survive a daemon restart.
    {
        use rightkit_agent_config::ConfigLoader;
        let loader = ConfigLoader::new();
        // Read-modify-write under the config lock on a blocking thread (the lock wait can last five
        // seconds and must not park an async worker): a concurrent save from another process is merged
        // in rather than overwritten.
        let edit_state = state.clone();
        // The write and the restore of the in-memory settings run together in one blocking task that
        // owns `previous_settings`, so a client that disconnects while this handler waits cannot skip
        // the restore (the task is not cancelled with the request).
        let saved = persist_or_restore(state.settings.clone(), previous_settings, move || {
            loader
                .update_user(|cfg| {
                    if let Some(id) = new_boss_model {
                        cfg.orchestration.boss_model = id;
                    }
                    if let Some(requested_mode) = body.role_model_mode {
                        let split_available = crate::model_policy::split_entitled(|feature| {
                            edit_state.has_license_feature(feature)
                        });
                        cfg.orchestration.role_model_mode = if split_available
                            && requested_mode
                                .eq_ignore_ascii_case(crate::model_policy::ROLE_MODEL_MODE_SPLIT)
                        {
                            crate::model_policy::ROLE_MODEL_MODE_SPLIT.to_string()
                        } else {
                            crate::model_policy::ROLE_MODEL_MODE_SINGLE.to_string()
                        };
                    }
                    if let Some(overrides) = &body.model_overrides {
                        for (agent, slug) in overrides {
                            let slug = slug.trim();
                            if slug.is_empty() {
                                cfg.orchestration.agents.remove(agent);
                            } else {
                                cfg.orchestration
                                    .agents
                                    .entry(agent.clone())
                                    .and_modify(|o| o.model_id = slug.to_string())
                                    .or_insert_with(|| {
                                        rightkit_agent_config::AgentOverride {
                                            model_id: slug.to_string(),
                                            provider_id: "openrouter".to_string(),
                                        }
                                    });
                            }
                        }
                    }
                    if let Some(remote) = body.remote {
                        if remote.clear_telegram_token.unwrap_or(false) {
                            cfg.remote.telegram_token = None;
                        } else if let Some(token) = remote.telegram_token {
                            let trimmed = token.trim();
                            if !trimmed.is_empty() {
                                cfg.remote.telegram_token = Some(trimmed.to_string());
                            }
                        }
                        if let Some(users) = remote.telegram_allowed_users {
                            cfg.remote.telegram_allowed_users = users;
                        }
                        if let Some(enabled) = remote.web_enabled {
                            cfg.remote.web_enabled = Some(enabled);
                        }
                    }
                    if let Some(routing) = body.routing {
                        cfg.orchestration.routing =
                            rightkit_agent_config::RoutingConfig {
                                capability_router: Some(routing.capability_router),
                                aggressiveness: routing.aggressiveness,
                            };
                    }
                    if let Some(orch) = body.orchestration {
                        cfg.orchestration.conductor_routing = Some(orch.conductor_routing);
                    }
                    if let Some(keep_resident) = body.keep_daemon_resident {
                        let mut store = lock_recover(&edit_state.settings, "state.settings");
                        store.keep_daemon_resident = keep_resident;
                    }
                    if let Some(b) = &body.budget {
                        apply_budget_patch(b, &mut cfg.orchestration.budget);
                    }
                })
                .map(|_| ())
        })
        .await;
        if let Err(e) = saved {
            eprintln!("[save_settings] config persist failed: {e}");
            // Nothing was persisted and `persist_or_restore` has already put the in-memory settings
            // back: report the failure rather than answering `saved: true` for settings that vanish on
            // restart.
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }

    let sessions = state
        .sessions
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .values()
        .cloned()
        .collect::<Vec<_>>();
    for session in sessions {
        session.publish_boundary_text("context", "settings updated");
        session.publish_boundary_text("world", "daemon settings updated");
    }
    Ok(Json(serde_json::json!({ "saved": true })))
}

async fn get_provider_key_status(
    Path(provider): Path<String>,
) -> Result<Json<ProviderKeyStatus>, StatusCode> {
    provider_key_status_for(&provider)
        .map(Json)
        .map_err(|_| StatusCode::BAD_REQUEST)
}

/// `DELETE /provider-keys/{provider}` — forget a stored key.
///
/// The app could store a key and never remove one, so a wrong or rotated key
/// could only be overwritten, never cleared, and a key whose keychain item had
/// become unwritable had no in-app exit at all.
async fn delete_provider_key(
    Path(provider): Path<String>,
    headers: HeaderMap,
) -> Result<Json<ProviderKeyStatus>, (StatusCode, Json<Value>)> {
    let plain = |code: StatusCode| (code, Json(json!({"error": code.to_string()})));
    enforce_trusted_source_authority(&headers).map_err(plain)?;
    validate_provider(&provider).map_err(|_| plain(StatusCode::BAD_REQUEST))?;

    let backend = rightkit_llm::agent_providers::OsKeyStore;
    rightkit_llm::agent_providers::remove_key(&backend, &provider).map_err(|reason| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": "provider key was not removed",
                "reason": reason,
                "provider": provider,
            })),
        )
    })?;

    provider_key_status_for(&provider)
        .map(Json)
        .map_err(|_| plain(StatusCode::BAD_REQUEST))
}

async fn save_provider_key(
    Path(provider): Path<String>,
    headers: HeaderMap,
    Json(body): Json<SaveProviderKey>,
) -> Result<Json<ProviderKeyStatus>, (StatusCode, Json<Value>)> {
    let plain = |code: StatusCode| (code, Json(json!({"error": code.to_string()})));
    enforce_trusted_source_authority(&headers).map_err(plain)?;
    validate_provider(&provider).map_err(|_| plain(StatusCode::BAD_REQUEST))?;
    let key = body.api_key.trim();
    if key.is_empty() {
        return Err(plain(StatusCode::BAD_REQUEST));
    }

    let backend = rightkit_llm::agent_providers::OsKeyStore;
    // write_key already explains itself. Discarding that reason with
    // `map_err(|_| INTERNAL_SERVER_ERROR)` produced a bodyless 500, so a user
    // whose OS keychain refused the write — the common case once a rebuilt or
    // re-signed binary no longer matches the existing item's ACL — saw a key
    // that simply never persisted, with nothing anywhere saying why.
    rightkit_llm::agent_providers::write_key(&backend, &provider, key).map_err(|reason| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": "provider key was not stored",
                "reason": reason,
                "provider": provider,
            })),
        )
    })?;

    provider_key_status_for(&provider)
        .map(Json)
        .map_err(|_| plain(StatusCode::BAD_REQUEST))
}

/// `POST /provider-keys/{provider}/test` — S:H13 test leg of provider
/// connect/test/rotate.
///
/// Verifies the credential the runtime will actually use, without ever
/// returning it. The response distinguishes missing (account not connected),
/// unreadable (keychain item exists but this build cannot open it), and the
/// live probe verdict (accepted / rejected / unverified / unreachable), so a
/// user with a broken keychain ACL gets a different answer than a user with a
/// wrong key. The secret itself never appears in the response body: the
/// serialized body is passed through the shared leakage gate before it is
/// emitted, and a gate failure becomes a generic 500 rather than output.
async fn test_provider_key(
    Path(provider): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let plain = |code: StatusCode| (code, Json(json!({"error": code.to_string()})));
    enforce_trusted_source_authority(&headers).map_err(plain)?;
    validate_provider(&provider).map_err(|_| plain(StatusCode::BAD_REQUEST))?;
    test_provider_key_payload_with(&rightkit_llm::agent_providers::OsKeyStore, &provider)
        .await
        .map(Json)
        .map_err(|(status, message)| (status, Json(json!({"error": message}))))
}

/// Backend-injectable payload builder so tests exercise the missing/unreadable
/// states without touching a real OS credential store or the network.
/// Production passes `&rightkit_llm::agent_providers::OsKeyStore`.
async fn test_provider_key_payload_with<B: rightkit_llm::agent_providers::KeyStoreBackend>(
    backend: &B,
    provider: &str,
) -> Result<Value, (StatusCode, String)> {
    let resolution = resolve_provider_credential_with(backend, provider)
        .map_err(|reason| (StatusCode::INTERNAL_SERVER_ERROR, reason))?;
    let (secret, mut body) = match resolution {
        ResolvedProviderCredential::Missing => {
            return Ok(json!({
                "provider": provider,
                "state": "missing",
                "valid": false,
                "checkedAt": now_iso(),
            }));
        }
        ResolvedProviderCredential::Unreadable { remedy } => {
            return Ok(json!({
                "provider": provider,
                "state": "unreadable",
                "valid": false,
                "remedy": remedy,
                "checkedAt": now_iso(),
            }));
        }
        ResolvedProviderCredential::Present { secret, source } => {
            (secret, json!({ "provider": provider, "source": source }))
        }
    };

    let outcome = probe_provider_credential(provider, &secret).await;
    let (state, valid, detail, provider_status) = probe_outcome_wire(&outcome);
    body["state"] = json!(state);
    body["valid"] = json!(valid);
    if let Some(detail) = detail {
        body["detail"] = json!(detail);
    }
    if let Some(status) = provider_status {
        body["providerStatus"] = json!(status);
    }
    body["checkedAt"] = json!(now_iso());

    // Leakage gate (S:H13 acceptance): the serialized body is checked for the
    // resolved secret before emission. This only fires on a future regression
    // that embeds credential material — exactly when it must block.
    let serialized = body.to_string();
    if rightkit_logs::KnownSecrets::from_values([secret.trim()]).is_exposed(&serialized) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "provider key test output failed the leakage guard".to_string(),
        ));
    }
    Ok(body)
}
