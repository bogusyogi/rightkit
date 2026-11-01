use super::*;

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ListSessionsQuery {
    /// Filter by tag: only return sessions that include this tag.
    #[serde(default)]
    pub tag: Option<String>,
    /// Filter by status: "running", "dormant", "done", "interrupted".
    #[serde(default)]
    pub status: Option<String>,
    /// Include child worker sessions. The default session list is the
    /// top-level chat history used by Recents.
    #[serde(default)]
    pub include_subagents: bool,
}

#[derive(Deserialize)]
pub struct SessionSearchQuery {
    pub q: String,
    pub limit: Option<usize>,
}

pub(crate) async fn session_workspace_root(
    state: &DaemonState,
    id: &str,
) -> Result<Option<String>, StatusCode> {
    if let Some(live) = state.get_session(id) {
        return Ok(Some(live.workspace_root.clone()));
    }
    let Some(storage) = &state.storage else {
        return Ok(None);
    };
    let storage2 = storage.clone();
    let id2 = id.to_string();
    tokio::task::spawn_blocking(move || {
        let store = storage2.blocking_lock();
        store
            .get_session(&id2)
            .map(|row| row.map(|row| row.workspace_root))
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub(crate) async fn session_spec_id(
    state: &DaemonState,
    id: &str,
) -> Result<Option<String>, StatusCode> {
    if let Some(live) = state.get_session(id) {
        return Ok(lock_recover(&live.spec_id, "session.spec_id").clone());
    }
    let Some(storage) = &state.storage else {
        return Ok(None);
    };
    let storage2 = storage.clone();
    let id2 = id.to_string();
    tokio::task::spawn_blocking(move || {
        let store = storage2.blocking_lock();
        let row = store.get_session(&id2)?;
        let spec_id = row.and_then(|row| {
            serde_json::from_str::<serde_json::Value>(&row.config_json)
                .ok()
                .and_then(|cfg| {
                    cfg.get("specId")
                        .and_then(|value| value.as_str())
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_string)
                })
        });
        Ok::<Option<String>, rightkit_agent_engine_storage::StorageError>(spec_id)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub(crate) async fn session_spec_completion_summary(
    state: &DaemonState,
    id: &str,
    persist_analysis: bool,
) -> Result<serde_json::Value, StatusCode> {
    let workspace_root = session_workspace_root(state, id)
        .await?
        .ok_or(StatusCode::NOT_FOUND)?;
    let Some(spec_id) = session_spec_id(state, id).await? else {
        return Ok(serde_json::json!({
            "sessionId": id,
            "bound": false,
            "specId": serde_json::Value::Null,
            "status": "pass",
            "canComplete": true,
            "blockerCount": 0,
            "warningCount": 0,
            "blockers": [],
            "warnings": [],
        }));
    };

    let (summary, store_to_persist) = {
        let mut store = lock_recover(&state.specs, "specs");
        let Some(spec) = store.spec(&spec_id) else {
            return Ok(serde_json::json!({
                "sessionId": id,
                "bound": true,
                "specId": spec_id,
                "status": "block",
                "canComplete": false,
                "blockerCount": 1,
                "warningCount": 0,
                "blockers": [{
                    "id": "missing-bound-spec",
                    "severity": "critical",
                    "kind": "missing_bound_spec",
                    "message": "The session is bound to a spec id that no longer exists.",
                    "sourceKind": "spec",
                    "sourceId": spec_id,
                }],
                "warnings": [],
            }));
        };
        if !same_path(&workspace_root, FsPath::new(&spec.workspace_root)) {
            return Ok(serde_json::json!({
                "sessionId": id,
                "bound": true,
                "specId": spec_id,
                "status": "block",
                "canComplete": false,
                "blockerCount": 1,
                "warningCount": 0,
                "blockers": [{
                    "id": "workspace-mismatch",
                    "severity": "critical",
                    "kind": "bound_spec_workspace_mismatch",
                    "message": "The bound spec belongs to a different workspace than this session.",
                    "sourceKind": "spec",
                    "sourceId": spec.id,
                }],
                "warnings": [],
            }));
        }
        let tasks = store.tasks(&spec_id);
        let decisions = store.constitution_decisions(&spec_id);
        let run = rightkit_evidence::specs::analyze_spec_with_constitution_decisions(
            &spec,
            &tasks,
            &decisions,
            format!("sar-{}", now_millis()),
            now_iso(),
        );
        let blockers = run
            .findings
            .iter()
            .filter(|finding| {
                matches!(
                    finding.severity,
                    rightkit_evidence::specs::FindingSeverity::Critical
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        let warnings = run
            .findings
            .iter()
            .filter(|finding| {
                matches!(
                    finding.severity,
                    rightkit_evidence::specs::FindingSeverity::Warning
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        let status = run.status.clone();
        let can_complete = status != rightkit_evidence::specs::AnalysisStatus::Block;
        let summary = serde_json::json!({
            "sessionId": id,
            "bound": true,
            "specId": spec_id,
            "specTitle": spec.title,
            "status": status,
            "canComplete": can_complete,
            "analysisId": run.id,
            "coverage": run.coverage,
            "blockerCount": blockers.len(),
            "warningCount": warnings.len(),
            "blockers": blockers,
            "warnings": warnings,
        });
        if persist_analysis {
            store.record_analysis(run);
            (summary, Some(store.clone()))
        } else {
            (summary, None)
        }
    };

    if let Some(store) = store_to_persist {
        if let Some(path) = state.spec_store_path.as_deref() {
            crate::api_specs::write_spec_store(path, &store)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
    }

    Ok(summary)
}

pub(crate) async fn ensure_bound_spec_can_complete(
    state: &DaemonState,
    id: &str,
) -> Result<(), StatusCode> {
    let summary = session_spec_completion_summary(state, id, true).await?;
    if summary["bound"].as_bool().unwrap_or(false)
        && !summary["canComplete"].as_bool().unwrap_or(false)
    {
        return Err(StatusCode::CONFLICT);
    }
    Ok(())
}

pub(crate) async fn update_session_spec_binding(
    state: &DaemonState,
    id: &str,
    spec_id: Option<String>,
) -> Result<bool, StatusCode> {
    let updated_at = now_iso();
    let mut found = false;
    if let Some(live) = state.get_session(id) {
        *lock_recover(&live.spec_id, "session.spec_id") = spec_id.clone();
        *lock_recover(&live.updated_at, "session.updated_at") = updated_at.clone();
        found = true;
    }
    if let Some(storage) = &state.storage {
        let storage2 = storage.clone();
        let id2 = id.to_string();
        let updated_at2 = updated_at.clone();
        let spec_id2 = spec_id.clone();
        let stored = tokio::task::spawn_blocking(move || {
            let store = storage2.blocking_lock();
            store.update_session_config_string(&id2, "specId", spec_id2.as_deref(), &updated_at2)
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        found = found || stored;
    }
    Ok(found)
}

pub(crate) async fn session_binding_row(
    state: &DaemonState,
    id: &str,
) -> Result<serde_json::Value, StatusCode> {
    if let Some(live) = state.get_session(id) {
        let mut row = session_row(&live);
        row["status"] = serde_json::json!("running");
        return Ok(row);
    }
    let Some(storage) = &state.storage else {
        return Err(StatusCode::NOT_FOUND);
    };
    let storage2 = storage.clone();
    let id2 = id.to_string();
    let stored = tokio::task::spawn_blocking(move || {
        let store = storage2.blocking_lock();
        store.get_session(&id2)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    let updated = if stored.updated_at.is_empty() {
        stored.created_at.clone()
    } else {
        stored.updated_at.clone()
    };
    let cfg: serde_json::Value =
        serde_json::from_str(&stored.config_json).unwrap_or(serde_json::Value::Null);
    Ok(serde_json::json!({
        "id": stored.id,
        "title": stored.title,
        "model": cfg.get("model").and_then(|v| v.as_str()).unwrap_or(""),
        "workspaceRoot": crate::api::api_workspace_root(&stored.workspace_root),
        "createdAt": stored.created_at,
        "updatedAt": updated,
        "role": stored.role,
        "parentSessionId": stored.parent_session_id,
        "forkedFromSessionId": cfg.get("forkedFromSessionId").cloned().unwrap_or(serde_json::Value::Null),
        "status": stored.status,
        "specId": cfg.get("specId").cloned().unwrap_or(serde_json::Value::Null),
        "safeMode": cfg.get("safeMode").cloned().unwrap_or(serde_json::Value::Bool(false)),
        "driverKind": cfg.get("driverKind").cloned().unwrap_or_else(|| serde_json::json!("native")),
        "tags": serde_json::from_str::<Vec<String>>(&stored.tags).unwrap_or_default(),
    }))
}
