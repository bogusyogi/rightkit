use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path as FsPath;
use std::sync::Arc;

use rightkit_agent_engine_protocol::{uuid_v7, ProtocolEvent, TranscriptMessage};

use crate::api::{
    away_summary_line, ensure_bound_spec_can_complete, lock_recover, normalize_driver_kind,
    normalize_effort, normalize_mcp_channel, normalize_session_status, normalize_thinking_budget,
    now_iso, require_license_feature, same_path, session_row, touch_session, transcript_created_at,
    transcript_for_session, untrusted_content_item, update_session_route, AwaySummaryQuery,
    CreateSession, DaemonState, LiveSession, PatchSession, SessionRuntimeOptions,
    SessionSearchQuery,
};
use crate::api_execution::{
    resolve_execution_backend_for_workspace, resolve_execution_workspace_id_for_session,
    resolve_execution_workspace_path_for_session,
};
use crate::api_permissions::enforce_trusted_source_authority;
use crate::api_toolsets::{
    apply_toolset_profile, resolve_toolset_profile, resolve_toolset_profile_with_config,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ForkSessionRequest {
    id: Option<String>,
    title: Option<String>,
    /// Durable idempotency key (CR3 #57): an identical retry replays the
    /// original response instead of creating a second session.
    #[serde(default)]
    nonce: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionListQuery {
    tag: Option<String>,
    status: Option<String>,
    q: Option<String>,
    sort: Option<String>,
    order: Option<String>,
    #[serde(default)]
    include_subagents: bool,
}

/// Longest first-message preview a session-list row carries, in characters
/// (including the trailing ellipsis when truncated).
pub(crate) const SESSION_PREVIEW_MAX_CHARS: usize = 120;

/// Collapse whitespace to one line and bound it to `SESSION_PREVIEW_MAX_CHARS`.
pub(crate) fn bounded_session_preview(text: &str) -> String {
    let single_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= SESSION_PREVIEW_MAX_CHARS {
        return single_line;
    }
    let mut out: String = single_line
        .chars()
        .take(SESSION_PREVIEW_MAX_CHARS - 1)
        .collect();
    out.push('\u{2026}');
    out
}

/// Preview of the first user message held in a live transcript.
fn transcript_session_preview(messages: &[TranscriptMessage]) -> Option<String> {
    messages.iter().find_map(|message| match message {
        TranscriptMessage::User { content, .. } => Some(bounded_session_preview(content)),
        _ => None,
    })
}

pub(crate) async fn list_sessions(
    State(state): State<DaemonState>,
    Query(query): Query<SessionListQuery>,
) -> Result<Json<Vec<serde_json::Value>>, StatusCode> {
    let live = state.list_sessions();
    let mut out: Vec<serde_json::Value> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for s in &live {
        seen.insert(s.id.clone());
        let mut row = session_row(s);
        row["preview"] = serde_json::json!(transcript_session_preview(&lock_recover(
            &s.transcript,
            "session.transcript"
        ))
        .unwrap_or_default());
        // A live engine owns runtime status; persisted status is a restart
        // snapshot and must not overwrite an active session projection.
        row["status"] = serde_json::json!("running");
        out.push(row);
    }
    // Merge in persisted sessions. Live sessions retain runtime status while
    // labels/history metadata and updatedAt are patched in; sessions not in memory are added as-is (could be
    // "dormant", "interrupted", "done", etc.).
    if let Some(storage) = &state.storage {
        let storage2 = storage.clone();
        // Previews come from storage only for sessions not held live; live rows
        // read their preview from the in-memory transcript above.
        let live_ids = seen.clone();
        let (rows, history_ids, stored_previews) = tokio::task::spawn_blocking(move || {
            let store = storage2.blocking_lock();
            let rows = store.list_sessions().unwrap_or_default();
            let previews: std::collections::HashMap<String, String> = rows
                .iter()
                .filter(|r| !live_ids.contains(&r.id))
                .filter_map(|r| {
                    store
                        .first_user_message_text(&r.id)
                        .ok()
                        .flatten()
                        .map(|text| (r.id.clone(), bounded_session_preview(&text)))
                })
                .collect();
            (
                rows,
                store.session_ids_with_chat_history().unwrap_or_default(),
                previews,
            )
        })
        .await
        .unwrap_or_default();
        if !rows.is_empty() {
            for r in rows {
                let updated = if r.updated_at.is_empty() {
                    r.created_at.clone()
                } else {
                    r.updated_at.clone()
                };
                if seen.contains(&r.id) {
                    // Patch the live entry with persisted metadata. Live engine
                    // ownership is authoritative for runtime handles, while
                    // storage is authoritative for operator-edited labels.
                    if let Some(entry) = out.iter_mut().find(|e| e["id"].as_str() == Some(&r.id)) {
                        let cfg: serde_json::Value =
                            serde_json::from_str(&r.config_json).unwrap_or(serde_json::Value::Null);
                        entry["title"] = serde_json::json!(r.title);
                        if let Some(model) = cfg.get("model").and_then(|v| v.as_str()) {
                            entry["model"] = serde_json::json!(model);
                        }
                        entry["inferenceSelection"] = cfg
                            .get("inferenceSelection")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        entry["executionBackend"] = cfg
                            .get("executionBackend")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        entry["executionWorkspaceId"] = cfg
                            .get("executionWorkspaceId")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        entry["executionNetworkProfile"] = cfg
                            .get("executionNetworkProfile")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        entry["driverKind"] = cfg
                            .get("driverKind")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!("native"));
                        entry["mcpChannel"] = cfg
                            .get("mcpChannel")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!("desktop"));
                        entry["specId"] = cfg
                            .get("specId")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        entry["thinkingBudgetTokens"] = cfg
                            .get("thinkingBudgetTokens")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        entry["disableThinking"] = cfg
                            .get("disableThinking")
                            .cloned()
                            .unwrap_or(serde_json::Value::Bool(false));
                        entry["safeMode"] = cfg
                            .get("safeMode")
                            .cloned()
                            .unwrap_or(serde_json::Value::Bool(false));
                        entry["forkedFromSessionId"] = cfg
                            .get("forkedFromSessionId")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        entry["updatedAt"] = serde_json::json!(updated);
                        entry["hasHistory"] = serde_json::json!(history_ids.contains(&r.id));
                        entry["tags"] = serde_json::json!(serde_json::from_str::<Vec<String>>(
                            &r.tags
                        )
                        .unwrap_or_default());
                        // Persisted recovery marker (P1 J1) takes priority —
                        // it's set by startup housekeeping + on every
                        // journal.open() and cleared on journal.close().
                        entry["recoverable"] = match r.recovery_marker.as_deref() {
                            Some(json) => {
                                serde_json::from_str(json).unwrap_or(serde_json::Value::Null)
                            }
                            None => serde_json::Value::Null,
                        };
                    }
                    continue;
                }
                let cfg: serde_json::Value =
                    serde_json::from_str(&r.config_json).unwrap_or(serde_json::Value::Null);
                out.push(serde_json::json!({
                    "id": r.id,
                    "title": r.title,
                    "model": cfg.get("model").and_then(|v| v.as_str()).unwrap_or(""),
                    "providerId": cfg.get("providerId").cloned().unwrap_or(serde_json::Value::Null),
                    "inferenceSelection": cfg.get("inferenceSelection").cloned().unwrap_or(serde_json::Value::Null),
                    "workspaceRoot": crate::api::api_workspace_root(&r.workspace_root),
                    "createdAt": r.created_at,
                    "updatedAt": updated,
                    "preview": stored_previews.get(&r.id).cloned().unwrap_or_default(),
					"hasHistory": history_ids.contains(&r.id),
                    "role": r.role,
                    "parentSessionId": r.parent_session_id,
                    "status": r.status,
                    "executionBackend": cfg.get("executionBackend").cloned().unwrap_or(serde_json::Value::Null),
                    "executionWorkspaceId": cfg.get("executionWorkspaceId").cloned().unwrap_or(serde_json::Value::Null),
                    "executionNetworkProfile": cfg.get("executionNetworkProfile").cloned().unwrap_or(serde_json::Value::Null),
                    "driverKind": cfg.get("driverKind").cloned().unwrap_or_else(|| serde_json::json!("native")),
                    "mcpChannel": cfg.get("mcpChannel").cloned().unwrap_or_else(|| serde_json::json!("desktop")),
                    "specId": cfg.get("specId").cloned().unwrap_or(serde_json::Value::Null),
                    "thinkingBudgetTokens": cfg.get("thinkingBudgetTokens").cloned().unwrap_or(serde_json::Value::Null),
                    "disableThinking": cfg.get("disableThinking").cloned().unwrap_or(serde_json::Value::Bool(false)),
                    "safeMode": cfg.get("safeMode").cloned().unwrap_or(serde_json::Value::Bool(false)),
                    "forkedFromSessionId": cfg.get("forkedFromSessionId").cloned().unwrap_or(serde_json::Value::Null),
                    // Match the live session_row() shape so the frontend sees the
                    // same fields whether a session is live or dormant.
                    "allowedTools": cfg.get("allowedTools").cloned().unwrap_or_else(|| serde_json::json!([])),
                    "disallowedTools": cfg.get("disallowedTools").cloned().unwrap_or_else(|| serde_json::json!([])),
                    "toolset": cfg.get("toolset").cloned().unwrap_or(serde_json::Value::Null),
                    "activeSkills": cfg.get("activeSkills").cloned().unwrap_or(serde_json::json!([])),
                    "workspaceTrust": cfg.get("workspaceTrust").cloned().unwrap_or_else(|| serde_json::json!("untrusted")),
                    "effort": cfg.get("effort").cloned().unwrap_or(serde_json::Value::Null),
                    "tags": serde_json::from_str::<Vec<String>>(&r.tags).unwrap_or_default(),
                    // Dormant sessions have no in-memory UsageAccount; cost is
                    // not persisted (only tracked while the session is live),
                    // so it is unknown here — null, matching the "unknown, not
                    // zero" contract used for unpriced models on live sessions.
                    "costUsd": serde_json::Value::Null,
                    // Persistent recovery marker (P1 J1) read from the row so
                    // a dormant session stays "recoverable" across list
                    // calls until the next startup reconciles (or the user
                    // resumes and cleanly completes).
                    "recoverable": match r.recovery_marker.as_deref() {
                        Some(json) => serde_json::from_str(json)
                            .unwrap_or(serde_json::Value::Null),
                        None => serde_json::Value::Null,
                    },
                }));
            }
        }
    }
    // Fallback recovery-marker FS read: ONLY for rows that don't already
    // carry a persisted marker. Persisted markers (set by startup
    // housekeeping + on every journal.open()/close()) are authoritative;
    // the FS read is a last-resort for the brief window between a fresh
    // session creation and the housekeeping sweep.
    if let Some(journal_dir) = &state.journal_dir {
        for row in out.iter_mut() {
            if !row["recoverable"].is_null() {
                continue;
            }
            let Some(id) = row["id"].as_str().map(|s| s.to_string()) else {
                continue;
            };
            let marker = crate::journal::recovery_marker_for(journal_dir, &id).await;
            if let Some(m) = marker {
                row["recoverable"] = serde_json::to_value(m).unwrap_or(serde_json::Value::Null);
            }
        }
    }
    // Recents and the default list contain only top-level chats. Child worker
    // sessions remain available to the background-agent surface on request.
    if !query.include_subagents {
        out.retain(|s| s["parentSessionId"].is_null());
    }
    // Apply optional tag filter.
    if let Some(ref tag) = query.tag {
        out.retain(|s| {
            s["tags"]
                .as_array()
                .map(|arr| arr.iter().any(|v| v.as_str() == Some(tag)))
                .unwrap_or(false)
        });
    }
    // Apply optional status filter.
    if let Some(ref status) = query.status {
        out.retain(|s| s["status"].as_str() == Some(status));
    }
    if let Some(q) = query.q.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        let q = q.to_lowercase();
        out.retain(|session| {
            ["id", "title", "workspaceRoot"]
                .iter()
                .filter_map(|key| session[*key].as_str())
                .any(|value| value.to_lowercase().contains(&q))
        });
    }
    let sort = query.sort.as_deref().unwrap_or("created");
    let descending = match query.order.as_deref().unwrap_or("desc") {
        "asc" => false,
        "desc" => true,
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    if !matches!(sort, "updated" | "created" | "title") {
        return Err(StatusCode::BAD_REQUEST);
    }
    out.sort_by(|a, b| {
        let (left, right) = match sort {
            "title" => (
                a["title"].as_str().unwrap_or("").to_lowercase(),
                b["title"].as_str().unwrap_or("").to_lowercase(),
            ),
            "created" => (
                a["createdAt"].as_str().unwrap_or("").to_string(),
                b["createdAt"].as_str().unwrap_or("").to_string(),
            ),
            _ => (
                a["updatedAt"].as_str().unwrap_or("").to_string(),
                b["updatedAt"].as_str().unwrap_or("").to_string(),
            ),
        };
        if descending {
            right.cmp(&left)
        } else {
            left.cmp(&right)
        }
    });
    Ok(Json(out))
}
/// Full-text search across past sessions' event payloads (cross-session recall).
/// Returns the best-ranked snippet per matching session. Requires persistence.
pub(crate) async fn search_sessions_handler(
    State(state): State<DaemonState>,
    Query(query): Query<SessionSearchQuery>,
) -> Json<serde_json::Value> {
    let Some(storage) = &state.storage else {
        return Json(serde_json::json!([]));
    };
    let storage2 = storage.clone();
    let q = query.q.trim().to_string();
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let hits = tokio::task::spawn_blocking(move || {
        let store = storage2.blocking_lock();
        store.search_sessions(&q, limit)
    })
    .await;
    let hits = match hits {
        Ok(Ok(hits)) => hits,
        Ok(Err(error)) => {
            tracing::warn!(%error, "session transcript search failed");
            Vec::new()
        }
        Err(error) => {
            tracing::warn!(%error, "session transcript search worker failed");
            Vec::new()
        }
    };
    let rows: Vec<serde_json::Value> = hits
        .iter()
        .map(|m| {
            serde_json::json!({
                "sessionId": m.session_id,
                "title": m.title,
                "snippet": m.snippet,
            })
        })
        .collect();
    Json(serde_json::json!(rows))
}
/// Delete a session tree: interrupt every running descendant, drop the tree
/// from the live map, and purge all of its persisted rows.
pub(crate) async fn delete_session_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let live_parent_links: Vec<(String, Option<String>)> =
        lock_recover(&state.sessions, "sessions")
            .values()
            .map(|session| (session.id.clone(), session.parent_session_id.clone()))
            .collect();
    let persisted_parent_links: Vec<(String, Option<String>)> =
        if let Some(storage) = &state.storage {
            let storage = storage.clone();
            tokio::task::spawn_blocking(move || {
                storage
                    .blocking_lock()
                    .list_sessions()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|row| (row.id, row.parent_session_id))
                    .collect()
            })
            .await
            .unwrap_or_default()
        } else {
            Vec::new()
        };

    let mut deleted_ids = HashSet::from([id.clone()]);
    loop {
        let before = deleted_ids.len();
        for (session_id, parent_id) in live_parent_links.iter().chain(&persisted_parent_links) {
            if parent_id
                .as_ref()
                .is_some_and(|parent| deleted_ids.contains(parent))
            {
                deleted_ids.insert(session_id.clone());
            }
        }
        if deleted_ids.len() == before {
            break;
        }
    }

    // Mark the complete tree before touching storage. Persisters hold this same
    // lock through writes, so deletion is ordered after any already-started write
    // and before every later event from an interrupted engine.
    lock_recover(&state.deleted_session_ids, "deleted_session_ids")
        .extend(deleted_ids.iter().cloned());

    let removed: Vec<Arc<LiveSession>> = {
        let mut sessions = lock_recover(&state.sessions, "sessions");
        deleted_ids
            .iter()
            .filter_map(|session_id| sessions.remove(session_id))
            .collect()
    };
    let had_live = !removed.is_empty();
    for live in &removed {
        if let Some(tx) = &live.interrupt_tx {
            let _ = tx.send(true);
        }
    }

    // CR-17: release the per-session compaction coordinator.
    //
    // `compaction_coordinator` holds a process-wide `session_id -> Arc<Compactor>`
    // registry so a compactor's breaker and prefire state survive across attempts
    // within one session. Nothing else frees those entries: until this call
    // existed, `retire_session` had no caller outside the engine crate's own
    // tests, so every session that was ever compacted left an `Arc<Compactor>`
    // resident for the life of the daemon -- including sessions the user had
    // explicitly deleted. Deleting a session is the one moment its coordinator is
    // provably finished with.
    //
    // Three session-keyed registries back compaction -- the coordinator, the
    // CR-18 envelope meter, and the CR-18 pending-retry ledger -- so this goes
    // through `retire_session_compaction_state`, the single retirement point
    // that clears all three. Clearing only the coordinator would still leak the
    // other two.
    //
    // Retire the whole tree, not just the root: children are compacted
    // independently and each holds its own slots.
    for session_id in &deleted_ids {
        rightkit_agent_engine_core::retire_session_compaction_state(session_id);
    }
    let mut deleted_persisted = false;
    if let Some(storage) = &state.storage {
        let storage2 = storage.clone();
        let root_id = id.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            let mut store = storage2.blocking_lock();
            store.delete_session_tree(&root_id)
        })
        .await;
        match outcome {
            Ok(Ok(result)) => {
                deleted_persisted = result.deleted_sessions > 0;
            }
            Ok(Err(err)) => {
                tracing::error!(session_id = %id, error = %err, "delete_session_tree failed");
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
            Err(join_err) => {
                tracing::error!(session_id = %id, error = %join_err, "delete_session_tree join failed");
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
        }
    }
    if !deleted_persisted && !had_live {
        return Err(StatusCode::NOT_FOUND);
    }
    // Lane-C: the session tree is committed-deleted, so tombstone each
    // session's absorbed transcript stream in Membrane. Local exporter
    // cursors already died inside `delete_session_tree`; the forget is
    // ordered strictly after the durable delete so no later export can
    // resurrect the stream. Failures are logged, not folded back into the
    // already-successful deletion.
    if let Some(exporter) = state.transcript_exporter.as_ref() {
        crate::native_transcript_exporter::spawn_session_forget(
            exporter.clone(),
            deleted_ids.iter().cloned().collect(),
        );
    }
    Ok(Json(serde_json::json!({
        "deleted": true,
        "id": id,
        "deletedSessionIds": deleted_ids,
    })))
}
pub(crate) async fn create_session_handler(
    State(state): State<DaemonState>,
    Json(body): Json<CreateSession>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let id = body.id.unwrap_or_else(|| format!("s-{}", uuid_v7()));
    if state.get_session(&id).is_some() {
        return Err(StatusCode::CONFLICT);
    }
    let workspace_config = rightkit_app_settings::agent_config::ConfigLoader::new()
        .load_files_for_workspace(FsPath::new(body.workspace_root.trim()));
    let requested_model = body.model.clone();
    let inference_selection = body.inference_selection.clone();
    if requested_model.is_some() && inference_selection.is_some() {
        return Err(StatusCode::BAD_REQUEST);
    }
    if inference_selection.is_some() && body.provider_id.is_some() {
        // A qualified selection carries its own connection authority; a
        // legacy provider pin cannot be committed to that route.
        return Err(StatusCode::BAD_REQUEST);
    }
    if let Some(selection) = &inference_selection {
        selection.validate().map_err(|_| StatusCode::BAD_REQUEST)?;
    }
    let resolved_inference_route = match inference_selection.as_ref() {
        Some(selection) => Some(
            crate::inference_runtime::ensure_inference_selection_qualified(&state, selection)
                .await
                .map_err(|_| StatusCode::BAD_REQUEST)?,
        ),
        None => None,
    };
    let inference_fingerprint = resolved_inference_route
        .as_ref()
        .map(|route| route.fingerprint.clone());
    let model = requested_model.or_else(|| {
        let model = workspace_config.model.default_model.trim();
        (!model.is_empty()).then(|| model.to_string())
    });
    let fallback_models = workspace_config.model.fallback_models.clone();
    let requested_toolset = effective_session_toolset(body.safe_mode, body.toolset.as_deref())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string);
    let toolset_profile = if body.safe_mode {
        Some(resolve_toolset_profile("read-only").map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?)
    } else {
        match requested_toolset.as_deref() {
            Some(name) => Some(
                resolve_toolset_profile_with_config(name, &workspace_config)
                    .map_err(|_| StatusCode::BAD_REQUEST)?,
            ),
            None => None,
        }
    };
    let execution_backend = {
        let explicit_execution_backend = body
            .execution_backend
            .as_deref()
            .map(str::trim)
            .is_some_and(|value| !value.is_empty());
        let explicit_execution_workspace = body
            .execution_workspace_id
            .as_deref()
            .map(str::trim)
            .is_some_and(|value| !value.is_empty());
        if explicit_execution_backend || explicit_execution_workspace {
            require_license_feature(&state, "execution_backends")?;
        }
        let registry = lock_recover(&state.execution_backends, "execution_backends");
        resolve_execution_backend_for_workspace(
            FsPath::new(body.workspace_root.trim()),
            &registry,
            body.execution_backend.as_deref(),
        )
        .map_err(|_| StatusCode::BAD_REQUEST)?
    };
    let execution_workspace_id = resolve_execution_workspace_id_for_session(
        FsPath::new(body.workspace_root.trim()),
        body.execution_workspace_id.as_deref(),
        execution_backend.as_deref(),
    )
    .map_err(|_| StatusCode::BAD_REQUEST)?;
    let execution_workspace_path = resolve_execution_workspace_path_for_session(
        FsPath::new(body.workspace_root.trim()),
        execution_workspace_id.as_deref(),
        execution_backend.as_deref(),
    )
    .map_err(|_| StatusCode::BAD_REQUEST)?;
    let base_allowed_tools = if body.allowed_tools.is_empty() {
        workspace_config.session.allowed_tools.clone()
    } else {
        body.allowed_tools
    };
    let base_disallowed_tools = if body.disallowed_tools.is_empty() {
        workspace_config.session.disallowed_tools.clone()
    } else {
        body.disallowed_tools
    };
    let (allowed_tools, disallowed_tools) = apply_toolset_profile(
        toolset_profile.as_ref(),
        base_allowed_tools,
        base_disallowed_tools,
    );
    let effort = normalize_effort(body.effort.or_else(|| {
        let effort = workspace_config.session.effort.trim();
        (!effort.is_empty()).then(|| effort.to_string())
    }))
    .map_err(|_| StatusCode::BAD_REQUEST)?;
    let thinking_budget_tokens = normalize_thinking_budget(
        body.thinking_budget_tokens
            .or(workspace_config.session.thinking_budget_tokens),
    )
    .map_err(|_| StatusCode::BAD_REQUEST)?;
    let disable_thinking = body
        .disable_thinking
        .unwrap_or_else(|| workspace_config.session.disable_thinking());
    let requested_spec_id = body
        .spec_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if let Some(spec_id) = requested_spec_id.as_deref() {
        let spec = {
            let store = lock_recover(&state.specs, "specs");
            store.spec(spec_id).ok_or(StatusCode::NOT_FOUND)?
        };
        if !same_path(
            body.workspace_root.trim(),
            FsPath::new(&spec.workspace_root),
        ) {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    // Request wins over the workspace config, so an unattended caller can pick a
    // mode without writing to the user's config file.
    let requested_permission_mode = body
        .permission_mode
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let permission_mode = match requested_permission_mode {
        // Strict: a misspelled mode is a 400, not a silent downgrade to `Ask`.
        Some(mode) => Some(
            rightkit_agent_policy::parse_permission_mode_strict(mode)
                .ok_or(StatusCode::BAD_REQUEST)?,
        ),
        None if workspace_config.permissions.mode.trim().is_empty() => None,
        None => Some(rightkit_agent_policy::parse_permission_mode(Some(
            workspace_config.permissions.mode.trim(),
        ))),
    };
    let driver_kind =
        normalize_driver_kind(body.driver_kind).map_err(|_| StatusCode::BAD_REQUEST)?;
    let live = state.create_session_with_options(
        &id,
        body.role.as_deref(),
        &body.workspace_root,
        &body.title,
        body.parent_session_id.as_deref(),
        SessionRuntimeOptions {
            model,
            provider_id: body
                .provider_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            inference_selection,
            inference_fingerprint,
            resolved_inference_route: resolved_inference_route.clone(),
            fallback_models,
            allowed_tools,
            disallowed_tools,
            toolset: toolset_profile.as_ref().map(|profile| profile.id.clone()),
            verifier_model: None,
            verifier_is_independent: None,
            active_skills: body
                .active_skills
                .into_iter()
                .map(|skill| skill.trim().to_string())
                .filter(|skill| !skill.is_empty())
                .collect(),
            execution_backend,
            execution_workspace_id,
            execution_workspace_path,
            execution_network_profile: None,
            driver_kind,
            mcp_channel: normalize_mcp_channel(body.mcp_channel),
            spec_id: requested_spec_id,
            effort,
            thinking_budget_tokens,
            disable_thinking,
            safe_mode: body.safe_mode,
            forked_from_session_id: None,
            permission_mode,
            max_iterations: body.max_iterations,
            budget_ceiling_usd: body.budget_ceiling_usd,
            system_prompt: body.system_prompt,
            append_system_prompt: body.append_system_prompt,
            add_dir: body.add_dir,
            bare: body.bare,
            no_mcp: body.no_mcp,
            mcp_config: body.mcp_config,
            lineage_budget: None,
            fable_mode: body.fable_mode.unwrap_or(false),
            brief_default: body
                .brief_default
                .unwrap_or(workspace_config.prompt.brief_default()),
        },
    );
    if let Some(route) = &resolved_inference_route {
        crate::inference_runtime::bind_inference_route(&state, &live.id, &live.id, None, route)
            .await
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    }
    // FM-177: acknowledged session row persistence — return 503 on
    // durable failure instead of warn!+success. Wait for the row to
    // appear in storage or for persistence_state to surface an error.
    if let Some(storage) = state.storage.clone() {
        let live_persistence = live.persistence_state.clone();
        let id_clone = live.id.clone();
        let _state_clone = state.clone();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut acknowledged = false;
        let mut saw_error: Option<String> = None;
        while std::time::Instant::now() < deadline {
            let snap = live_persistence.snapshot();
            if let Some(err) = snap.last_error.clone() {
                saw_error = Some(err);
                break;
            }
            let exists = {
                let store = storage.clone();
                let id = id_clone.clone();
                tokio::task::spawn_blocking(move || {
                    store
                        .blocking_lock()
                        .get_session(&id)
                        .ok()
                        .flatten()
                        .is_some()
                })
                .await
                .unwrap_or(false)
            };
            if exists {
                acknowledged = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        if let Some(err) = saw_error {
            tracing::error!(session_id = %live.id, error = %err, "session creation persistence failed");
            lock_recover(&state.sessions, "sessions").remove(&live.id);
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        if !acknowledged {
            let exists = {
                let store = storage.clone();
                let id = live.id.clone();
                tokio::task::spawn_blocking(move || {
                    store
                        .blocking_lock()
                        .get_session(&id)
                        .ok()
                        .flatten()
                        .is_some()
                })
                .await
                .unwrap_or(false)
            };
            if !exists {
                tracing::error!(session_id = %live.id, "session row not persisted after timeout");
                lock_recover(&state.sessions, "sessions").remove(&live.id);
                return Err(StatusCode::SERVICE_UNAVAILABLE);
            }
        }
    }
    Ok(Json(session_row(&live)))
}

fn effective_session_toolset(safe_mode: bool, requested: Option<&str>) -> Option<&str> {
    if safe_mode {
        Some("read-only")
    } else {
        requested
    }
}

pub(crate) async fn fork_session_handler(
    State(state): State<DaemonState>,
    Path(parent_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<ForkSessionRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let nonce = super::idempotency::normalize_nonce(body.nonce.as_deref())?;
    let digest = super::idempotency::request_digest(
        "fork_session",
        &serde_json::json!({ "id": body.id, "title": body.title }),
    );
    let storage = state.storage.clone();
    let scope_id = parent_id.clone();
    super::idempotency::run_idempotent(storage, &scope_id, "fork_session", nonce, digest, || {
        fork_session_once(state, parent_id, headers, body)
    })
    .await
}

async fn fork_session_once(
    state: DaemonState,
    parent_id: String,
    headers: HeaderMap,
    body: ForkSessionRequest,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // Fork creates a new durable session tree and is therefore a mutation,
    // not a read-only transcript operation. Require the same trusted-source
    // authority used by delete/message mutation routes before any state is
    // inspected or allocated.
    enforce_trusted_source_authority(&headers)?;
    let new_id = body.id.unwrap_or_else(|| format!("s-{}", uuid_v7()));
    if state.get_session(&new_id).is_some() || persisted_session_exists(&state, &new_id).await {
        return Err(StatusCode::CONFLICT);
    }

    let (role, workspace_root, parent_title, mut options) =
        if let Some(parent) = state.get_session(&parent_id) {
            (
                parent.role.clone(),
                parent.workspace_root.clone(),
                lock_recover(&parent.title, "session.title").clone(),
                SessionRuntimeOptions {
                    model: Some(lock_recover(&parent.model, "session.model").clone()),
                    provider_id: lock_recover(&parent.provider_id, "session.provider_id").clone(),
                    allowed_tools: parent.allowed_tools.clone(),
                    disallowed_tools: parent.disallowed_tools.clone(),
                    toolset: parent.toolset.clone(),
                    active_skills: parent.active_skills.clone(),
                    execution_backend: parent.execution_backend.clone(),
                    execution_workspace_id: parent.execution_workspace_id.clone(),
                    execution_workspace_path: parent.execution_workspace_path.clone(),
                    execution_network_profile: parent.execution_network_profile.clone(),
                    driver_kind: lock_recover(&parent.driver_kind, "session.driver_kind").clone(),
                    mcp_channel: parent.mcp_channel.clone(),
                    spec_id: lock_recover(&parent.spec_id, "session.spec_id").clone(),
                    effort: lock_recover(&parent.effort, "session.effort").clone(),
                    thinking_budget_tokens: parent.thinking_budget_tokens,
                    disable_thinking: parent.disable_thinking,
                    safe_mode: parent.safe_mode,
                    ..SessionRuntimeOptions::default()
                },
            )
        } else {
            let row = persisted_session_row(&state, &parent_id)
                .await
                .ok_or(StatusCode::NOT_FOUND)?;
            (
                row.role.clone(),
                row.workspace_root.clone(),
                row.title.clone(),
                crate::api::session_runtime_options_from_config(&row.config_json),
            )
        };

    let title = body
        .title
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("Fork of {parent_title}"));
    options.forked_from_session_id = Some(parent_id.clone());
    if !options.brief_default {
        options.brief_default = rightkit_app_settings::agent_config::ConfigLoader::new()
            .load_files_for_workspace(FsPath::new(workspace_root.trim()))
            .prompt
            .brief_default();
    }
    let live = state.create_session_with_options(
        &new_id,
        role.as_deref(),
        &workspace_root,
        &title,
        Some(&parent_id),
        options,
    );
    // FM-177: fork row persistence acknowledged — same 503 semantics as create.
    if let Some(storage) = state.storage.clone() {
        let live_persistence = live.persistence_state.clone();
        let id_clone = live.id.clone();
        let _state_clone = state.clone();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut acknowledged = false;
        let mut saw_error: Option<String> = None;
        while std::time::Instant::now() < deadline {
            let snap = live_persistence.snapshot();
            if let Some(err) = snap.last_error.clone() {
                saw_error = Some(err);
                break;
            }
            let exists = {
                let store = storage.clone();
                let id = id_clone.clone();
                tokio::task::spawn_blocking(move || {
                    store
                        .blocking_lock()
                        .get_session(&id)
                        .ok()
                        .flatten()
                        .is_some()
                })
                .await
                .unwrap_or(false)
            };
            if exists {
                acknowledged = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        if let Some(err) = saw_error {
            tracing::error!(session_id = %live.id, error = %err, "fork session persistence failed");
            lock_recover(&state.sessions, "sessions").remove(&live.id);
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        if !acknowledged {
            let exists = {
                let store = storage.clone();
                let id = live.id.clone();
                tokio::task::spawn_blocking(move || {
                    store
                        .blocking_lock()
                        .get_session(&id)
                        .ok()
                        .flatten()
                        .is_some()
                })
                .await
                .unwrap_or(false)
            };
            if !exists {
                tracing::error!(session_id = %live.id, "fork session row not persisted after timeout");
                lock_recover(&state.sessions, "sessions").remove(&live.id);
                return Err(StatusCode::SERVICE_UNAVAILABLE);
            }
        }
    }
    Ok(Json(serde_json::json!({
        "id": live.id,
        "parentSessionId": parent_id,
        "title": title,
        "workspaceRoot": crate::api::api_workspace_root(&workspace_root),
        "session": session_row(&live),
    })))
}

pub(crate) async fn persisted_session_exists(state: &DaemonState, id: &str) -> bool {
    persisted_session_row(state, id).await.is_some()
}

pub(crate) async fn persisted_session_row(
    state: &DaemonState,
    id: &str,
) -> Option<rightkit_agent_engine_storage::SessionRow> {
    let storage = state.storage.as_ref()?.clone();
    let id = id.to_string();
    tokio::task::spawn_blocking(move || storage.blocking_lock().get_session(&id).ok().flatten())
        .await
        .ok()
        .flatten()
}
pub(crate) async fn get_transcript(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<serde_json::Value>>, StatusCode> {
    // Live session: return the in-memory transcript.
    if let Some(live) = state.get_session(&id) {
        let messages = lock_recover(&live.transcript, "session.transcript");
        let rows: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| serde_json::to_value(m).unwrap_or(serde_json::Value::Null))
            .collect();
        return Ok(Json(rows));
    }
    // Dormant session: rehydrate the transcript from the persisted event log.
    let storage = state.storage.as_ref().ok_or(StatusCode::NOT_FOUND)?;
    let storage2 = storage.clone();
    let id2 = id.clone();
    let events = tokio::task::spawn_blocking(move || {
        let store = storage2.blocking_lock();
        store.all_events(&id2)
    })
    .await
    .map_err(|_| StatusCode::NOT_FOUND)?
    .map_err(|_| StatusCode::NOT_FOUND)?;
    if events.is_empty() {
        return Err(StatusCode::NOT_FOUND);
    }
    let messages = crate::rehydrate::rehydrate_transcript(&events);
    let rows: Vec<serde_json::Value> = messages
        .iter()
        .map(|m| serde_json::to_value(m).unwrap_or(serde_json::Value::Null))
        .collect();
    Ok(Json(rows))
}
pub(crate) async fn get_away_summary(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    Query(query): Query<AwaySummaryQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let transcript = transcript_for_session(&state, &id).await?;
    let since_index = query.since_index.unwrap_or(0).min(transcript.len());
    let limit = query.limit.unwrap_or(12).clamp(1, 25);
    let visible: Vec<&TranscriptMessage> = transcript
        .iter()
        .skip(since_index)
        .filter(|message| away_summary_line(message).is_some())
        .collect();
    let message_count = visible.len();
    let user_messages = visible
        .iter()
        .filter(|message| matches!(message, TranscriptMessage::User { .. }))
        .count();
    let assistant_messages = visible
        .iter()
        .filter(|message| matches!(message, TranscriptMessage::Assistant { .. }))
        .count();
    let tool_results = visible
        .iter()
        .filter(|message| matches!(message, TranscriptMessage::Tool { .. }))
        .count();
    let error_count = visible
        .iter()
        .filter(|message| matches!(message, TranscriptMessage::Tool { is_error: true, .. }))
        .count();
    let latest_at = visible
        .iter()
        .filter_map(|message| transcript_created_at(message))
        .next_back()
        .unwrap_or_default();
    let bullets: Vec<String> = visible
        .iter()
        .rev()
        .filter_map(|message| away_summary_line(message))
        .take(limit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    Ok(Json(serde_json::json!({
        "sessionId": id,
        "sinceIndex": since_index,
        "nextIndex": transcript.len(),
        "messageCount": message_count,
        "userMessages": user_messages,
        "assistantMessages": assistant_messages,
        "toolResults": tool_results,
        "errorCount": error_count,
        "latestAt": latest_at,
        "hasActivity": message_count > 0,
        "bullets": bullets,
    })))
}
pub(crate) async fn get_untrusted_content(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let transcript = transcript_for_session(&state, &id).await?;
    let items = transcript
        .iter()
        .enumerate()
        .filter_map(untrusted_content_item)
        .collect::<Vec<_>>();
    Ok(Json(serde_json::json!({
        "sessionId": id,
        "untrustedCount": items.len(),
        "items": items,
    })))
}
/// Bring a dormant session back to life (rehydrate + re-spawn the engine with
/// its history seeded) without sending a message. Idempotent for live sessions.
pub(crate) async fn resume_session_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    match state.resume_session(&id).await {
        Some(_) => Ok(Json(serde_json::json!({ "resumed": true, "id": id }))),
        None => Err(StatusCode::NOT_FOUND),
    }
}
pub(crate) async fn interrupt_session(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;

    // A session the daemon no longer holds live (restart, or a row that exists
    // only in storage) may still have RUNNING children — exactly the
    // runaway-conductor case, where Stop returning 404 left force-quitting the
    // app as the only recourse. Cancel whatever children are live and report
    // what was reached, rather than refusing outright.
    let Some(live) = state.get_session(&id) else {
        let cancelled = state.interrupt_live_children_of(&id);
        return if cancelled == 0 {
            Err(StatusCode::NOT_FOUND)
        } else {
            Ok(Json(serde_json::json!({
                "ok": true,
                "parentLive": false,
                "cancelledChildren": cancelled,
            })))
        };
    };
    if let Some(tx) = &live.interrupt_tx {
        let _ = tx.send(true);
    }
    // Cancel every live descendant too: a parent's own interrupt does not
    // reach its children's engines.
    let cancelled_descendants = state.interrupt_live_children_of(&id);
    // Operator prompts and plan gates wait on oneshots outside the provider
    // request. Dropping their senders is required to unwind the tool future;
    // the interrupt watch alone cannot wake those waits.
    //
    // Tool approvals wait the same way and were missed here, so a run parked on
    // an approval ignored Stop entirely — the one state where a user most wants
    // it to work. Dropping the sender resolves the receiver to Err, which the
    // dispatcher reads as "denied", so the tool unwinds instead of hanging.
    lock_recover(&live.pending_questions, "session.pending_questions").clear();
    lock_recover(&live.pending_elicitations, "session.pending_elicitations").clear();
    lock_recover(&live.pending_plans, "session.pending_plans").clear();
    lock_recover(&live.pending_approvals, "session.pending_approvals").clear();

    // If this is a sub-agent session, notify the parent so the orchestrator can react.
    if let Some(parent_id) = &live.parent_session_id {
        if let Some(parent) = state.get_session(parent_id) {
            let seq = parent.conduit.next_seq();
            parent.conduit.publish(ProtocolEvent::SubAgentCancelled {
                session_id: parent_id.clone(),
                seq,
                created_at: now_iso(),
                sub_session_id: id.clone(),
                role: live.role.clone().unwrap_or_default(),
                cancelled_by: "user".into(),
            });
        }
    }

    Ok(Json(serde_json::json!({
        "interrupted": true,
        "cancelledChildren": cancelled_descendants,
    })))
}
pub(crate) async fn kill_session(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let live = state.get_session(&id).ok_or(StatusCode::NOT_FOUND)?;
    if let Some(tx) = &live.interrupt_tx {
        let _ = tx.send(true);
    }
    if let Some(store) = state.storage.clone() {
        let now = now_iso();
        let id_clone = id.clone();
        tokio::spawn(async move {
            let s = store.lock().await;
            if let Err(e) = s.update_session_status(&id_clone, "dormant", &now) {
                tracing::warn!(session_id = %id_clone, error = %e, "kill_session status update failed");
            }
        });
    }
    lock_recover(&state.sessions, "sessions").remove(&id);
    Ok(Json(serde_json::json!({ "killed": true })))
}
pub(crate) async fn patch_session(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<PatchSession>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    if body.inference_selection.is_some()
        && (body.model.is_some() || body.provider_id.is_some() || body.effort.is_some())
    {
        // A qualified selection carries its own connection/route authority;
        // the legacy model/provider/effort patch cannot be committed to it.
        return Err(StatusCode::BAD_REQUEST);
    }
    let live_session = state.get_session(&id);
    let persisted_row = if live_session.is_none() {
        if let Some(store) = &state.storage {
            let s = store.lock().await;
            s.get_session(&id).ok().flatten()
        } else {
            None
        }
    } else {
        None
    };
    if live_session.is_none() && persisted_row.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    let resolved_inference_route = match body.inference_selection.as_ref() {
        Some(selection) => {
            selection.validate().map_err(|_| StatusCode::BAD_REQUEST)?;
            if live_session
                .as_ref()
                .is_some_and(|live| live.parent_session_id.is_some())
                || persisted_row
                    .as_ref()
                    .is_some_and(|row| row.parent_session_id.is_some())
            {
                return Err(StatusCode::CONFLICT);
            }
            if let Some(live) = &live_session {
                if live
                    .inference_active_requests
                    .load(std::sync::atomic::Ordering::Acquire)
                    > 0
                    || live.run_budget.lock().await.concurrent_in_flight() > 0
                    || lock_recover(&state.sessions, "sessions")
                        .values()
                        .any(|candidate| {
                            candidate.parent_session_id.as_deref() == Some(id.as_str())
                        })
                {
                    return Err(StatusCode::CONFLICT);
                }
            }
            Some(
                crate::inference_runtime::ensure_inference_selection_qualified(&state, selection)
                    .await
                    .map_err(|_| StatusCode::BAD_REQUEST)?,
            )
        }
        None => None,
    };
    if let Some(live) = &live_session {
        touch_session(live);
    }
    if let Some(title) = body.title {
        if let Some(store) = state.storage.clone() {
            let now = now_iso();
            let id_clone = id.clone();
            let title_for_storage = title.clone();
            tokio::task::spawn_blocking(move || {
                let s = store.blocking_lock();
                s.rename_session(&id_clone, &title_for_storage, &now)
            })
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .map_err(|e| {
                tracing::warn!(session_id = %id, error = %e, "patch_session rename failed");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        }
        if let Some(live) = &live_session {
            *lock_recover(&live.title, "session.title") = title;
        }
    }
    if let Some(tags) = body.tags {
        let tags_json = serde_json::to_string(&tags).unwrap_or_default();
        let now = now_iso();
        let id2 = id.clone();
        if let Some(store) = state.storage.clone() {
            tokio::task::spawn_blocking(move || {
                let s = store.blocking_lock();
                s.update_session_tags(&id2, &tags_json, &now)
            })
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .map_err(|e| {
                tracing::warn!(session_id = %id, error = %e, "patch_session tags update failed");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        }
    }
    if let Some(status) = body.status {
        update_session_lifecycle_status(&state, &id, &status).await?;
    }
    let model_updated = body.model.is_some();
    let mut model_applies_to = serde_json::Value::Null;
    if body.model.is_some() || body.provider_id.is_some() || body.effort.is_some() {
        // model/providerId/effort move as one route patch. An omitted model
        // keeps the session's current model so provider- or effort-only
        // switches work; an omitted provider re-derives the resolved route.
        let model = match body.model {
            Some(model) => model,
            None => live_session
                .as_ref()
                .map(|live| lock_recover(&live.model, "session.model").clone())
                .or_else(|| {
                    persisted_row.as_ref().and_then(|row| {
                        serde_json::from_str::<serde_json::Value>(&row.config_json)
                            .ok()
                            .and_then(|cfg| {
                                cfg.get("model")
                                    .and_then(|value| value.as_str())
                                    .map(str::to_string)
                            })
                    })
                })
                .ok_or(StatusCode::BAD_REQUEST)?,
        };
        let applied_live = update_session_route(
            &state,
            &id,
            &model,
            body.provider_id.as_deref(),
            body.effort.as_deref(),
        )
        .await?;
        model_applies_to = serde_json::Value::String(if applied_live {
            "live".to_string()
        } else {
            "resume".to_string()
        });
    }
    let driver_kind_updated = body.driver_kind.is_some();
    let mut driver_kind_value = serde_json::Value::Null;
    if let Some(driver_kind) = body.driver_kind {
        let normalized =
            normalize_driver_kind(Some(driver_kind)).map_err(|_| StatusCode::BAD_REQUEST)?;
        update_session_driver_kind(&state, &id, &normalized).await?;
        driver_kind_value = serde_json::Value::String(normalized);
    }
    let inference_selection_updated = resolved_inference_route.is_some();
    let mut inference_selection_applies_to = serde_json::Value::Null;
    if let (Some(selection), Some(route)) = (body.inference_selection, resolved_inference_route) {
        crate::inference_runtime::bind_inference_route(&state, &id, &id, None, &route)
            .await
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let store = state
            .storage
            .clone()
            .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
        let selection_json =
            serde_json::to_value(&selection).map_err(|_| StatusCode::BAD_REQUEST)?;
        let fingerprint = route.fingerprint.clone();
        let id_for_storage = id.clone();
        let updated_at = now_iso();
        let persisted = tokio::task::spawn_blocking(move || {
            store.blocking_lock().update_session_inference_selection(
                &id_for_storage,
                &selection_json,
                &fingerprint,
                &updated_at,
            )
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(|error| {
            tracing::warn!(session_id = %id, error = %error, "patch_session inference selection persist failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        if !persisted {
            return Err(StatusCode::NOT_FOUND);
        }
        if let Some(live) = &live_session {
            let previous_authority =
                lock_recover(&live.inference_selection, "session.inference_selection")
                    .as_ref()
                    .map(|current| current.authority);
            let can_update_live = previous_authority == Some(selection.authority)
                && live.inference_route_tx.is_some();
            *lock_recover(&live.inference_selection, "session.inference_selection") =
                Some(selection.clone());
            *lock_recover(&live.inference_fingerprint, "session.inference_fingerprint") =
                Some(route.fingerprint.clone());
            *lock_recover(&live.model, "session.model") = selection.model_id.clone();
            if can_update_live {
                live.inference_route_tx
                    .as_ref()
                    .expect("checked inference route sender")
                    .send(route)
                    .await
                    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
                inference_selection_applies_to = serde_json::Value::String("live".into());
            } else {
                if let Some(interrupt) = &live.interrupt_tx {
                    let _ = interrupt.send(true);
                }
                lock_recover(&state.sessions, "sessions").remove(&id);
                state
                    .resume_session(&id)
                    .await
                    .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
                inference_selection_applies_to = serde_json::Value::String("restarted".into());
            }
        } else {
            inference_selection_applies_to = serde_json::Value::String("resume".into());
        }
    }
    Ok(Json(serde_json::json!({
        "updated": true,
        "id": id,
        "modelUpdated": model_updated,
        "modelAppliesTo": model_applies_to,
        "driverKindUpdated": driver_kind_updated,
        "driverKind": driver_kind_value,
        "inferenceSelectionUpdated": inference_selection_updated,
        "inferenceSelectionAppliesTo": inference_selection_applies_to,
    })))
}

/// Switch a session's root conductor, in durable config AND in the live
/// session.
///
/// Both halves are required. `issue_capability_lease` compares the requested
/// kind against the LIVE value, so persisting alone would make the very next
/// send 409 against the conductor the user just picked — the switch would look
/// accepted and then refuse to work until a restart.
pub(crate) async fn update_session_driver_kind(
    state: &DaemonState,
    id: &str,
    new_kind: &str,
) -> Result<(), StatusCode> {
    let apply_live = |state: &DaemonState| {
        if let Some(live) = state.get_session(id) {
            *lock_recover(&live.driver_kind, "session.driver_kind") = new_kind.to_string();
        }
    };
    let Some(store) = state.storage.clone() else {
        apply_live(state);
        return Ok(());
    };
    let id_owned = id.to_string();
    let new_kind_owned = new_kind.to_string();
    let now = now_iso();
    match tokio::task::spawn_blocking(move || -> Result<(), StatusCode> {
        let s = store.blocking_lock();
        let bindings = s.list_driver_bindings_for_session(&id_owned).map_err(|e| {
            tracing::warn!(session_id = %id_owned, error = %e, "patch_session driver binding lookup failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        // A bound vendor process (an existing non-native binding for a
        // different kind) owns a live external session/thread on the other
        // side of that binding; repointing driverKind here would silently
        // orphan that process instead of retargeting it, so refuse rather
        // than let the two desync.
        if bindings
            .iter()
            .any(|binding| binding.driver_kind != "native" && binding.driver_kind != new_kind_owned)
        {
            return Err(StatusCode::CONFLICT);
        }
        s.update_session_config_string(&id_owned, "driverKind", Some(&new_kind_owned), &now)
            .map_err(|e| {
                tracing::warn!(session_id = %id_owned, error = %e, "patch_session driverKind update failed");
                StatusCode::INTERNAL_SERVER_ERROR
            })
            .map(|_| ())
    })
    .await
    {
        // Only after the durable write succeeds — a live value the database
        // does not agree with would survive until restart and then silently
        // revert.
        Ok(Ok(())) => {
            apply_live(state);
            Ok(())
        }
        Ok(Err(status)) => Err(status),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}
pub(crate) async fn update_session_lifecycle_status(
    state: &DaemonState,
    id: &str,
    status: &str,
) -> Result<(), StatusCode> {
    let status = normalize_session_status(status).ok_or(StatusCode::BAD_REQUEST)?;
    if status == "done" {
        ensure_bound_spec_can_complete(state, id).await?;
    }
    let now = now_iso();
    let Some(store) = state.storage.clone() else {
        if state.get_session(id).is_none() {
            return Err(StatusCode::NOT_FOUND);
        }
        apply_live_session_status(&state, id, &status).await?;
        return Ok(());
    };
    let id_owned = id.to_string();
    let status_owned = status.clone();
    let updated_at = now.clone();
    let updated = tokio::task::spawn_blocking(move || {
        let store = store.blocking_lock();
        if store.get_session(&id_owned)?.is_none() {
            return Ok(false);
        }
        store.update_session_status(&id_owned, &status_owned, &updated_at)?;
        Ok::<bool, rightkit_agent_engine_storage::StorageError>(true)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .map_err(|error| {
        tracing::warn!(session_id = %id, %error, "session lifecycle status persistence failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    if !updated {
        return Err(StatusCode::NOT_FOUND);
    }
    apply_live_session_status(&state, id, &status).await
}

/// Clone a conversation into an independent dormant session. Unlike `fork`,
/// clone keeps a top-level session and copies only canonical replay facts;
/// no prior tool action is dispatched again.
pub(crate) async fn clone_session_handler(
    State(state): State<DaemonState>,
    Path(source_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<ForkSessionRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let nonce = super::idempotency::normalize_nonce(body.nonce.as_deref())?;
    let digest = super::idempotency::request_digest(
        "clone_session",
        &serde_json::json!({ "id": body.id, "title": body.title }),
    );
    let storage = state.storage.clone();
    let scope_id = source_id.clone();
    super::idempotency::run_idempotent(storage, &scope_id, "clone_session", nonce, digest, || {
        clone_session_once(state, source_id, headers, body)
    })
    .await
}

async fn clone_session_once(
    state: DaemonState,
    source_id: String,
    headers: HeaderMap,
    body: ForkSessionRequest,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let source_live = state.get_session(&source_id);
    if source_live
        .as_ref()
        .is_some_and(|live| live.live_turn_handle.is_active())
    {
        return Err(StatusCode::CONFLICT);
    }
    if let Some(live) = &source_live {
        let target = live.persistence_state.snapshot().last_published_seq;
        if target > 0 {
            crate::api::await_event_persistence(live.persistence_state.clone(), target)
                .await
                .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        }
    }
    let storage = state
        .storage
        .clone()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let id = body.id.unwrap_or_else(|| format!("s-{}", uuid_v7()));
    if state.get_session(&id).is_some() {
        return Err(StatusCode::CONFLICT);
    }
    let id_for_storage = id.clone();
    let source_id_for_storage = source_id.clone();
    let title_override = body.title;
    let result = tokio::task::spawn_blocking(move || {
        let mut store = storage.blocking_lock();
        if store
            .get_session(&id_for_storage)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .is_some()
        {
            return Err(StatusCode::CONFLICT);
        }
        let source = store
            .get_session(&source_id_for_storage)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .ok_or(StatusCode::NOT_FOUND)?;
        let title = title_override
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("Clone of {}", source.title));
        let now = now_iso();
        let mut config: serde_json::Value =
            serde_json::from_str(&source.config_json).unwrap_or_else(|_| serde_json::json!({}));
        if let Some(object) = config.as_object_mut() {
            object.insert(
                "forkedFromSessionId".to_string(),
                serde_json::Value::String(source_id_for_storage.clone()),
            );
        }
        let row = rightkit_agent_engine_storage::SessionRow {
            id: id_for_storage.clone(),
            title: title.clone(),
            workspace_root: source.workspace_root.clone(),
            created_at: now.clone(),
            updated_at: now,
            parent_session_id: None,
            role: source.role.clone(),
            status: "dormant".to_string(),
            config_json: serde_json::to_string(&config).unwrap_or_else(|_| "{}".to_string()),
            tags: source.tags.clone(),
            recovery_marker: None,
        };
        store
            .create_session(&row)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let source_events = store
            .all_events(&source_id_for_storage)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let mut next_seq = store
            .latest_seq(&id_for_storage)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            + 1;
        let mut copied = 0usize;
        for source_event in source_events {
            let Ok(mut payload) =
                serde_json::from_str::<serde_json::Value>(&source_event.payload_json)
            else {
                continue;
            };
            let is_replay_fact = matches!(
                source_event.kind.as_str(),
                "userMessageSubmitted"
                    | "assistantMessageCompleted"
                    | "toolCallStarted"
                    | "toolApprovalRequested"
                    | "toolResultAdded"
                    | "sessionCompacted"
                    | "sessionContextFolded"
            );
            if !is_replay_fact {
                continue;
            }
            if payload["sessionId"].as_str() != Some(source_id_for_storage.as_str()) {
                continue;
            }
            let Some(object) = payload.as_object_mut() else {
                continue;
            };
            object.insert(
                "sessionId".to_string(),
                serde_json::Value::String(id_for_storage.clone()),
            );
            object.insert("seq".to_string(), serde_json::json!(next_seq));
            if source_event.kind == "userMessageSubmitted" {
                object.insert("nonce".to_string(), serde_json::Value::Null);
            }
            let Ok(mut event) = serde_json::from_value::<ProtocolEvent>(payload) else {
                continue;
            };
            event.canonicalize_identity(next_seq);
            let payload =
                serde_json::to_string(&event).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            store
                .append_event(&id_for_storage, &source_event.kind, &payload)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            next_seq += 1;
            copied += 1;
        }
        Ok::<_, StatusCode>((row, copied))
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let (row, copied) = match result {
        Ok(result) => result,
        Err(status) => {
            if status != StatusCode::CONFLICT {
                if let Some(storage) = state.storage.clone() {
                    let id = id.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        storage.blocking_lock().delete_session_tree(&id)
                    })
                    .await;
                }
            }
            return Err(status);
        }
    };
    Ok(Json(serde_json::json!({
        "id": row.id,
        "sourceSessionId": source_id,
        "title": row.title,
        "workspaceRoot": crate::api::api_workspace_root(&row.workspace_root),
        "status": row.status,
        "copiedEvents": copied,
        "session": {
            "id": row.id,
            "title": row.title,
            "workspaceRoot": crate::api::api_workspace_root(&row.workspace_root),
            "status": row.status,
            "parentSessionId": serde_json::Value::Null,
            "forkedFromSessionId": source_id,
        }
    })))
}

async fn apply_live_session_status(
    state: &DaemonState,
    id: &str,
    status: &str,
) -> Result<(), StatusCode> {
    if status == "archived" {
        if let Some(live) = lock_recover(&state.sessions, "sessions").remove(id) {
            if let Some(tx) = &live.interrupt_tx {
                let _ = tx.send(true);
            }
        }
    } else if status == "running" {
        state
            .resume_session(id)
            .await
            .ok_or(StatusCode::NOT_FOUND)?;
    }
    Ok(())
}
