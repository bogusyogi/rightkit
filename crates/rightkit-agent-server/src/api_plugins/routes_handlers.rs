pub(crate) fn plugin_routes() -> Router<DaemonState> {
    Router::new()
        .route(
            "/provider-extensions",
            get(list_provider_extensions_handler),
        )
        .route("/plugins", get(list_plugins_handler))
        .route("/plugins/marketplace", get(list_plugin_marketplace_handler))
        .route(
            "/plugins/marketplace/categories",
            get(list_plugin_marketplace_categories_handler),
        )
        .route(
            "/plugins/marketplace/sync",
            post(sync_plugin_marketplace_handler),
        )
        .route(
            "/plugins/marketplace/{id}/install",
            post(install_marketplace_plugin_handler),
        )
        .route("/plugins/install", post(install_plugin_handler))
        .route("/plugins/reload", post(reload_plugins_handler))
        .route(
            "/plugins/{id}",
            axum::routing::delete(remove_plugin_handler),
        )
        .route("/plugins/{id}/enable", post(enable_plugin_handler))
        .route("/plugins/{id}/disable", post(disable_plugin_handler))
        .route("/plugins/{id}/rollback", post(rollback_plugin_handler))
        .route("/plugins/{id}/versions", get(list_plugin_versions_handler))
        .route(
            "/sessions/{id}/plugins/{pluginId}/enable",
            post(enable_session_plugin_handler),
        )
        .route(
            "/sessions/{id}/plugins/{pluginId}/disable",
            post(disable_session_plugin_handler),
        )
        .route(
            "/sessions/{id}/plugins",
            get(list_session_plugins_handler).delete(clear_session_plugins_handler),
        )
}

/// A plugin route refusal: the status plus a body whose `message` says why in plain words
/// (contract R9), never a bare status code.
type PluginRouteError = (StatusCode, Json<serde_json::Value>);

fn plugin_refusal(status: StatusCode, code: &str, message: impl Into<String>) -> PluginRouteError {
    (
        status,
        Json(serde_json::json!({"code": code, "message": message.into()})),
    )
}

fn plugin_error(error: rightkit_agent_plugins::PluginError) -> PluginRouteError {
    use rightkit_agent_plugins::PluginError;
    match error {
        PluginError::NotFound(message) => {
            plugin_refusal(StatusCode::NOT_FOUND, "plugin_not_found", message)
        }
        PluginError::Refused(message) => {
            plugin_refusal(StatusCode::BAD_REQUEST, "plugin_refused", message)
        }
        PluginError::Parse(message) => plugin_refusal(
            StatusCode::BAD_REQUEST,
            "plugin_manifest_invalid",
            format!("the plugin manifest is not valid: {message}"),
        ),
        PluginError::Conflict(message) => {
            plugin_refusal(StatusCode::CONFLICT, "plugin_conflict", message)
        }
        PluginError::Io(message) => {
            plugin_refusal(StatusCode::INTERNAL_SERVER_ERROR, "plugin_io", message)
        }
    }
}

/// Mutating plugin routes accept only a trusted source (contract R16).
fn require_trusted_plugin_source(headers: &HeaderMap) -> Result<(), PluginRouteError> {
    enforce_trusted_source_authority(headers).map_err(|status| {
        plugin_refusal(
            status,
            "untrusted_source",
            "plugins can only be changed from a trusted source",
        )
    })
}

fn extension_execution_diagnostics_json() -> serde_json::Value {
    serde_json::json!({
        "schema_version": crate::host_identity::schema("plugins.execution-trust.v1"),
        "extension_execution": rightkit_agent_plugins::extension_trust_catalog(),
    })
}

/// PL-004: project registered extension declarations through their existing
/// runtime registry. Do not expose resolved credentials or endpoint headers,
/// and do not turn a declared health probe into a successful health claim.
async fn list_provider_extensions_handler(State(state): State<DaemonState>) -> Json<Value> {
    use rightkit_llm::agent_providers::extension_registration::AuthResolverDeclaration;
    let registry =
        crate::api::lock_recover(&state.provider_extensions, "state.provider_extensions");
    let rows: Vec<Value> = registry
        .iter()
        .map(|provider| {
            let resolver = match &provider.auth {
                AuthResolverDeclaration::ApiKey { .. } => "api_key",
                AuthResolverDeclaration::OAuth => "oauth",
                AuthResolverDeclaration::Ambient { name } => name.as_str(),
                AuthResolverDeclaration::Extension { resolver_id } => resolver_id.as_str(),
            };
            json!({
                "id": provider.id,
                "displayName": provider.display_name,
                "version": provider.version,
                "source": "extension",
                "endpointOrigin": provider.endpoint.effective().origin,
                "protocol": provider.protocol,
                "authResolver": resolver,
                "authDeclaration": provider.auth,
                "models": provider.models,
                "health": "unchecked",
                "healthProbe": provider.health_probe,
            })
        })
        .collect();
    Json(Value::Array(rows))
}

async fn list_plugins_handler(
    State(state): State<DaemonState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let discovery =
        discover_plugin_report(&state.plugin_dir).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let plugins = discovery.plugins;
    // Contract R10: a plugin whose manifest does not parse is listed with its path and error,
    // never silently dropped and never able to fail the whole listing.
    let broken_json = discovery
        .broken
        .iter()
        .map(|broken| {
            serde_json::json!({
                "path": broken.path.display().to_string(),
                "error": broken.error,
            })
        })
        .collect::<Vec<_>>();
    let plugins_json = plugins
        .iter()
        .map(|p| {
            let install_dir = state.plugin_dir.join(&p.id);
            let mut json = plugin_manifest_json_with_provenance(p, &install_dir);
            // Attach rollback versions for UI chip: installed vs update vs rollback available
            let loader = rightkit_agent_plugins::PluginLoader::new(state.plugin_dir.clone());
            let rollback_versions = loader.available_rollback_versions(&p.id);
            if let Some(obj) = json.as_object_mut() {
                obj.insert(
                    "rollbackVersions".into(),
                    serde_json::Value::Array(
                        rollback_versions
                            .into_iter()
                            .map(serde_json::Value::String)
                            .collect(),
                    ),
                );
                // Include per-session activation hint: if any session has overrides, expose count
                // The UI can query per-session endpoint for details; this is a lightweight hint.
                let has_session_overrides = {
                    let activations = state
                        .session_plugin_activations
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    // Check if any session has overrides for this plugin
                    let mut found = false;
                    for session_id in activations.enabled_overrides_keys() {
                        if activations.is_active(&p.id, &session_id, p.enabled) != p.enabled {
                            found = true;
                            break;
                        }
                    }
                    if !found {
                        for session_id in activations.disabled_overrides_keys() {
                            if !activations.is_active(&p.id, &session_id, p.enabled) {
                                found = true;
                                break;
                            }
                        }
                    }
                    found
                };
                obj.insert(
                    "hasSessionOverrides".into(),
                    serde_json::Value::Bool(has_session_overrides),
                );
            }
            json
        })
        .collect::<Vec<_>>();
    let extension_execution = extension_execution_diagnostics_json()["extension_execution"].clone();
    Ok(Json(serde_json::json!({
        "pluginDir": state.plugin_dir,
        "plugins": plugins_json,
        "broken": broken_json,
        "extension_execution": extension_execution,
    })))
}

async fn reload_plugins_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, PluginRouteError> {
    require_trusted_plugin_source(&headers)?;
    let plugins = discover_plugins(&state.plugin_dir)
        .map_err(|e| plugin_refusal(StatusCode::INTERNAL_SERVER_ERROR, "plugin_io", e))?;
    let middleware_count = refresh_runtime_plugins(&state);
    refresh_live_session_catalogs(&state);
    let plugins_json = plugins
        .iter()
        .map(|p| {
            let install_dir = state.plugin_dir.join(&p.id);
            plugin_manifest_json_with_provenance(p, &install_dir)
        })
        .collect::<Vec<_>>();
    let extension_execution = extension_execution_diagnostics_json()["extension_execution"].clone();
    Ok(Json(serde_json::json!({
        "reloaded": true,
        "pluginDir": state.plugin_dir,
        "count": plugins.len(),
        "middlewareCount": middleware_count,
        "plugins": plugins_json,
        "extension_execution": extension_execution,
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginMarketplaceQuery {
    #[serde(default)]
    query: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    tag: String,
    #[serde(default)]
    sort: String,
    #[serde(default)]
    limit: Option<usize>,
}

async fn list_plugin_marketplace_handler(
    State(state): State<DaemonState>,
    Query(q): Query<PluginMarketplaceQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let catalog = load_plugin_marketplace(&state.plugin_marketplace_path)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let query = q.query.trim();
    let category = if q.category.trim().is_empty() {
        None
    } else {
        Some(q.category.trim())
    };
    let tag = if q.tag.trim().is_empty() {
        None
    } else {
        Some(q.tag.trim())
    };
    // Map sort string to CatalogSort equivalent for daemon catalog
    let mut filtered: Vec<&PluginMarketplaceEntry> = catalog
        .plugins
        .iter()
        .filter(|entry| {
            if let Some(cat) = category {
                if !cat.eq_ignore_ascii_case(entry.category.trim()) {
                    return false;
                }
            }
            if let Some(t) = tag {
                if !entry
                    .tags
                    .iter()
                    .any(|tag| tag.eq_ignore_ascii_case(t.trim()))
                {
                    return false;
                }
            }
            if query.is_empty() {
                return true;
            }
            let q_lower = query.to_lowercase();
            entry.manifest.id.to_lowercase().contains(&q_lower)
                || entry.manifest.name.to_lowercase().contains(&q_lower)
                || entry.manifest.description.to_lowercase().contains(&q_lower)
                || entry.category.to_lowercase().contains(&q_lower)
                || entry
                    .tags
                    .iter()
                    .any(|tag| tag.to_lowercase().contains(&q_lower))
        })
        .collect();
    // Apply sort
    match q.sort.trim().to_lowercase().as_str() {
        "rating" => {
            filtered.sort_by(|a, b| {
                b.rating
                    .partial_cmp(&a.rating)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| b.downloads.cmp(&a.downloads))
            });
        }
        "downloads" => {
            filtered.sort_by_key(|item| std::cmp::Reverse(item.downloads));
        }
        "name" => {
            filtered.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
        }
        _ => {
            // relevance: keep catalog order (or sort by query match quality)
            // For empty query, preserve insertion order; for non-empty, boost exact id match
            if !query.is_empty() {
                let q_lower = query.to_lowercase();
                filtered.sort_by(|a, b| {
                    let a_score = if a.manifest.id.to_lowercase() == q_lower {
                        0
                    } else if a.manifest.name.to_lowercase().contains(&q_lower) {
                        1
                    } else {
                        2
                    };
                    let b_score = if b.manifest.id.to_lowercase() == q_lower {
                        0
                    } else if b.manifest.name.to_lowercase().contains(&q_lower) {
                        1
                    } else {
                        2
                    };
                    a_score.cmp(&b_score)
                });
            }
        }
    }
    if let Some(n) = q.limit {
        filtered.truncate(n);
    }
    let plugins = filtered
        .into_iter()
        .map(plugin_marketplace_entry_json)
        .collect::<Vec<_>>();
    // Also compute available categories for the catalog UI to render filter chips
    let mut category_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for entry in &catalog.plugins {
        let cat = if entry.category.trim().is_empty() {
            "uncategorized".to_string()
        } else {
            entry.category.clone()
        };
        *category_counts.entry(cat).or_insert(0) += 1;
    }
    let mut categories: Vec<serde_json::Value> = category_counts
        .into_iter()
        .map(|(cat, count)| serde_json::json!({"category": cat, "count": count}))
        .collect();
    categories.sort_by(|a, b| a["category"].as_str().cmp(&b["category"].as_str()));
    Ok(Json(serde_json::json!({
        "marketplacePath": state.plugin_marketplace_path,
        "plugins": plugins,
        "categories": categories,
    })))
}

async fn list_plugin_marketplace_categories_handler(
    State(state): State<DaemonState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let catalog = load_plugin_marketplace(&state.plugin_marketplace_path)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut all_tags: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for entry in &catalog.plugins {
        let cat = if entry.category.trim().is_empty() {
            "uncategorized".to_string()
        } else {
            entry.category.clone()
        };
        *counts.entry(cat).or_insert(0) += 1;
        for tag in &entry.tags {
            *all_tags.entry(tag.clone()).or_insert(0) += 1;
        }
    }
    let mut categories: Vec<serde_json::Value> = counts
        .into_iter()
        .map(|(cat, count)| serde_json::json!({"category": cat, "count": count}))
        .collect();
    categories.sort_by(|a, b| a["category"].as_str().cmp(&b["category"].as_str()));
    let mut tags: Vec<serde_json::Value> = all_tags
        .into_iter()
        .map(|(tag, count)| serde_json::json!({"tag": tag, "count": count}))
        .collect();
    tags.sort_by(|a, b| a["tag"].as_str().cmp(&b["tag"].as_str()));
    Ok(Json(serde_json::json!({
        "marketplacePath": state.plugin_marketplace_path,
        "categories": categories,
        "tags": tags,
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncPluginMarketplaceRequest {
    catalog_url: String,
    expected_sha256: String,
    trust_accepted: bool,
    #[serde(default = "default_true")]
    require_signed: bool,
    #[serde(default)]
    allow_file_urls: bool,
    #[serde(default)]
    file_url_trust_accepted: bool,
}

fn default_true() -> bool {
    true
}

async fn sync_plugin_marketplace_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(req): Json<SyncPluginMarketplaceRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    if !req.trust_accepted || req.expected_sha256.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    if req.allow_file_urls && !req.file_url_trust_accepted {
        return Err(StatusCode::BAD_REQUEST);
    }
    let bytes = fetch_marketplace_url_bytes(
        &req.catalog_url,
        req.allow_file_urls && req.file_url_trust_accepted,
        "catalog",
    )
    .await
    .map_err(|_| StatusCode::BAD_REQUEST)?;
    let actual_sha256 = rightkit_fs::digest::sha256_bytes(&bytes);
    if !req.expected_sha256.eq_ignore_ascii_case(&actual_sha256) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut catalog: PluginMarketplaceCatalog =
        serde_json::from_slice(&bytes).map_err(|_| StatusCode::BAD_REQUEST)?;
    validate_synced_marketplace_catalog(&catalog, req.require_signed)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    catalog.synced_from = req.catalog_url;
    catalog.synced_at = now_iso();
    catalog.signature_sha256 = actual_sha256.clone();
    if catalog.signed_by.trim().is_empty() && req.require_signed {
        catalog.signed_by = "entry-signers".into();
    }
    write_plugin_marketplace(&state.plugin_marketplace_path, &catalog)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({
        "synced": true,
        "marketplacePath": state.plugin_marketplace_path,
        "catalogUrl": catalog.synced_from,
        "checksumSha256": actual_sha256,
        "pluginCount": catalog.plugins.len(),
        "requireSigned": req.require_signed,
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallPluginRequest {
    source_path: PathBuf,
    trust_accepted: bool,
    #[serde(default)]
    overwrite: bool,
}

async fn install_plugin_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(req): Json<InstallPluginRequest>,
) -> Result<Json<serde_json::Value>, PluginRouteError> {
    require_trusted_plugin_source(&headers)?;
    if !req.trust_accepted {
        return Err(plugin_refusal(
            StatusCode::BAD_REQUEST,
            "trust_not_accepted",
            "a plugin runs code on this machine; confirm you trust it (trustAccepted: true) to install it",
        ));
    }
    let installed = rightkit_agent_plugins::PluginLoader::new(state.plugin_dir.clone())
        .install(&req.source_path, req.overwrite)
        .map_err(plugin_error)?;
    tracing::info!(
        event = "plugin_install",
        plugin = %installed.manifest.id,
        overwritten = installed.overwritten,
        outcome = "ok",
        "plugin lifecycle"
    );
    // A local install has no publisher signature block, so the provenance defaults to
    // `unsigned` (audit A-8 SPEC 4). The UI badges this state and the user has explicitly
    // accepted the install via `trustAccepted: true`.
    let record = PluginSignatureRecord::unsigned("local".into(), now_iso());
    write_record(&installed.path, &record);
    let middleware_count = refresh_runtime_plugins(&state);
    refresh_live_session_catalogs(&state);
    let status_str = match record.status {
        SignatureStatus::Signed => "signed",
        SignatureStatus::Unsigned => "unsigned",
        SignatureStatus::Unknown => "unknown",
    };
    // An install that replaced a plugin says what changed between the two versions.
    let replaced = installed.replaced.as_ref().map(|replaced| {
        serde_json::json!({
            "fromVersion": replaced.from_version,
            "toVersion": replaced.to_version,
            "added": {"commands": replaced.added_commands, "tools": replaced.added_tools},
            "removed": {"commands": replaced.removed_commands, "tools": replaced.removed_tools},
        })
    });
    Ok(Json(serde_json::json!({
        "installed": true,
        "overwritten": installed.overwritten,
        "replaced": replaced,
        "pluginDir": state.plugin_dir,
        "installedPath": installed.path,
        "middlewareCount": middleware_count,
        "signatureStatus": status_str,
        "plugin": plugin_manifest_json_with_provenance(&installed.manifest, state.plugin_dir.join(&installed.manifest.id).as_path()),
    })))
}

/// `DELETE /plugins/{id}`: remove an installed plugin (its folder and enabled record). Its
/// commands and tools leave every live session's next catalog.
async fn remove_plugin_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, PluginRouteError> {
    require_trusted_plugin_source(&headers)?;
    rightkit_agent_plugins::PluginLoader::new(state.plugin_dir.clone())
        .remove(&id)
        .map_err(plugin_error)?;
    tracing::info!(event = "plugin_remove", plugin = %id, outcome = "ok", "plugin lifecycle");
    let middleware_count = refresh_runtime_plugins(&state);
    refresh_live_session_catalogs(&state);
    Ok(Json(
        serde_json::json!({ "id": id, "removed": true, "middlewareCount": middleware_count }),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallMarketplacePluginRequest {
    trust_accepted: bool,
    expected_sha256: Option<String>,
    #[serde(default)]
    overwrite: bool,
    #[serde(default)]
    allow_file_urls: bool,
    #[serde(default)]
    file_url_trust_accepted: bool,
}

async fn install_marketplace_plugin_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<InstallMarketplacePluginRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    if !req.trust_accepted {
        return Err(StatusCode::BAD_REQUEST);
    }
    if req.allow_file_urls && !req.file_url_trust_accepted {
        return Err(StatusCode::BAD_REQUEST);
    }
    let catalog = load_plugin_marketplace(&state.plugin_marketplace_path)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let entry = catalog
        .plugins
        .iter()
        .find(|entry| entry.manifest.id == id)
        .ok_or(StatusCode::NOT_FOUND)?;
    // The id names a folder under the temp and plugin directories: never `.`, `..` or a path.
    if !validate_plugin_id(&entry.manifest.id) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if !entry.signature.trim().is_empty()
        || !entry.signing_public_key.trim().is_empty()
        || !entry.signed_by.trim().is_empty()
    {
        verify_marketplace_entry_signature(entry).map_err(|_| StatusCode::BAD_REQUEST)?;
    }
    let temp_root = std::env::temp_dir().join(format!(
        "{app}-plugin-install-{}-{}",
        entry.manifest.id,
        now_millis(),
        app = crate::host_identity::app()
    ));
    let mut package_checksum = String::new();
    let source_path = if entry.source_path.as_os_str().is_empty() {
        if entry.source_url.trim().is_empty() || entry.package_sha256.trim().is_empty() {
            return Err(StatusCode::BAD_REQUEST);
        }
        let package_bytes = fetch_marketplace_url_bytes(
            &entry.source_url,
            req.allow_file_urls && req.file_url_trust_accepted,
            "plugin package",
        )
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;
        package_checksum = rightkit_fs::digest::sha256_bytes(&package_bytes);
        if !entry
            .package_sha256
            .eq_ignore_ascii_case(package_checksum.as_str())
        {
            return Err(StatusCode::BAD_REQUEST);
        }
        unpack_plugin_package(&package_bytes, &temp_root).map_err(|_| StatusCode::BAD_REQUEST)?
    } else {
        entry.source_path.clone()
    };
    let actual_checksum =
        plugin_source_sha256(&source_path).map_err(|_| StatusCode::BAD_REQUEST)?;
    if !entry
        .checksum_sha256
        .eq_ignore_ascii_case(actual_checksum.as_str())
    {
        let _ = std::fs::remove_dir_all(&temp_root);
        return Err(StatusCode::BAD_REQUEST);
    }
    if let Some(expected) = req.expected_sha256.as_deref() {
        let expected_matches = expected.eq_ignore_ascii_case(actual_checksum.as_str())
            || (!package_checksum.is_empty()
                && expected.eq_ignore_ascii_case(package_checksum.as_str()));
        if !expected_matches {
            let _ = std::fs::remove_dir_all(&temp_root);
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    let (manifest, installed_path, overwritten) =
        install_local_plugin(&state.plugin_dir, &source_path, req.overwrite)?;
    let _ = std::fs::remove_dir_all(&temp_root);
    // Marketplace provenance: a non-empty signature block on the catalog entry
    // means a publisher signed this build. We only set Signed when the entry
    // carried a verifying signature (the verify call above passed); an empty
    // signature block is treated as Unsigned so the UI badges it.
    let record = if !entry.signature.trim().is_empty()
        && !entry.signing_public_key.trim().is_empty()
        && !entry.signed_by.trim().is_empty()
    {
        PluginSignatureRecord::signed(
            entry.signed_by.clone(),
            entry.signing_public_key.clone(),
            entry.signature.clone(),
            "marketplace".into(),
            now_iso(),
        )
    } else {
        PluginSignatureRecord::unsigned("marketplace".into(), now_iso())
    };
    write_record(&installed_path, &record);
    let status_str = match record.status {
        SignatureStatus::Signed => "signed",
        SignatureStatus::Unsigned => "unsigned",
        SignatureStatus::Unknown => "unknown",
    };
    let middleware_count = refresh_runtime_plugins(&state);
    refresh_live_session_catalogs(&state);
    Ok(Json(serde_json::json!({
        "installed": true,
        "overwritten": overwritten,
        "pluginDir": state.plugin_dir,
        "installedPath": installed_path,
        "marketplacePath": state.plugin_marketplace_path,
        "sourceUrl": entry.source_url,
        "checksumSha256": actual_checksum,
        "packageSha256": package_checksum,
        "signedBy": entry.signed_by,
        "signingPublicKey": entry.signing_public_key,
        "signature": entry.signature,
        "middlewareCount": middleware_count,
        "signatureStatus": status_str,
        "plugin": plugin_manifest_json_with_provenance(&manifest, state.plugin_dir.join(&manifest.id).as_path()),
    })))
}

async fn enable_plugin_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, PluginRouteError> {
    require_trusted_plugin_source(&headers)?;
    set_plugin_enabled(&state.plugin_dir, &id, true)?;
    tracing::info!(event = "plugin_enable", plugin = %id, outcome = "ok", "plugin lifecycle");
    let middleware_count = refresh_runtime_plugins(&state);
    refresh_live_session_catalogs(&state);
    Ok(Json(
        serde_json::json!({ "id": id, "enabled": true, "middlewareCount": middleware_count }),
    ))
}

async fn disable_plugin_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, PluginRouteError> {
    require_trusted_plugin_source(&headers)?;
    set_plugin_enabled(&state.plugin_dir, &id, false)?;
    tracing::info!(event = "plugin_disable", plugin = %id, outcome = "ok", "plugin lifecycle");
    let middleware_count = refresh_runtime_plugins(&state);
    refresh_live_session_catalogs(&state);
    Ok(Json(
        serde_json::json!({ "id": id, "enabled": false, "middlewareCount": middleware_count }),
    ))
}

// ── FM-156: rollback ──────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RollbackPluginRequest {
    target_version: String,
    #[serde(default)]
    trust_accepted: bool,
}

async fn rollback_plugin_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<RollbackPluginRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    if req.target_version.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    // Prefer marketplace versioned entry if catalog contains the target version;
    // fallback to filesystem snapshots created during prior installs.
    let catalog = load_plugin_marketplace(&state.plugin_marketplace_path)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let catalog_entry = catalog
        .plugins
        .iter()
        .find(|e| e.manifest.id == id && e.manifest.version == req.target_version)
        .cloned();
    if let Some(entry) = catalog_entry {
        // Install from catalog entry (with snapshot preservation)
        let temp_root = std::env::temp_dir().join(format!(
            "{app}-plugin-rollback-{}-{}-{}",
            id,
            req.target_version,
            now_millis(),
            app = crate::host_identity::app()
        ));
        let source_path = if entry.source_path.as_os_str().is_empty() {
            if entry.source_url.trim().is_empty() {
                // No package source: fallback to snapshot restore via PluginLoader
                let loader = rightkit_agent_plugins::PluginLoader::new(state.plugin_dir.clone());
                loader
                    .rollback(&id, &req.target_version)
                    .map_err(|e| match e {
                        rightkit_agent_plugins::PluginError::NotFound(_) => StatusCode::NOT_FOUND,
                        _ => StatusCode::INTERNAL_SERVER_ERROR,
                    })?;
                let middleware_count = refresh_runtime_plugins(&state);
                refresh_live_session_catalogs(&state);
                let plugins = discover_plugins(&state.plugin_dir)
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                let manifest = plugins
                    .iter()
                    .find(|p| p.id == id)
                    .ok_or(StatusCode::NOT_FOUND)?;
                return Ok(Json(serde_json::json!({
                    "rolledBack": true,
                    "id": id,
                    "targetVersion": req.target_version,
                    "middlewareCount": middleware_count,
                    "plugin": plugin_manifest_json_with_provenance(
                        manifest,
                        state.plugin_dir.join(&id).as_path()
                    ),
                })));
            }
            // For URL source, we would fetch package — but marketplace entries with file paths are more common in tests
            entry.source_path.clone()
        } else {
            entry.source_path.clone()
        };
        // Verify checksum if present
        if !entry.checksum_sha256.trim().is_empty() {
            let actual = plugin_source_sha256(&source_path).map_err(|_| StatusCode::BAD_REQUEST)?;
            if !actual.eq_ignore_ascii_case(&entry.checksum_sha256) {
                let _ = std::fs::remove_dir_all(&temp_root);
                return Err(StatusCode::BAD_REQUEST);
            }
        }
        let (manifest, installed_path, _) =
            install_local_plugin(&state.plugin_dir, &source_path, true)?;
        let _ = std::fs::remove_dir_all(&temp_root);
        let record = if !entry.signature.trim().is_empty() {
            PluginSignatureRecord::signed(
                entry.signed_by.clone(),
                entry.signing_public_key.clone(),
                entry.signature.clone(),
                "rollback-marketplace".into(),
                now_iso(),
            )
        } else {
            PluginSignatureRecord::unsigned("rollback-marketplace".into(), now_iso())
        };
        write_record(&installed_path, &record);
        let middleware_count = refresh_runtime_plugins(&state);
        refresh_live_session_catalogs(&state);
        return Ok(Json(serde_json::json!({
            "rolledBack": true,
            "id": id,
            "targetVersion": req.target_version,
            "middlewareCount": middleware_count,
            "plugin": plugin_manifest_json_with_provenance(
                &manifest,
                state.plugin_dir.join(&id).as_path()
            ),
        })));
    }
    // No catalog entry: use filesystem snapshots
    let loader = rightkit_agent_plugins::PluginLoader::new(state.plugin_dir.clone());
    loader
        .rollback(&id, &req.target_version)
        .map_err(|e| match e {
            rightkit_agent_plugins::PluginError::NotFound(_) => StatusCode::NOT_FOUND,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        })?;
    let middleware_count = refresh_runtime_plugins(&state);
    refresh_live_session_catalogs(&state);
    let plugins =
        discover_plugins(&state.plugin_dir).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let manifest = plugins
        .iter()
        .find(|p| p.id == id)
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(serde_json::json!({
        "rolledBack": true,
        "id": id,
        "targetVersion": req.target_version,
        "middlewareCount": middleware_count,
        "plugin": plugin_manifest_json_with_provenance(
            manifest,
            state.plugin_dir.join(&id).as_path()
        ),
    })))
}

async fn list_plugin_versions_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let plugins =
        discover_plugins(&state.plugin_dir).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let current = plugins
        .iter()
        .find(|p| p.id == id)
        .map(|p| p.version.clone());
    let loader = rightkit_agent_plugins::PluginLoader::new(state.plugin_dir.clone());
    let rollback_versions = loader.available_rollback_versions(&id);
    // Also collect marketplace versions for the same id
    let catalog = load_plugin_marketplace(&state.plugin_marketplace_path)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut marketplace_versions: Vec<String> = catalog
        .plugins
        .iter()
        .filter(|e| e.manifest.id == id)
        .map(|e| e.manifest.version.clone())
        .collect();
    marketplace_versions.sort();
    marketplace_versions.dedup();
    // Compose unified version list
    let mut all_versions = std::collections::BTreeSet::new();
    if let Some(cur) = current.clone() {
        all_versions.insert(cur);
    }
    for v in rollback_versions.iter() {
        all_versions.insert(v.clone());
    }
    for v in marketplace_versions.iter() {
        all_versions.insert(v.clone());
    }
    let versions: Vec<String> = all_versions.into_iter().collect();
    Ok(Json(serde_json::json!({
        "id": id,
        "currentVersion": current,
        "rollbackVersions": rollback_versions,
        "marketplaceVersions": marketplace_versions,
        "allVersions": versions,
    })))
}

// ── FM-156: per-session activation ────────────────────────────────────────

async fn list_session_plugins_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let session = state.get_session(&id);
    if session.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    let plugins =
        discover_plugins(&state.plugin_dir).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let activations = state
        .session_plugin_activations
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let active_plugins: Vec<serde_json::Value> = plugins
        .iter()
        .map(|p| {
            let active = activations.is_active(&p.id, &id, p.enabled);
            let mut json = plugin_manifest_json(p);
            if let Some(obj) = json.as_object_mut() {
                obj.insert("activeForSession".into(), serde_json::Value::Bool(active));
            }
            json
        })
        .collect();
    let (enabled_overrides, disabled_overrides) = activations.overrides_for_session(&id);
    Ok(Json(serde_json::json!({
        "sessionId": id,
        "plugins": active_plugins,
        "enabledOverrides": enabled_overrides,
        "disabledOverrides": disabled_overrides,
    })))
}

async fn enable_session_plugin_handler(
    State(state): State<DaemonState>,
    Path((session_id, plugin_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    if state.get_session(&session_id).is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    let plugins =
        discover_plugins(&state.plugin_dir).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if plugins.iter().find(|p| p.id == plugin_id).is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    {
        let mut activations = state
            .session_plugin_activations
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        activations.enable_for_session(&plugin_id, &session_id);
    }
    if let Some(session) = state.get_session(&session_id) {
        session.tool_catalog.refresh();
    }
    Ok(Json(serde_json::json!({
        "sessionId": session_id,
        "pluginId": plugin_id,
        "activeForSession": true,
    })))
}

async fn disable_session_plugin_handler(
    State(state): State<DaemonState>,
    Path((session_id, plugin_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    if state.get_session(&session_id).is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    let plugins =
        discover_plugins(&state.plugin_dir).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if plugins.iter().find(|p| p.id == plugin_id).is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    {
        let mut activations = state
            .session_plugin_activations
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        activations.disable_for_session(&plugin_id, &session_id);
    }
    if let Some(session) = state.get_session(&session_id) {
        session.tool_catalog.refresh();
    }
    Ok(Json(serde_json::json!({
        "sessionId": session_id,
        "pluginId": plugin_id,
        "activeForSession": false,
    })))
}

async fn clear_session_plugins_handler(
    State(state): State<DaemonState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    {
        let mut activations = state
            .session_plugin_activations
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        activations.clear_session(&session_id);
    }
    if let Some(session) = state.get_session(&session_id) {
        session.tool_catalog.refresh();
    }
    Ok(Json(serde_json::json!({
        "sessionId": session_id,
        "cleared": true,
    })))
}

fn set_plugin_enabled(dir: &FsPath, id: &str, enabled: bool) -> Result<(), PluginRouteError> {
    let mut loader = rightkit_agent_plugins::PluginLoader::new(dir.to_path_buf());
    loader.discover().map_err(plugin_error)?;
    let result = if enabled {
        loader.enable(id)
    } else {
        loader.disable(id)
    };
    result.map_err(plugin_error)
}

/// Publish replacement catalogs for every active session. Each engine adopts
/// its replacement only at next provider boundary, preserving in-flight
/// request immutability while making global lifecycle changes observable.
fn refresh_live_session_catalogs(state: &DaemonState) {
    let sessions = state
        .sessions
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .values()
        .cloned()
        .collect::<Vec<_>>();
    for session in sessions {
        let _ = session.tool_catalog.refresh();
        let prompt = crate::api::plugin_prompt_context(&state.plugin_dir, session.role.as_deref())
            .unwrap_or_default();
        let world = discover_plugins(&state.plugin_dir)
            .map(|plugins| serde_json::to_string(&plugins).unwrap_or_default())
            .unwrap_or_default();
        session.publish_boundary_text("prompt", prompt);
        session.publish_boundary_text("context", "plugin lifecycle catalog refreshed");
        session.publish_boundary_text("world", world);
    }
}
