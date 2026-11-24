//! The axum routes of the attention inbox, behind the non-default `server` feature.
//! A host that wants only the inbox, routing and store logic leaves the feature off and does not pull axum.

use axum::{
    extract::{FromRef, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{get, patch, post},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::*;

/// Refuse requests whose source-trust header marks them as lower-trust input.
fn enforce_trusted_source_authority(headers: &HeaderMap) -> Result<(), StatusCode> {
    let Some(value) = headers.get(config().source_trust_header.as_str()) else {
        return Ok(());
    };
    let value = value.to_str().map_err(|_| StatusCode::BAD_REQUEST)?;
    let normalized: String = value
        .trim()
        .chars()
        .filter(|ch| !matches!(ch, '-' | '_' | ' '))
        .collect::<String>()
        .to_ascii_lowercase();
    match normalized.as_str() {
        "remoteagentuntrusted" | "untrusted" | "tooluntrusted" => Err(StatusCode::FORBIDDEN),
        _ => Ok(()),
    }
}

/// Empty axum sub-state for the attention route group. No attention handler
/// ever reads daemon state (verified: every handler body reduces to
/// `let _ = state;` and `state_default_workspace_root` below ignores its
/// argument entirely), so the sub-state carries no fields. The binary crate
/// implements `axum::extract::FromRef<DaemonState> for AttentionRouterState`
/// so this router can be `.merge()`d into the daemon's `Router<DaemonState>`.
#[derive(Clone, Copy, Default)]
pub struct AttentionRouterState;

pub fn attention_routes<S>() -> Router<S>
where
    AttentionRouterState: FromRef<S>,
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/attention/items", get(list_attention_handler))
        .route(
            "/attention/items/{id}/state",
            patch(update_attention_state_handler),
        )
        .route(
            "/attention/items/{id}/snooze",
            post(snooze_attention_handler),
        )
        .route(
            "/attention/items/{id}/unsnooze",
            post(unsnooze_attention_handler),
        )
        .route(
            "/attention/density",
            get(get_density_handler).patch(set_density_handler),
        )
        .route(
            "/attention/policy",
            get(get_policy_handler).patch(set_policy_handler),
        )
        .route(
            "/attention/quiet-hours",
            get(get_quiet_hours_handler).patch(set_quiet_hours_handler),
        )
        .route("/attention/deeplink", post(resolve_deeplink_handler))
        .route("/attention/banner", get(get_attention_banner_handler))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DensityRequest {
    workspace_root: String,
    density: TranscriptDensity,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceQuery {
    workspace_root: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateAttentionStateRequest {
    workspace_root: String,
    state: AttentionState,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SnoozeRequest {
    workspace_root: String,
    snooze_until: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuietHoursRequest {
    workspace_root: String,
    enabled: bool,
    windows: Vec<QuietHoursWindow>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeeplinkRequest {
    workspace_root: String,
    target: String,
    params: serde_json::Map<String, Value>,
}

fn state_default_workspace_root(state: &AttentionRouterState) -> String {
    let _ = state;
    ".".to_string()
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

async fn list_attention_handler(
    State(state): State<AttentionRouterState>,
    Query(query): Query<WorkspaceQuery>,
) -> Result<Json<AttentionListResponse>, StatusCode> {
    let workspace_root = query
        .workspace_root
        .clone()
        .unwrap_or_else(|| state_default_workspace_root(&state));
    if !enforce_workspace_root(&workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let density = load_density(&workspace_root);
    let quiet_hours_active =
        quiet_hours_active_in_zones(&workspace_root, now_seconds(), &zone_offset_secs);
    let items = build_attention_items(&workspace_root, density);
    let counts = count_attention(&items);
    Ok(Json(AttentionListResponse {
        workspace_root,
        density,
        items,
        counts,
        quiet_hours_active,
    }))
}

async fn update_attention_state_handler(
    State(state): State<AttentionRouterState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<UpdateAttentionStateRequest>,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let _ = state;
    if !enforce_workspace_root(&req.workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    // Update is reflected in the in-memory attention store; persisted
    // through the existing notifications file path so that the on-disk store
    // remains the durable source of truth across restarts.
    let notifications_path = std::path::PathBuf::from(&req.workspace_root)
        .join(&config().state_dir)
        .join("notifications.json");
    let guard = lock_attention_state();
    let mut rows: Vec<Value> = read_notification_rows(&notifications_path);
    let mut updated = false;
    for row in rows.iter_mut() {
        if row.get("id").and_then(Value::as_str) == Some(id.as_str()) {
            row["state"] = json!(req.state.as_str());
            row["updatedAt"] = json!(now_iso());
            updated = true;
        }
    }
    if !updated {
        rows.push(json!({
            "id": id,
            "kind": "attention",
            "title": "",
            "body": "",
            "source": "attention",
            "state": req.state.as_str(),
            "createdAt": now_iso(),
            "updatedAt": now_iso(),
            "read": req.state == AttentionState::Complete,
        }));
    }
    write_json(&notifications_path, &rows).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    drop(guard);
    Ok(Json(json!({
        "id": id,
        "state": req.state.as_str(),
        "updated": true,
        "updatedAt": now_iso(),
    })))
}

async fn snooze_attention_handler(
    State(state): State<AttentionRouterState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<SnoozeRequest>,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let _ = state;
    if !enforce_workspace_root(&req.workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    ensure_attention_dir(&req.workspace_root).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    {
        let _guard = lock_attention_state();
        let mut map = read_json::<SnoozeMap>(&snooze_path(&req.workspace_root)).unwrap_or_default();
        map.by_id.insert(id.clone(), req.snooze_until.clone());
        write_json(&snooze_path(&req.workspace_root), &map)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    Ok(Json(json!({
        "id": id,
        "snoozedUntil": req.snooze_until,
        "saved": true,
    })))
}

async fn unsnooze_attention_handler(
    State(state): State<AttentionRouterState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let _ = state;
    let workspace_root = query
        .workspace_root
        .clone()
        .ok_or(StatusCode::BAD_REQUEST)?;
    if !enforce_workspace_root(&workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    {
        let _guard = lock_attention_state();
        let mut map = read_json::<SnoozeMap>(&snooze_path(&workspace_root)).unwrap_or_default();
        map.by_id.remove(&id);
        write_json(&snooze_path(&workspace_root), &map)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    Ok(Json(json!({
        "id": id,
        "saved": true,
    })))
}

async fn get_density_handler(
    State(state): State<AttentionRouterState>,
    Query(query): Query<WorkspaceQuery>,
) -> Result<Json<DensityResponse>, StatusCode> {
    let workspace_root = query
        .workspace_root
        .clone()
        .unwrap_or_else(|| state_default_workspace_root(&state));
    if !enforce_workspace_root(&workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let density = load_density(&workspace_root);
    Ok(Json(DensityResponse {
        workspace_root,
        density,
        durable_history: true,
    }))
}

async fn set_density_handler(
    State(state): State<AttentionRouterState>,
    headers: HeaderMap,
    Json(req): Json<DensityRequest>,
) -> Result<Json<DensityResponse>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let _ = state;
    if !enforce_workspace_root(&req.workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    ensure_attention_dir(&req.workspace_root).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let body = json!({
        "workspaceRoot": req.workspace_root,
        "density": req.density.as_str(),
        "durableHistory": true,
        "savedAt": now_iso(),
    });
    write_json(&density_path(&req.workspace_root), &body)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(DensityResponse {
        workspace_root: req.workspace_root,
        density: req.density,
        durable_history: true,
    }))
}

async fn get_policy_handler(
    State(state): State<AttentionRouterState>,
    Query(query): Query<WorkspaceQuery>,
) -> Result<Json<ProjectNotificationPolicy>, StatusCode> {
    let workspace_root = query
        .workspace_root
        .clone()
        .unwrap_or_else(|| state_default_workspace_root(&state));
    if !enforce_workspace_root(&workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(Json(load_policy(&workspace_root)))
}

async fn set_policy_handler(
    State(state): State<AttentionRouterState>,
    headers: HeaderMap,
    Json(policy): Json<ProjectNotificationPolicy>,
) -> Result<Json<ProjectNotificationPolicy>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let _ = state;
    if !enforce_workspace_root(&policy.workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if policy.max_items_per_session == 0 || policy.max_items_per_session > 10_000 {
        return Err(StatusCode::BAD_REQUEST);
    }
    ensure_attention_dir(&policy.workspace_root).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    write_json(&policy_path(&policy.workspace_root), &policy)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(policy))
}

async fn get_quiet_hours_handler(
    State(state): State<AttentionRouterState>,
    Query(query): Query<WorkspaceQuery>,
) -> Result<Json<QuietHoursResponse>, StatusCode> {
    let workspace_root = query
        .workspace_root
        .clone()
        .unwrap_or_else(|| state_default_workspace_root(&state));
    if !enforce_workspace_root(&workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(Json(load_quiet_hours(&workspace_root)))
}

async fn set_quiet_hours_handler(
    State(state): State<AttentionRouterState>,
    headers: HeaderMap,
    Json(req): Json<QuietHoursRequest>,
) -> Result<Json<QuietHoursResponse>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let _ = state;
    if !enforce_workspace_root(&req.workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if req
        .windows
        .iter()
        .any(|window| !valid_hhmm(&window.start) || !valid_hhmm(&window.end))
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    ensure_attention_dir(&req.workspace_root).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let response = QuietHoursResponse {
        workspace_root: req.workspace_root.clone(),
        enabled: req.enabled,
        windows: req.windows,
    };
    write_json(&quiet_hours_path(&req.workspace_root), &response)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(response))
}

async fn resolve_deeplink_handler(
    State(state): State<AttentionRouterState>,
    headers: HeaderMap,
    Json(req): Json<DeeplinkRequest>,
) -> Result<Json<DeeplinkResolution>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let _ = state;
    if !enforce_workspace_root(&req.workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let target = req.target.to_ascii_lowercase();
    let allowed_targets = [
        "approval", "failure", "result", "session", "schedule", "pr", "check", "task",
    ];
    let resolved = allowed_targets.iter().any(|t| **t == target);
    let path = if resolved {
        format!("/attention/{target}")
    } else {
        "/attention".to_string()
    };
    Ok(Json(DeeplinkResolution {
        resolved,
        target: req.target,
        path,
        params: req.params,
        note: if resolved {
            None
        } else {
            Some(format!(
                "target '{}' is not a recognized attention deep-link; falling back to the attention index",
                target
            ))
        },
    }))
}

async fn get_attention_banner_handler(
    State(state): State<AttentionRouterState>,
    Query(query): Query<WorkspaceQuery>,
) -> Result<Json<Option<AttentionBanner>>, StatusCode> {
    let workspace_root = query
        .workspace_root
        .clone()
        .unwrap_or_else(|| state_default_workspace_root(&state));
    if !enforce_workspace_root(&workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let density = load_density(&workspace_root);
    let items = build_attention_items(&workspace_root, density);
    let banner = items
        .into_iter()
        .find(|item| {
            matches!(
                item.state,
                AttentionState::NeedsInput | AttentionState::Failed
            )
        })
        .map(|item| AttentionBanner {
            banner_id: format!("banner-{}", item.id),
            state: item.state,
            title: item.title,
            body: item.body,
            deeplink: item.deeplink,
            workspace_root,
            generated_at: now_iso(),
        });
    Ok(Json(banner))
}

