use axum::{
    extract::{Path, Query},
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::path::{Path as FsPath, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::DaemonState;
use crate::api_permissions::enforce_trusted_source_authority;

pub fn notification_routes() -> Router<DaemonState> {
    Router::new()
        .route("/notifications", get(list_notifications_handler))
        .route(
            "/notifications/{id}/dismiss",
            post(dismiss_notification_handler),
        )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotificationWorkspaceQuery {
    #[serde(default)]
    workspace_root: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationRow {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub read: bool,
}

pub(crate) fn notifications_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    schemas.insert(
        "NotificationRow".to_string(),
        json!({
            "type": "object",
            "required": ["id", "kind", "title", "body", "source", "createdAt", "read"],
            "properties": {
                "id": {"type": "string"},
                "kind": {"type": "string"},
                "title": {"type": "string"},
                "body": {"type": "string"},
                "source": {"type": "string"},
                "scheduleId": {"type": ["string", "null"]},
                "sessionId": {"type": ["string", "null"]},
                "createdAt": {"type": "string"},
                "read": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "NotificationsResponse".to_string(),
        json!({
            "type": "object",
            "required": ["workspaceRoot", "notificationStorePath", "unreadCount", "notifications"],
            "properties": {
                "workspaceRoot": {"type": "string"},
                "notificationStorePath": {"type": "string"},
                "unreadCount": {"type": "integer", "minimum": 0},
                "notifications": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/NotificationRow"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "DismissNotificationRequest".to_string(),
        json!({
            "type": "object",
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "DismissNotificationResponse".to_string(),
        json!({
            "type": "object",
            "required": ["dismissed", "notification"],
            "properties": {
                "dismissed": {"type": "boolean"},
                "notification": {"$ref": "#/components/schemas/NotificationRow"}
            },
            "additionalProperties": false
        }),
    );
    schemas
}

pub(crate) fn notifications_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/NotificationsResponse"})
}

pub(crate) fn dismiss_notification_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/DismissNotificationRequest"})
}

pub(crate) fn dismiss_notification_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/DismissNotificationResponse"})
}

pub fn append_desktop_notification(
    workspace_root: &FsPath,
    source: &str,
    schedule_id: Option<&str>,
    session_id: Option<&str>,
    title: impl Into<String>,
    body: impl Into<String>,
) -> Result<NotificationRow, String> {
    let now_seconds = current_unix_seconds();
    append_desktop_notification_at(
        workspace_root,
        source,
        schedule_id,
        session_id,
        title.into(),
        body.into(),
        now_seconds,
    )
}

/// Live attention-routing gate: a desktop notification is suppressed (not
/// persisted) while the project's quiet hours are active, so the operator is
/// not interrupted. `now_seconds` is injected so the routing decision is
/// deterministic and unit-testable.
fn append_desktop_notification_at(
    workspace_root: &FsPath,
    source: &str,
    schedule_id: Option<&str>,
    session_id: Option<&str>,
    title: String,
    body: String,
    now_seconds: u64,
) -> Result<NotificationRow, String> {
    let row = NotificationRow {
        id: format!("notification-{}", current_epoch_nanos()),
        kind: "desktopNotification".to_string(),
        title: truncate_notification_text(&title, 140),
        body: truncate_notification_text(&body, 4_000),
        source: source.to_string(),
        schedule_id: schedule_id.map(str::to_string),
        session_id: session_id.map(str::to_string),
        created_at: current_unix_timestamp(),
        read: false,
    };
    let root_str = workspace_root.to_string_lossy();
    if rightkit_agent_engine_attention::quiet_hours_active(root_str.as_ref(), now_seconds) {
        // Quiet hours active: route the notification as suppressed.
        return Ok(row);
    }
    let path = notifications_store_path(workspace_root);
    let mut rows = load_notifications(workspace_root)?;
    rows.push(row.clone());
    save_notifications(&path, &rows)?;
    Ok(row)
}

async fn list_notifications_handler(
    Query(query): Query<NotificationWorkspaceQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let root = notification_workspace_root(&query);
    let mut rows = load_notifications(&root).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    rows.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let unread_count = rows.iter().filter(|row| !row.read).count();
    Ok(Json(serde_json::json!({
        "workspaceRoot": root,
        "notificationStorePath": notifications_store_path(&root),
        "unreadCount": unread_count,
        "notifications": rows,
    })))
}

async fn dismiss_notification_handler(
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<NotificationWorkspaceQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let root = notification_workspace_root(&query);
    let path = notifications_store_path(&root);
    let mut rows = load_notifications(&root).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let Some(row) = rows.iter_mut().find(|row| row.id == id) else {
        return Err(StatusCode::NOT_FOUND);
    };
    row.read = true;
    let notification = row.clone();
    save_notifications(&path, &rows).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({
        "dismissed": true,
        "notification": notification,
    })))
}

fn notification_workspace_root(query: &NotificationWorkspaceQuery) -> PathBuf {
    if query.workspace_root.trim().is_empty() {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        PathBuf::from(query.workspace_root.trim())
    }
}

pub fn notifications_store_path(workspace_root: &FsPath) -> PathBuf {
    workspace_root.join(".coderight").join("notifications.json")
}

fn load_notifications(workspace_root: &FsPath) -> Result<Vec<NotificationRow>, String> {
    let path = notifications_store_path(workspace_root);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("failed to read notifications '{}': {e}", path.display()))?;
    serde_json::from_str(&raw)
        .map_err(|e| format!("failed to parse notifications '{}': {e}", path.display()))
}

fn save_notifications(path: &FsPath, rows: &[NotificationRow]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            format!(
                "failed to create notifications dir '{}': {e}",
                parent.display()
            )
        })?;
    }
    let raw = serde_json::to_string_pretty(rows)
        .map_err(|e| format!("failed to encode notifications: {e}"))?;
    crate::store_write::write_store_file(path, raw.as_bytes())
        .map_err(|e| format!("failed to write notifications '{}': {e}", path.display()))
}

fn truncate_notification_text(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    value
        .chars()
        .take(max_chars.saturating_sub(16))
        .collect::<String>()
        + "...[truncated]"
}

fn current_epoch_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default()
}

fn current_unix_timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    format!("unix:{secs}")
}

fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notifications_openapi_components_cover_notification_routes() {
        let components = notifications_openapi_components();
        for name in [
            "NotificationRow",
            "NotificationsResponse",
            "DismissNotificationRequest",
            "DismissNotificationResponse",
        ] {
            assert!(components.contains_key(name), "missing {name}");
        }
        assert_eq!(
            notifications_response_schema()["$ref"],
            "#/components/schemas/NotificationsResponse"
        );
        assert_eq!(
            dismiss_notification_request_schema()["$ref"],
            "#/components/schemas/DismissNotificationRequest"
        );
        assert_eq!(
            dismiss_notification_response_schema()["$ref"],
            "#/components/schemas/DismissNotificationResponse"
        );
        assert_eq!(
            components["NotificationsResponse"]["properties"]["notifications"]["items"]["$ref"],
            "#/components/schemas/NotificationRow"
        );
    }

    #[test]
    fn quiet_hours_gate_suppresses_desktop_notification_during_quiet_hours() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".coderight").join("attention")).unwrap();
        let policy = json!({
            "workspaceRoot": root.to_string_lossy(),
            "enabled": true,
            "windows": [{"start": "22:00", "end": "07:00", "timezone": null}]
        });
        std::fs::write(
            root.join(".coderight")
                .join("attention")
                .join("quiet-hours.json"),
            serde_json::to_string_pretty(&policy).unwrap(),
        )
        .unwrap();

        // 23:30 is inside the midnight-crossing quiet window: not persisted.
        let _ = append_desktop_notification_at(
            root,
            "schedule",
            None,
            None,
            "title".to_string(),
            "body".to_string(),
            seconds_for_hhmm(23, 30),
        )
        .unwrap();
        let stored = std::fs::read_to_string(root.join(".coderight").join("notifications.json"))
            .unwrap_or_default();
        assert!(
            !stored.contains("\"title\""),
            "notification must not be persisted during quiet hours"
        );

        // 09:00 is outside the quiet window: persisted.
        let _ = append_desktop_notification_at(
            root,
            "schedule",
            None,
            None,
            "title".to_string(),
            "body".to_string(),
            seconds_for_hhmm(9, 0),
        )
        .unwrap();
        let stored_after =
            std::fs::read_to_string(root.join(".coderight").join("notifications.json"))
                .unwrap_or_default();
        assert!(
            stored_after.contains("\"title\""),
            "notification must be persisted outside quiet hours"
        );
    }

    fn seconds_for_hhmm(hours: u64, minutes: u64) -> u64 {
        hours * 3_600 + minutes * 60
    }
}
