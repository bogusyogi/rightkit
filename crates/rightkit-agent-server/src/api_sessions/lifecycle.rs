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
    /// Turn-scoped fork (sessions D20, rule 31 option (b)): branch from just before the
    /// parent's `userMessageSubmitted` event with this seq. Absent: the whole session.
    #[serde(default)]
    before_seq: Option<i64>,
    /// Turn-scoped fork counted from the end: branch before the Nth-last user turn (1 = the
    /// last). Ignored when `beforeSeq` is set.
    #[serde(default)]
    turns_back: Option<usize>,
    /// Durable idempotency key (CR3 #57): an identical retry replays the
    /// original response instead of creating a second session.
    #[serde(default)]
    nonce: Option<String>,
    /// The folder the fork works in; absent: the parent's. A session whose folder is gone is forked
    /// into the folder the caller is in (`run --session-id ID --fork`).
    #[serde(default)]
    workspace_root: Option<String>,
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
    /// `needsYou=true` (or `needs_you=true`): only chats waiting on the person (an approval or a question).
    #[serde(default, alias = "needs_you")]
    needs_you: Option<bool>,
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
    // What each live session is waiting on, for the state mark added at the end.
    let mut live_waits: std::collections::HashMap<String, (usize, Option<String>, usize)> =
        std::collections::HashMap::new();
    for s in &live {
        seen.insert(s.id.clone());
        {
            let approvals = lock_recover(&s.pending_approvals, "session.pending_approvals").len();
            let approval_tool = lock_recover(
                &s.pending_approval_contexts,
                "session.pending_approval_contexts",
            )
            .values()
            .next()
            .map(|context| context.tool_name.clone());
            let questions = lock_recover(&s.pending_questions, "session.pending_questions").len()
                + lock_recover(&s.pending_elicitations, "session.pending_elicitations").len();
            live_waits.insert(s.id.clone(), (approvals, approval_tool, questions));
        }
        let mut row = session_row(s);
        row["preview"] = serde_json::json!(transcript_session_preview(&lock_recover(
            &s.transcript,
            "session.transcript"
        ))
        .unwrap_or_default());
        // A live engine owns runtime status while a turn runs; between turns
        // the persisted status (idle for a top-level session, the terminal
        // status for a child) is patched in below.
        let turn_active = row["turnActive"].as_bool().unwrap_or(false);
        row["status"] = serde_json::json!(if turn_active { "running" } else { "idle" });
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
                        if entry["turnActive"].as_bool() != Some(true)
                            && !r.status.is_empty()
                            && r.status != "running"
                        {
                            entry["status"] = serde_json::json!(r.status);
                        }
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
                        entry["recoverable"] = recoverable_marker(&r.status, &r.updated_at);
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
                    // A session stored as `interrupted` is recoverable: the
                    // status is the crash record (sessions rule 34).
                    "recoverable": recoverable_marker(&r.status, &r.updated_at),
                }));
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
    // The state mark and the goal on every row. `status` keeps its old meaning; `chatState`
    // says which chat is waiting on the person and why, which finished unread, which stopped.
    {
        use super::chat_state::{
            chat_state_json, goal_json, seen_snapshot, session_is_from_schedule, ChatFacts,
        };
        let ids: Vec<String> = out
            .iter()
            .filter_map(|row| row["id"].as_str().map(str::to_owned))
            .collect();
        let (tails, goals) = match &state.storage {
            Some(storage) => {
                let storage = storage.clone();
                tokio::task::spawn_blocking(move || {
                    let store = storage.blocking_lock();
                    let mut tails = std::collections::HashMap::new();
                    let mut goals = std::collections::HashMap::new();
                    for id in ids {
                        if let Ok(tail) = store.session_tail(&id) {
                            tails.insert(id.clone(), tail);
                        }
                        if let Ok(Some(goal)) = store.newest_displayable_goal_for_session(&id) {
                            goals.insert(id, goal);
                        }
                    }
                    (tails, goals)
                })
                .await
                .unwrap_or_default()
            }
            None => Default::default(),
        };
        let seen_at = seen_snapshot();
        // Which account each chat runs on (a session's own choice, else its project's default).
        let accounts = tokio::task::spawn_blocking(crate::api_accounts::accounts_snapshot)
            .await
            .ok();
        for row in out.iter_mut() {
            let id = row["id"].as_str().unwrap_or_default().to_string();
            row["account"] = accounts
                .as_ref()
                .and_then(|snapshot| {
                    snapshot.view_for(&id, row["workspaceRoot"].as_str().unwrap_or_default())
                })
                .unwrap_or(serde_json::Value::Null);
            let waits = live_waits.get(&id);
            let stored_status = row["status"].as_str().unwrap_or_default().to_string();
            let facts = ChatFacts {
                live: waits.is_some(),
                turn_active: row["turnActive"].as_bool().unwrap_or(false),
                pending_approvals: waits.map(|w| w.0).unwrap_or(0),
                approval_tool: waits.and_then(|w| w.1.as_deref()),
                pending_questions: waits.map(|w| w.2).unwrap_or(0),
                stored_status: &stored_status,
                tail: tails.get(&id),
                seen_at: seen_at.get(&id).map(String::as_str),
                from_schedule: session_is_from_schedule(&id),
            };
            row["chatState"] = chat_state_json(&facts);
            row["goal"] = goal_json(goals.get(&id));
        }
    }
    if query.needs_you == Some(true) {
        out.retain(|row| row["chatState"]["needsYou"].as_bool() == Some(true));
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
    // Only the ids this call tombstoned are lifted again if the storage delete fails.
    let newly_tombstoned: Vec<String> = {
        let mut tombstones = lock_recover(&state.deleted_session_ids, "deleted_session_ids");
        deleted_ids
            .iter()
            .filter(|session_id| tombstones.insert((*session_id).clone()))
            .cloned()
            .collect()
    };

    // Sessions rule 41: deleting a session deletes its history snapshots. Find their
    // workspaces now, while the rows that name them still exist; the snapshots, the live
    // engines and the compaction state are only touched once the storage delete commits,
    // so a failed delete leaves the session whole (audit M3).
    let mut workspaces: Vec<(String, String)> = {
        let sessions = lock_recover(&state.sessions, "sessions");
        deleted_ids
            .iter()
            .filter_map(|session_id| sessions.get(session_id))
            .map(|live| (live.id.clone(), live.workspace_root.clone()))
            .collect()
    };
    {
        if let Some(storage) = state.storage.clone() {
            let known: HashSet<String> = workspaces.iter().map(|(id, _)| id.clone()).collect();
            let stored_ids: Vec<String> = deleted_ids
                .iter()
                .filter(|session_id| !known.contains(*session_id))
                .cloned()
                .collect();
            let stored = tokio::task::spawn_blocking(move || {
                let store = storage.blocking_lock();
                stored_ids
                    .into_iter()
                    .filter_map(|session_id| {
                        let row = store.get_session(&session_id).ok().flatten()?;
                        Some((session_id, row.workspace_root))
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default();
            workspaces.extend(stored);
        }
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
        let failed = match outcome {
            Ok(Ok(result)) => {
                deleted_persisted = result.deleted_sessions > 0;
                false
            }
            Ok(Err(err)) => {
                tracing::error!(session_id = %id, error = %err, "delete_session_tree failed");
                true
            }
            Err(join_err) => {
                tracing::error!(session_id = %id, error = %join_err, "delete_session_tree join failed");
                true
            }
        };
        if failed {
            // The delete is one transaction, so nothing durable changed: lift this call's
            // tombstones so the still-listed session keeps persisting its writes.
            {
                let mut tombstones =
                    lock_recover(&state.deleted_session_ids, "deleted_session_ids");
                for session_id in &newly_tombstoned {
                    tombstones.remove(session_id);
                }
            }
            // While tombstoned, the event persister skipped (did not buffer) every event a
            // live engine emitted. A skipped seq never reaches SQLite, so that session's
            // persister stalls behind it and its in-memory seq runs ahead of the durable log.
            // Wait briefly for writes already in flight; if the log still ends short of the
            // seqs the engine reserved, latch persistence failure. The latch interrupts the
            // engine, refuses further events and tells live clients why. The client can
            // retry the delete. A session that emitted nothing in the window stays whole.
            let live: Vec<Arc<LiveSession>> = {
                let sessions = lock_recover(&state.sessions, "sessions");
                newly_tombstoned
                    .iter()
                    .filter_map(|session_id| sessions.get(session_id).cloned())
                    .collect()
            };
            for live in live {
                let reserved = live.conduit.current_high_water();
                let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
                let durable = loop {
                    let durable = storage.lock().await.latest_seq(&live.id).ok();
                    if durable.is_some_and(|seq| seq >= reserved)
                        || tokio::time::Instant::now() >= deadline
                    {
                        break durable;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                };
                if !durable.is_some_and(|seq| seq >= reserved) {
                    let reason = format!(
                        "session delete failed and events emitted during it were not persisted \
                         (durable log ends at seq {}, engine reached seq {reserved}); \
                         the session is stopped: delete it again",
                        durable.map_or_else(|| "unknown".to_string(), |seq| seq.to_string()),
                    );
                    tracing::error!(session_id = %live.id, %reason, "failed delete left a persistence hole");
                    live.conduit.latch_persistence_failure(reason);
                }
            }
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }
    let removed: Vec<Arc<LiveSession>> = {
        let mut sessions = lock_recover(&state.sessions, "sessions");
        deleted_ids
            .iter()
            .filter_map(|session_id| sessions.remove(session_id))
            .collect()
    };
    // A deleted session never resumes with its `trustWorkspace` config grant.
    for session_id in &deleted_ids {
        crate::daemon_state::set_session_config_grant(session_id, false);
    }
    {
        let _ = tokio::task::spawn_blocking(move || {
            for (session_id, workspace_root) in workspaces {
                for root in session_history_roots(std::path::Path::new(&workspace_root), &session_id) {
                    let store = rightkit_agent_engine_history::HistoryStore::new(
                        &root,
                        rightkit_agent_config::state_dir_name(),
                        &session_id,
                    );
                    if let Err(error) = store.delete_session() {
                        tracing::warn!(session_id = %session_id, %error, "could not delete the session's history snapshots");
                    }
                }
            }
        })
        .await;
    }
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
    if !deleted_persisted && !had_live {
        return Err(StatusCode::NOT_FOUND);
    }
    // Storage rule 35: deleting a session also removes its files (shell-output copies, trace,
    // run-change records). Failures are logged and never undo the committed deletion.
    if let Some(state_root) = rightkit_agent_config::state_root() {
        let run_changes = crate::api_run_changes::shared_run_changes_store();
        for session_id in &deleted_ids {
            crate::store_write::remove_session_files(&state_root, session_id);
            run_changes.forget_session(session_id);
        }
    }
    // A deleted chat's "seen" mark would otherwise stay in the seen file for good.
    for session_id in &deleted_ids {
        super::messages::retire_steering_queue(session_id);
        super::chat_state::forget_seen(session_id);
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
/// Why a session create failed. A refusal carries its reason (tools rule 62 names the trust
/// state); every other failure is a bare status as before.
#[derive(Debug)]
pub(crate) enum CreateSessionError {
    Status(StatusCode),
    Refused {
        status: StatusCode,
        message: String,
    },
    /// The chosen account cannot be used; `body` is the plain error JSON from `api_accounts`.
    Account {
        status: StatusCode,
        body: serde_json::Value,
    },
}

impl std::fmt::Display for CreateSessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CreateSessionError::Status(status) => write!(f, "{status}"),
            CreateSessionError::Refused { status, message } => write!(f, "{status}: {message}"),
            CreateSessionError::Account { status, body } => write!(f, "{status}: {body}"),
        }
    }
}

impl From<StatusCode> for CreateSessionError {
    fn from(status: StatusCode) -> Self {
        CreateSessionError::Status(status)
    }
}

impl axum::response::IntoResponse for CreateSessionError {
    fn into_response(self) -> axum::response::Response {
        match self {
            CreateSessionError::Status(status) => status.into_response(),
            CreateSessionError::Account { status, body } => (status, Json(body)).into_response(),
            CreateSessionError::Refused { status, message } => (
                status,
                Json(serde_json::json!({ "error": message, "code": "bypass_refused_untrusted" })),
            )
                .into_response(),
        }
    }
}

/// Rule 62: `Bypass` combined with an untrusted workspace is refused at session create, whether
/// the mode came from the request or from the workspace config.
fn create_bypass_refusal(state: &DaemonState, body: &CreateSession) -> Option<String> {
    let root = body.workspace_root.trim();
    let requested = body
        .permission_mode
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let mode = match requested {
        Some(mode) => rightkit_agent_policy::parse_permission_mode_strict(mode)?,
        None => {
            // C6: the request's `trustWorkspace` makes the configured mode visible here (so a
            // `bypass` the workspace file configures is refused), while the trust below still
            // comes from the store only: the grant never enables bypass.
            let config = rightkit_agent_config::ConfigLoader::new()
                .load_files_for_workspace_granted(FsPath::new(root), body.trust_workspace);
            let configured = config.permissions.mode.trim();
            if !configured.is_empty() {
                rightkit_agent_policy::parse_permission_mode(Some(configured))
            } else {
                // No mode anywhere: the engine's defaults (`Ask`, or `AutoSafe` for a session with
                // a `parentSessionId`) are never `Bypass`.
                return None;
            }
        }
    };
    rightkit_agent_policy::TrustPolicy::bypass_refusal(mode, state.session_workspace_trust(root))
}

/// HTTP entry for `POST /sessions`. A `trustWorkspace` grant (issue 445) is honoured only from a
/// trusted source, the same authority every other mutation demands; from a lower-trust source
/// (for example a mobile client) the grant is dropped and the session is created untrusted.
pub(crate) async fn create_session_route(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(mut body): Json<CreateSession>,
) -> Result<Json<serde_json::Value>, CreateSessionError> {
    if body.trust_workspace
        && crate::api_permissions::enforce_trusted_source_authority(&headers).is_err()
    {
        tracing::warn!("trustWorkspace dropped: the request is not from a trusted source");
        body.trust_workspace = false;
    }
    create_session_handler(State(state), Json(body)).await
}

pub(crate) async fn create_session_handler(
    State(state): State<DaemonState>,
    Json(body): Json<CreateSession>,
) -> Result<Json<serde_json::Value>, CreateSessionError> {
    if let Some(message) = create_bypass_refusal(&state, &body) {
        return Err(CreateSessionError::Refused {
            status: StatusCode::FORBIDDEN,
            message,
        });
    }
    // An unknown or unready account is refused with a plain message before anything is created.
    if let Some(account_id) = body.account_id.as_deref() {
        crate::api_accounts::validate_new_session_account(account_id)
            .map_err(|(status, Json(body))| CreateSessionError::Account { status, body })?;
    }
    create_session_checked(State(state), Json(body))
        .await
        .map_err(CreateSessionError::Status)
}

/// Ids a create request is still working on. The 409 checks below are separated from the
/// insert by awaits, and the insert hands back an existing session with the same id, so a
/// second concurrent create must be refused here rather than given the first caller's
/// session (audit M2).
static CREATING_SESSION_IDS: std::sync::LazyLock<std::sync::Mutex<HashSet<String>>> =
    std::sync::LazyLock::new(Default::default);

/// Releases a `CREATING_SESSION_IDS` claim when the request ends, however it ends.
struct CreatingSessionId(String);

impl Drop for CreatingSessionId {
    fn drop(&mut self) {
        CREATING_SESSION_IDS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.0);
    }
}

async fn create_session_checked(
    State(state): State<DaemonState>,
    Json(body): Json<CreateSession>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let explicit_id = body.id.is_some();
    if body
        .id
        .as_deref()
        .is_some_and(|id| !is_safe_client_session_id(id))
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let id = body.id.unwrap_or_else(|| format!("s-{}", uuid_v7()));
    if !CREATING_SESSION_IDS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(id.clone())
    {
        return Err(StatusCode::CONFLICT);
    }
    let _creating = CreatingSessionId(id.clone());
    if state.get_session(&id).is_some() {
        return Err(StatusCode::CONFLICT);
    }
    if explicit_id {
        // A stored (dormant) session already owns this id. Creating a fresh one over it would
        // restart the event counter at 1 and collide with the durable rows, so the caller must
        // resume it instead (the message route does that), exactly as for a live session.
        if let Some(storage) = state.storage.clone() {
            if storage
                .lock()
                .await
                .get_session(&id)
                .ok()
                .flatten()
                .is_some()
            {
                return Err(StatusCode::CONFLICT);
            }
        }
    }
    // Per-session `trustWorkspace` grant (issue 445), honoured on this create only: the resume
    // path (409 / `ensure_session`) and `patch_route` never reach here. It needs an `Inside`
    // binding that is neither the filesystem root nor the home directory.
    let trust_grant =
        body.trust_workspace && crate::daemon_state::trust_grant_allowed(&body.workspace_root);
    // Issue 494 item 4: a grant that was asked for and not given is reported in the response.
    let trust_grant_dropped = body.trust_workspace && !trust_grant;
    // Issue 489: an explicit own toolset must name a real profile (fail closed, visibly).
    if let Some(own) = body
        .own_toolset
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        let config = crate::daemon_state::load_workspace_config_granted(
            FsPath::new(body.workspace_root.trim()),
            trust_grant,
        );
        if crate::api_toolsets::resolve_toolset_profile_with_config(own, &config).is_err() {
            tracing::warn!(toolset = %own, "create refused: ownToolset is not a known profile");
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    let workspace_config = rightkit_agent_config::ConfigLoader::new()
        .load_files_for_workspace_granted(FsPath::new(body.workspace_root.trim()), trust_grant);
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
    // The chosen account is recorded as this session's selection before the session exists, and
    // taken back if creation then fails, so a chat never starts on a different account than the
    // one asked for and a failed create leaves no selection behind.
    let account_selection = match body.account_id.as_deref() {
        Some(account_id) => Some(
            crate::api_accounts::record_new_session_account(&id, account_id)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        ),
        None => None,
    };
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
            own_toolset: body.own_toolset.clone(),
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
            output_schema: body.output_schema,
            add_dir: body.add_dir,
            bare: body.bare,
            fork_seed: None,
            fork_system_prompt: None,
            no_mcp: body.no_mcp,
            mcp_config: body.mcp_config,
            trust_workspace: trust_grant,
            config_grant: false,
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
            crate::daemon_state::set_session_config_grant(&live.id, false);
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
                crate::daemon_state::set_session_config_grant(&live.id, false);
                return Err(StatusCode::SERVICE_UNAVAILABLE);
            }
        }
    }
    if let Some(selection) = account_selection {
        selection.keep();
    }
    let mut row = session_row(&live);
    if trust_grant_dropped {
        row["trustWorkspaceDropped"] = serde_json::Value::String(TRUST_GRANT_DROPPED.to_string());
    }
    Ok(Json(row))
}

/// Why a `trustWorkspace` grant was not given (issue 494 item 4). The session is still created,
/// untrusted, exactly as without the flag.
const TRUST_GRANT_DROPPED: &str = "the trust grant was not applied: it covers a project folder only, never the home directory, the filesystem root or a path outside a project; the session was created untrusted";

fn effective_session_toolset(safe_mode: bool, requested: Option<&str>) -> Option<&str> {
    if safe_mode {
        Some("read-only")
    } else {
        requested
    }
}

/// The `recoverable` field of a session row: set only for a session whose last
/// turn never finished (status `interrupted`), `null` otherwise.
pub(crate) fn recoverable_marker(status: &str, last_active_at: &str) -> serde_json::Value {
    if status == "interrupted" {
        serde_json::json!({ "lastActiveAt": last_active_at })
    } else {
        serde_json::Value::Null
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
        &serde_json::json!({ "id": body.id, "title": body.title, "beforeSeq": body.before_seq, "turnsBack": body.turns_back, "workspaceRoot": body.workspace_root }),
    );
    let storage = state.storage.clone();
    let scope_id = parent_id.clone();
    super::idempotency::run_idempotent(storage, &scope_id, "fork_session", nonce, digest, || {
        fork_session_once(state, parent_id, headers, body)
    })
    .await
}

/// Fork a session: a whole-session copy that becomes a new top-level, dormant
/// session with one predecessor link (`forkedFromSessionId`). It is not a
/// sub-agent and not an empty child (sessions rules 7, 8 and 31). Only the
/// canonical replay facts are copied, no prior tool action is dispatched
/// again, and the copy is refused with 409 while the source has an active
/// turn. The fork's first message resumes it from the copied transcript in the
/// default permission mode, never Bypass (rule 35); it carries no child
/// ceiling (rule 38).
async fn fork_session_once(
    state: DaemonState,
    parent_id: String,
    headers: HeaderMap,
    body: ForkSessionRequest,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let parent_live = state.get_session(&parent_id);
    if parent_live
        .as_ref()
        .is_some_and(|live| live.live_turn_handle.is_active())
    {
        return Err(StatusCode::CONFLICT);
    }
    if let Some(live) = &parent_live {
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
    if body
        .id
        .as_deref()
        .is_some_and(|id| !is_safe_client_session_id(id))
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let id = body.id.unwrap_or_else(|| format!("s-{}", uuid_v7()));
    if state.get_session(&id).is_some() {
        return Err(StatusCode::CONFLICT);
    }
    let id_for_storage = id.clone();
    let parent_id_for_storage = parent_id.clone();
    let title_override = body.title;
    let workspace_override = body
        .workspace_root
        .as_deref()
        .map(str::trim)
        .filter(|root| !root.is_empty())
        .map(str::to_string);
    let before_seq = body.before_seq;
    let turns_back = body.turns_back;
    let result = tokio::task::spawn_blocking(move || {
        let store = storage.blocking_lock();
        if store
            .get_session(&id_for_storage)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .is_some()
        {
            return Err(StatusCode::CONFLICT);
        }
        let parent_row = store
            .get_session(&parent_id_for_storage)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .ok_or(StatusCode::NOT_FOUND)?;
        let title = title_override
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("Fork of {}", parent_row.title));
        let now = now_iso();
        let mut config: serde_json::Value =
            serde_json::from_str(&parent_row.config_json).unwrap_or_else(|_| serde_json::json!({}));
        if let Some(object) = config.as_object_mut() {
            object.insert(
                "forkedFromSessionId".to_string(),
                serde_json::Value::String(parent_id_for_storage.clone()),
            );
        }
        let row = rightkit_agent_engine_storage::SessionRow {
            id: id_for_storage.clone(),
            title: title.clone(),
            workspace_root: workspace_override.unwrap_or_else(|| parent_row.workspace_root.clone()),
            created_at: now.clone(),
            updated_at: now,
            parent_session_id: None,
            role: parent_row.role.clone(),
            status: "dormant".to_string(),
            config_json: serde_json::to_string(&config).unwrap_or_else(|_| "{}".to_string()),
            tags: parent_row.tags.clone(),
            recovery_marker: None,
        };
        store
            .create_session(&row)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let mut parent_events = store
            .all_events(&parent_id_for_storage)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        // A turn-scoped fork cuts just before one of the parent's own user turns.
        let mut cut_at: Option<String> = None;
        let before_seq = match (before_seq, turns_back) {
            (Some(seq), _) => Some(seq),
            (None, Some(back)) => {
                let user_turns: Vec<i64> = parent_events
                    .iter()
                    .filter(|event| event.kind == "userMessageSubmitted")
                    .map(|event| event.seq)
                    .collect();
                if back == 0 || back > user_turns.len() {
                    return Err(StatusCode::BAD_REQUEST);
                }
                Some(user_turns[user_turns.len() - back])
            }
            (None, None) => None,
        };
        if let Some(before) = before_seq {
            let cut = parent_events
                .iter()
                .find(|event| event.seq == before && event.kind == "userMessageSubmitted")
                .ok_or(StatusCode::BAD_REQUEST)?;
            cut_at = Some(cut.created_at.clone());
            parent_events.retain(|event| event.seq < before);
        }
        let mut next_seq = store
            .latest_seq(&id_for_storage)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            + 1;
        // Parse outside the storage lock and append in chunks, so a long log
        // does not freeze every other session while it copies. The new id is
        // private until this returns, so nobody else writes to it.
        drop(store);
        let mut prepared: Vec<(String, String)> = Vec::new();
        for parent_event in parent_events {
            let Ok(mut payload) =
                serde_json::from_str::<serde_json::Value>(&parent_event.payload_json)
            else {
                continue;
            };
            let is_replay_fact = matches!(
                parent_event.kind.as_str(),
                "userMessageSubmitted"
                    | "assistantMessageCompleted"
                    | "toolCallStarted"
                    | "toolApprovalRequested"
                    | "toolResultAdded"
                    | "sessionCompacted"
                    | "sessionContextFolded"
            ) || (parent_event.kind == "humanSteeringInjected"
                && matches!(payload["injectedBy"].as_str(), Some("rewind" | "redo")));
            // Rewind and redo markers replay, so the fork's transcript matches the parent's.
            if !is_replay_fact {
                continue;
            }
            if payload["sessionId"].as_str() != Some(parent_id_for_storage.as_str()) {
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
            if parent_event.kind == "userMessageSubmitted" {
                object.insert("nonce".to_string(), serde_json::Value::Null);
            }
            let Ok(mut event) = serde_json::from_value::<ProtocolEvent>(payload) else {
                continue;
            };
            event.canonicalize_identity(next_seq);
            let payload =
                serde_json::to_string(&event).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            prepared.push((parent_event.kind.clone(), payload));
            next_seq += 1;
        }
        let copied = prepared.len();
        for chunk in prepared.chunks(200) {
            let mut store = storage.blocking_lock();
            for (kind, payload) in chunk {
                store
                    .append_event(&id_for_storage, kind, payload)
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            }
        }
        Ok::<_, StatusCode>((row, copied, cut_at, before_seq))
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let (row, copied, cut_at, before_seq) = match result {
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
    let cfg: serde_json::Value =
        serde_json::from_str(&row.config_json).unwrap_or(serde_json::Value::Null);
    // The copied `inferenceSelection` resumes only with a binding keyed to the
    // fork's own id (`resume_binding_matches`), so qualify and bind it as session
    // create does. It runs before the history copy, so a failure here rolls back
    // with the row delete alone and leaves no copied snapshots on disk.
    if let Some(selection) = cfg.get("inferenceSelection").cloned().and_then(|value| {
        serde_json::from_value::<crate::inference_runtime::InferenceSelection>(value).ok()
    }) {
        let bound =
            match crate::inference_runtime::ensure_inference_selection_qualified(&state, &selection)
                .await
            {
                // The fork may not move the parent's route: a parent whose stored
                // binding drifted cannot resume, so neither may its copy.
                Ok(route) => match crate::inference_runtime::resume_binding_matches(
                    &state, &parent_id, &route,
                )
                .await
                {
                    Ok(()) => crate::inference_runtime::bind_inference_route(
                        &state, &id, &id, None, &route,
                    )
                    .await
                    .map_err(|error| {
                        tracing::warn!(session_id = %id, error = ?error, "the fork could not bind the parent's inference route");
                        StatusCode::SERVICE_UNAVAILABLE
                    }),
                    Err(error) => {
                        tracing::warn!(session_id = %id, error = ?error, "fork refused: the parent's stored inference binding does not match");
                        Err(StatusCode::BAD_REQUEST)
                    }
                },
                // Session create answers the same qualification failure with 400.
                Err(error) => {
                    tracing::warn!(session_id = %id, error = ?error, "fork refused: the parent's inference selection no longer qualifies");
                    Err(StatusCode::BAD_REQUEST)
                }
            };
        if let Err(status) = bound {
            if let Some(storage) = state.storage.clone() {
                let id = id.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    storage.blocking_lock().delete_session_tree(&id)
                })
                .await;
            }
            return Err(status);
        }
    }
    // Sessions rules 41 and 42: the fork gets its own copy of the parent's
    // snapshots (the whole active branch), never a shared lineage.
    {
        // A session whose bypass mode forced isolation ran (and kept its history) in
        // `<workspace>/<state-dir>/worktrees/<id>`; the fork runs in the workspace itself
        // (see `fork_history_roots`).
        let workspace = std::path::PathBuf::from(&row.workspace_root);
        let (parent, child) = (parent_id.clone(), id.clone());
        let copied_history = tokio::task::spawn_blocking(move || {
            let (parent_root, child_root) = fork_history_roots(&workspace, &parent);
            rightkit_agent_engine_history::HistoryStore::fork_from(
                &parent_root,
                &child_root,
                rightkit_agent_config::state_dir_name(),
                &parent,
                &child,
                history_cutoff_ns(cut_at.as_deref()),
            )
        })
        .await;
        if !matches!(copied_history, Ok(Ok(()))) {
            tracing::error!(session_id = %id, "the fork could not copy the parent's history snapshots");
            if let Some(storage) = state.storage.clone() {
                let id = id.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    storage.blocking_lock().delete_session_tree(&id)
                })
                .await;
            }
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
    }
    Ok(Json(serde_json::json!({
        "id": row.id,
        "forkedFromSessionId": parent_id,
        "title": row.title,
        "workspaceRoot": crate::api::api_workspace_root(&row.workspace_root),
        "status": row.status,
        "copiedEvents": copied,
        "beforeSeq": before_seq,
        "session": {
            "id": row.id,
            "title": row.title,
            "model": cfg.get("model").and_then(|v| v.as_str()).unwrap_or(""),
            "workspaceRoot": crate::api::api_workspace_root(&row.workspace_root),
            "status": row.status,
            "role": row.role,
            "toolset": cfg.get("toolset").cloned().unwrap_or(serde_json::Value::Null),
            "activeSkills": cfg.get("activeSkills").cloned().unwrap_or_else(|| serde_json::json!([])),
            "createdAt": row.created_at,
            "updatedAt": row.updated_at,
            "parentSessionId": serde_json::Value::Null,
            "forkedFromSessionId": parent_id,
        }
    })))
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
        let mut events = store.all_events(&id2)?;
        // Audit M4: show oversized events whole, as the live transcript did.
        store.restore_spilled_payloads(&id2, &mut events)?;
        Ok::<_, rightkit_agent_engine_storage::StorageError>(events)
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
    // A killed session goes dormant; rehydrate never revives the config grant.
    crate::daemon_state::set_session_config_grant(&id, false);
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
    let mut cache_warning: Option<String> = None;
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
        // Judged before the switch commits: afterwards the live model is already the new one.
        cache_warning = model_updated
            .then(|| crate::api::model_switch_cache_warning(&state, &id, &model))
            .flatten();
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
    let mut reply = serde_json::json!({
        "updated": true,
        "id": id,
        "modelUpdated": model_updated,
        "modelAppliesTo": model_applies_to,
        "driverKindUpdated": driver_kind_updated,
        "driverKind": driver_kind_value,
        "inferenceSelectionUpdated": inference_selection_updated,
        "inferenceSelectionAppliesTo": inference_selection_applies_to,
    });
    // A warning, not a refusal: the switch above already committed.
    if let Some(warning) = cache_warning {
        reply["cacheWarning"] = serde_json::Value::String(warning);
    }
    Ok(Json(reply))
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
        apply_live_session_status(state, id, status).await?;
        return Ok(());
    };
    let id_owned = id.to_string();
    let status_owned = status;
    let updated_at = now.clone();
    let updated = tokio::task::spawn_blocking(move || {
        let store = store.blocking_lock();
        if store.get_session(&id_owned)?.is_none() {
            return Ok(false);
        }
        store.update_session_status(&id_owned, status_owned, &updated_at)?;
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
    apply_live_session_status(state, id, status).await
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
        crate::daemon_state::set_session_config_grant(id, false);
    } else if status == "running" {
        state
            .resume_session(id)
            .await
            .ok_or(StatusCode::NOT_FOUND)?;
    }
    Ok(())
}

/// A client-chosen session id becomes a directory name (history snapshots,
/// isolation worktrees, traces), so refuse ids that are empty, `.`/`..`,
/// contain a path separator or control character, or are unreasonably long.
/// The rule is the same on every OS, because a session can move between hosts:
/// ids Windows cannot hold as a directory name are refused everywhere too
/// (the characters `: * ? " < > |`, a trailing `.` or space, and the reserved
/// device names `CON`, `PRN`, `AUX`, `NUL`, `COM1`-`COM9`, `LPT1`-`LPT9`, with
/// or without an extension). Windows rejects or rewrites those names (`...`
/// resolves to the parent directory), so the isolation worktree would fall
/// back to running in place (audit 2026-11-05 round 2, finding 5).
pub(super) fn is_safe_client_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id != "."
        && id != ".."
        && !id.chars().any(|c| {
            matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control()
        })
        && !id.ends_with('.')
        && !id.ends_with(' ')
        && !is_windows_reserved_name(id)
}

/// Whether `name` is a Windows device name (`CON`, `NUL`, `COM1`, `LPT9`, ...),
/// in any case, alone or followed by an extension (`nul.txt`) or trailing spaces.
fn is_windows_reserved_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    let upper = stem.to_ascii_uppercase();
    match upper.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" => true,
        _ => {
            let bytes = upper.as_bytes();
            bytes.len() == 4
                && (upper.starts_with("COM") || upper.starts_with("LPT"))
                && (b'1'..=b'9').contains(&bytes[3])
        }
    }
}

/// Where a fork's history copy is read from and written to (audit 2026-11-05
/// round 2, finding 1): `(parent_root, child_root)`.
///
/// The parent's history lives where it ran: its forced isolation worktree
/// (`<workspace>/<state-dir>/worktrees/<id>`, a real checkout with `.git`) when
/// bypass mode forced one, else the workspace. A fork never runs in such a
/// worktree: it is a stored session that only ever resumes, a resume restores
/// Ask or Plan and never Bypass (sessions rule 35), and only Bypass forces
/// isolation. So the fork runs, and keeps its history, in the stored workspace
/// root, and the copy goes there, its paths rebased from the parent's worktree
/// onto the workspace. The old target, `worktrees/<child>`, was read by nothing,
/// survived session delete, and, once written, made any later `git worktree
/// add` for that id refuse the non-empty directory.
fn fork_history_roots(
    workspace: &std::path::Path,
    parent_id: &str,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let parent_isolated = workspace
        .join(rightkit_agent_config::state_dir_name())
        .join("worktrees")
        .join(parent_id);
    if parent_isolated.join(".git").exists() {
        (parent_isolated, workspace.to_path_buf())
    } else {
        (workspace.to_path_buf(), workspace.to_path_buf())
    }
}

/// Every root a session's history may live under: the workspace, and its
/// isolation worktree directory when one exists (a session that ran isolated
/// and later resumed in place has history in both). Deleting both also removes
/// a copy an earlier fork wrote into `worktrees/<child>`, a directory that never
/// became a checkout (audit 2026-11-05 round 2, finding 1).
fn session_history_roots(workspace: &std::path::Path, session_id: &str) -> Vec<std::path::PathBuf> {
    let mut roots = vec![workspace.to_path_buf()];
    let isolated = workspace
        .join(rightkit_agent_config::state_dir_name())
        .join("worktrees")
        .join(session_id);
    if isolated.is_dir() {
        roots.push(isolated);
    }
    roots
}

/// History snapshots a turn-scoped fork keeps: those taken before the cut user turn
/// (`created_at`: epoch seconds as the event log stores it, or RFC 3339 for older
/// rows), or all of them for a whole-session fork.
fn history_cutoff_ns(cut_at: Option<&str>) -> u128 {
    cut_at
        .and_then(|at| {
            at.trim().parse::<u128>().ok().or_else(|| {
                rightkit_time::DateTime::parse_rfc3339(at)
                    .and_then(|at| u128::try_from(at.unix_seconds()).ok())
            })
        })
        .map(|secs| secs * 1_000_000_000)
        .unwrap_or(u128::MAX)
}

#[cfg(test)]
mod client_session_id_tests {
    use super::is_safe_client_session_id;

    #[test]
    fn ids_that_could_escape_a_directory_are_refused() {
        for id in [
            "", ".", "..", "../x", "a/b", "a\\b", "..\\..", "/", "a\0b", "a\nb",
        ] {
            assert!(!is_safe_client_session_id(id), "{id:?}");
        }
        assert!(!is_safe_client_session_id(&"x".repeat(257)));
    }

    #[test]
    fn ordinary_ids_are_accepted() {
        for id in [
            "s-0193a1b2",
            "my-session",
            "a.b",
            "日本",
            "console",
            "COM0",
            "LPT10",
            "nul-x",
        ] {
            assert!(is_safe_client_session_id(id), "{id:?}");
        }
    }

    #[test]
    fn ids_windows_cannot_hold_as_a_directory_are_refused_everywhere() {
        for id in [
            "x:y", "a*b", "a?b", "a\"b", "a<b", "a>b", "a|b", "...", "a.", "a ", "CON", "con",
            "Nul", "nul.txt", "AUX", "PRN", "COM1", "com9.log", "LPT1", "CON .x",
        ] {
            assert!(!is_safe_client_session_id(id), "{id:?}");
        }
    }
}

#[cfg(test)]
mod fork_history_tests {
    use super::{fork_history_roots, session_history_roots};

    fn git(dir: &std::path::Path, args: &[&str]) -> bool {
        rightkit_child_process::command_std("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .is_ok_and(|output| output.status.success())
    }

    #[test]
    fn a_fork_of_an_isolated_parent_copies_into_the_workspace_it_runs_in() {
        let ws = tempfile::tempdir().unwrap();
        let root = ws.path();
        if !git(root, &["init", "-q"]) {
            eprintln!("git unavailable; skipped");
            return;
        }
        std::fs::write(root.join("f.txt"), b"x").unwrap();
        assert!(git(root, &["add", "f.txt"]));
        assert!(git(
            root,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "init"
            ]
        ));
        let worktrees = root
            .join(rightkit_agent_config::state_dir_name())
            .join("worktrees");
        let parent = worktrees.join("P");
        std::fs::create_dir_all(&parent).unwrap();
        assert!(git(
            root,
            &[
                "worktree",
                "add",
                "--detach",
                "--force",
                parent.to_str().unwrap(),
                "HEAD"
            ]
        ));
        let parent_file = parent.join("f.txt");
        let store = rightkit_agent_engine_history::HistoryStore::new(
            &parent,
            rightkit_agent_config::state_dir_name(),
            "P",
        );
        store
            .snapshot_targets(std::slice::from_ref(&parent_file))
            .unwrap();
        std::fs::write(&parent_file, b"y").unwrap();
        assert!(store
            .record_after_targets(std::slice::from_ref(&parent_file))
            .is_empty());

        let (parent_root, child_root) = fork_history_roots(root, "P");
        assert_eq!(parent_root, parent);
        assert_eq!(child_root, root);
        rightkit_agent_engine_history::HistoryStore::fork_from(
            &parent_root,
            &child_root,
            rightkit_agent_config::state_dir_name(),
            "P",
            "C",
            u128::MAX,
        )
        .unwrap();
        assert!(
            !worktrees.join("C").exists(),
            "nothing is written where a `git worktree add` for the child would go"
        );
        let copy = root
            .join(rightkit_agent_config::state_dir_name())
            .join("history")
            .join("C");
        assert!(copy.is_dir(), "the copy is where the child runs");

        // The copy is the child's: rebased onto the workspace, so a rewind in the child
        // restores the workspace file and never touches the parent's worktree.
        std::fs::write(root.join("f.txt"), b"y").unwrap();
        let report = rightkit_agent_engine_history::HistoryStore::new(
            root,
            rightkit_agent_config::state_dir_name(),
            "C",
        )
        .restore_since(root, 0);
        assert_eq!(report.restored.len(), 1, "{report:?}");
        assert_eq!(std::fs::read(root.join("f.txt")).unwrap(), b"x");
        assert_eq!(std::fs::read(&parent_file).unwrap(), b"y");

        // Session delete removes the copy.
        for history_root in session_history_roots(root, "C") {
            rightkit_agent_engine_history::HistoryStore::new(
                &history_root,
                rightkit_agent_config::state_dir_name(),
                "C",
            )
            .delete_session()
            .unwrap();
        }
        assert!(!copy.exists());
    }

    #[test]
    fn a_fork_of_a_parent_that_ran_in_place_copies_in_place() {
        let ws = tempfile::tempdir().unwrap();
        let (parent_root, child_root) = fork_history_roots(ws.path(), "P");
        assert_eq!(parent_root, ws.path());
        assert_eq!(child_root, ws.path());
    }

    #[test]
    fn delete_also_clears_a_copy_left_in_a_directory_that_never_became_a_worktree() {
        let ws = tempfile::tempdir().unwrap();
        let stray = ws
            .path()
            .join(rightkit_agent_config::state_dir_name())
            .join("worktrees")
            .join("C");
        let store = rightkit_agent_engine_history::HistoryStore::new(
            &stray,
            rightkit_agent_config::state_dir_name(),
            "C",
        );
        let file = stray.join("f.txt");
        std::fs::create_dir_all(&stray).unwrap();
        std::fs::write(&file, b"x").unwrap();
        store.snapshot_targets(&[file]).unwrap();
        let copy = stray
            .join(rightkit_agent_config::state_dir_name())
            .join("history")
            .join("C");
        assert!(copy.is_dir());
        for root in session_history_roots(ws.path(), "C") {
            rightkit_agent_engine_history::HistoryStore::new(
                &root,
                rightkit_agent_config::state_dir_name(),
                "C",
            )
            .delete_session()
            .unwrap();
        }
        assert!(!copy.exists());
    }
}
