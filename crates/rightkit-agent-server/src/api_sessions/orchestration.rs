use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use std::path::{Path as FsPath, PathBuf};

use rightkit_agent_engine_protocol::{ProtocolEvent, TranscriptMessage};

use crate::api::{
    live_shell_statuses_for_workspace, load_stored_artifacts, load_workspace_task_records,
    lock_recover, now_iso, now_millis, persist_artifacts_snapshot, same_path, session_binding_row,
    session_spec_completion_summary, session_workspace_root, task_node_json, task_record_json,
    update_session_spec_binding, workspace_tasks_path, BindSessionSpecRequest, DaemonState,
    OrchestrateBody, SessionQuery,
};
use crate::api_permissions::enforce_trusted_source_authority;
pub(crate) async fn orchestrate_session(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<OrchestrateBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    // CR3 #57: a retried orchestrate must not start a second conductor run.
    let nonce = super::idempotency::normalize_nonce(body.nonce.as_deref())?;
    let digest = super::idempotency::request_digest(
        "orchestrate_session",
        &serde_json::json!({
            "goal": body.goal,
            "dryRun": body.dry_run,
            "requestLabel": body.request_label,
        }),
    );
    let storage = state
        .get_session(&id)
        .and_then(|live| live.storage.clone())
        .or_else(|| state.storage.clone());
    let scope_id = id.clone();
    super::idempotency::run_idempotent(
        storage,
        &scope_id,
        "orchestrate_session",
        nonce,
        digest,
        || orchestrate_session_once(state, id, headers, body),
    )
    .await
}

async fn orchestrate_session_once(
    state: DaemonState,
    id: String,
    headers: HeaderMap,
    body: OrchestrateBody,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    // Orchestration itself is NOT a paid feature: every tier can run the
    // conductor. What Pro buys is multi-model role routing — without the
    // entitlement `run_conductor` collapses every role onto the conductor's own
    // model via `apply_effective_role_model_policy_with_override`, which is the
    // intended free-tier behavior. Gating this endpoint on `brews` made an
    // explicit /audit or /blueprint fail with a bare 402 on unlicensed installs,
    // so the command reported "starting…" and then nothing happened at all.
    let session = state.get_session(&id).ok_or(StatusCode::NOT_FOUND)?;
    let goal = body.goal.clone();
    let dry_run = body.dry_run;

    // Record what was asked for, in the session the user is watching.
    //
    // Slash commands reach the daemon through /commands/execute, so an
    // orchestrated run left NO user message in the transcript: the request was
    // invisible on reload, and the auto-titler — which needs a first
    // user+assistant pair — could never fire, leaving every /audit session
    // named "New session" forever.
    // Prefer what the human typed. `/audit` expands to a ~1kB instruction
    // block; showing that back to them reads like the agent talking to itself.
    let recorded_request = body
        .request_label
        .as_deref()
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| goal.clone());
    if !dry_run && !recorded_request.trim().is_empty() {
        lock_recover(&session.transcript, "session.transcript").push(TranscriptMessage::User {
            id: format!("user-{}", now_millis()),
            created_at: now_iso(),
            content: recorded_request.clone(),
            trust: None,
            pinned: false,
            attachments: Vec::new(),
        });
        let seq = session.conduit.next_seq();
        session
            .conduit
            .publish(ProtocolEvent::UserMessageSubmitted {
                session_id: id.clone(),
                seq,
                created_at: now_iso(),
                text: recorded_request,
                nonce: None,
                trust: None,
                attachments: Vec::new(),
            });
    }

    let st = state.clone();
    let sid = id.clone();
    tokio::spawn(async move {
        if let Err(e) = st.run_conductor(&sid, &goal, dry_run).await {
            tracing::error!(session_id = %sid, error = %e, "orchestrate handler error");
            let seq = session.conduit.next_seq();
            session.conduit.publish(ProtocolEvent::RunFailed {
                session_id: sid.clone(),
                seq,
                created_at: now_iso(),
                error: e,
                kind: "orchestration".into(),
                hint: "Check that the conductor's model and provider key are configured.".into(),
            });
        }
    });
    Ok(Json(serde_json::json!({ "accepted": true })))
}
pub(crate) async fn bind_session_spec_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<BindSessionSpecRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let spec_id = body.spec_id.trim();
    if spec_id.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let spec = {
        let store = lock_recover(&state.specs, "specs");
        store.spec(spec_id).ok_or(StatusCode::NOT_FOUND)?
    };
    let session_workspace = session_workspace_root(&state, &id)
        .await?
        .ok_or(StatusCode::NOT_FOUND)?;
    if !same_path(&session_workspace, FsPath::new(&spec.workspace_root)) {
        return Err(StatusCode::BAD_REQUEST);
    }
    update_session_spec_binding(&state, &id, Some(spec_id.to_string())).await?;
    Ok(Json(session_binding_row(&state, &id).await?))
}
pub(crate) async fn clear_session_spec_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    if !update_session_spec_binding(&state, &id, None).await? {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(Json(session_binding_row(&state, &id).await?))
}
pub(crate) async fn get_session_spec_completion_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    Ok(Json(
        session_spec_completion_summary(&state, &id, false).await?,
    ))
}
pub(crate) async fn get_tasks(
    State(state): State<DaemonState>,
    Query(q): Query<SessionQuery>,
) -> Json<serde_json::Value> {
    let mut rows = Vec::new();
    if let Some(sid) = &q.session_id {
        let tasks = if let Some(live) = state.get_session(sid) {
            lock_recover(&live.tasks, "session.tasks").clone()
        } else if let Some(storage) = &state.storage {
            // Dormant session: rebuild the latest task-graph snapshot from the
            // event log so the Tasks pane survives a restart too.
            let storage2 = storage.clone();
            let sid2 = sid.clone();
            tokio::task::spawn_blocking(move || {
                let store = storage2.blocking_lock();
                store
                    .all_events(&sid2)
                    .map(|ev| crate::rehydrate::rehydrate_tasks(&ev))
                    .unwrap_or_default()
            })
            .await
            .unwrap_or_default()
        } else {
            vec![]
        };
        rows.extend(tasks.iter().map(task_node_json));
    }
    if !q.workspace_root.trim().is_empty() {
        let root = PathBuf::from(q.workspace_root.trim());
        match load_workspace_task_records(&root) {
            Ok(records) => {
                let path = workspace_tasks_path(&root);
                let live_shell_statuses = live_shell_statuses_for_workspace(&state, &root);
                rows.extend(
                    records
                        .values()
                        .map(|record| task_record_json(record, &path, &live_shell_statuses)),
                );
            }
            Err(e) => {
                tracing::warn!(workspace_root = %root.display(), error = %e, "tasks workspace scan failed")
            }
        }
    }
    rows.sort_by(|a, b| {
        a["id"]
            .as_str()
            .unwrap_or_default()
            .cmp(b["id"].as_str().unwrap_or_default())
    });
    Json(serde_json::json!(rows))
}
pub(crate) async fn get_artifacts(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let artifacts = match state.get_session(&id) {
        Some(live) => {
            let artifacts = lock_recover(&live.artifacts, "session.artifacts").clone();
            if artifacts.is_empty() {
                load_stored_artifacts(&state, &id).await.unwrap_or_default()
            } else {
                persist_artifacts_snapshot(&state, &id, &artifacts).await;
                artifacts
            }
        }
        None => load_stored_artifacts(&state, &id)
            .await
            .ok_or(StatusCode::NOT_FOUND)?,
    };
    let rows: Vec<serde_json::Value> = artifacts
        .iter()
        .map(|a| serde_json::to_value(a).unwrap_or(serde_json::Value::Null))
        .collect();
    Ok(Json(serde_json::json!(rows)))
}
pub(crate) async fn get_taskgraph(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let live = state.get_session(&id).ok_or(StatusCode::NOT_FOUND)?;
    let tasks = lock_recover(&live.tasks, "session.tasks").clone();
    let rows: Vec<serde_json::Value> = tasks
        .iter()
        .map(|t| serde_json::to_value(t).unwrap_or(serde_json::Value::Null))
        .collect();
    Ok(Json(serde_json::json!(rows)))
}
/// F1 — per-sub-agent roster rows for the session agent rail.
pub(crate) async fn get_session_agents(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let live = state.get_session(&id).ok_or(StatusCode::NOT_FOUND)?;
    let rows = lock_recover(&live.agent_roster, "session.agent_roster").rows();
    Ok(Json(
        serde_json::to_value(rows).unwrap_or(serde_json::json!([])),
    ))
}
