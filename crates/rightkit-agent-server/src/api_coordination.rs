use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{delete, get},
    Router,
};
use rightkit_agent_host_traits::skill_host::{CommandAction, CommandResult};
use serde_json::{json, Map, Value};

use crate::api::{lock_recover, DaemonState};
use crate::api_permissions::enforce_trusted_source_authority;

pub fn coordination_routes() -> Router<DaemonState> {
    Router::new()
        .route("/messages", get(list_messages_handler))
        .route("/teams", get(list_teams_handler))
        .route("/teams/{name}", delete(delete_team_handler))
}

pub(crate) fn coordination_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    schemas.insert(
        "TeamDefinition".to_string(),
        json!({
            "type": "object",
            "required": ["name", "members"],
            "properties": {
                "name": {"type": "string"},
                "description": {"type": ["string", "null"]},
                "members": {"type": "array", "items": {"type": "string"}},
                "maxWorkers": {"type": ["integer", "null"], "minimum": 0},
                "budgetUsd": {"type": ["number", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "TeamListResponse".to_string(),
        json!({
            "type": "object",
            "required": ["teamStorePath", "teams"],
            "properties": {
                "teamStorePath": {"type": "string"},
                "teams": {"type": "array", "items": {"$ref": "#/components/schemas/TeamDefinition"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "MessageHistoryRow".to_string(),
        json!({
            "type": "object",
            "required": [
                "id",
                "createdAt",
                "fromSessionId",
                "channel",
                "message",
                "deliveryStatus",
                "deliveredSessionIds"
            ],
            "properties": {
                "id": {"type": "string"},
                "createdAt": {"type": "string"},
                "fromSessionId": {"type": "string"},
                "targetSessionId": {"type": ["string", "null"]},
                "channel": {"type": "string"},
                "message": {"type": "string"},
                "deliveryStatus": {"type": "string"},
                "deliveredSessionIds": {"type": "array", "items": {"type": "string"}},
                "error": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "MessageListResponse".to_string(),
        json!({
            "type": "object",
            "required": ["messageStorePath", "messages"],
            "properties": {
                "messageStorePath": {"type": "string"},
                "messages": {"type": "array", "items": {"$ref": "#/components/schemas/MessageHistoryRow"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "DeleteTeamResponse".to_string(),
        json!({
            "type": "object",
            "required": ["deleted", "summary"],
            "properties": {
                "deleted": {"type": "boolean"},
                "summary": {"type": "object", "additionalProperties": true}
            },
            "additionalProperties": false
        }),
    );
    schemas
}

pub(crate) fn coordination_openapi_response_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("get", "/teams") => Some(json!({"$ref": "#/components/schemas/TeamListResponse"})),
        ("get", "/messages") => Some(json!({"$ref": "#/components/schemas/MessageListResponse"})),
        ("delete", "/teams/{name}") => {
            Some(json!({"$ref": "#/components/schemas/DeleteTeamResponse"}))
        }
        _ => None,
    }
}

async fn list_teams_handler(State(state): State<DaemonState>) -> Json<serde_json::Value> {
    let mut teams: Vec<_> = lock_recover(&state.live_teams, "state.live_teams")
        .values()
        .cloned()
        .collect();
    teams.sort_by(|a, b| a.name.cmp(&b.name));
    Json(serde_json::json!({
        "teamStorePath": state.team_store_path,
        "teams": teams.iter().map(team_json).collect::<Vec<_>>(),
    }))
}

async fn list_messages_handler(State(state): State<DaemonState>) -> Json<serde_json::Value> {
    let mut messages = lock_recover(&state.message_history, "state.message_history").clone();
    messages.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Json(serde_json::json!({
        "messageStorePath": state.message_store_path,
        "messages": messages,
    }))
}

async fn delete_team_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    match rightkit_agent_tools::coding_tools::TeamDirectory::delete_team(&state, name).await {
        Ok(summary) => Ok(Json(serde_json::json!({
            "deleted": true,
            "summary": summary,
        }))),
        Err(e) if e.contains("team not found") => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

fn team_json(team: &rightkit_agent_tools::coding_tools::TeamDefinition) -> serde_json::Value {
    serde_json::json!({
        "name": team.name,
        "description": team.description,
        "members": team.members,
        "maxWorkers": team.max_workers,
        "budgetUsd": team.budget_usd,
    })
}

pub fn render_teams_command(state: &DaemonState) -> CommandResult {
    let mut teams: Vec<_> = lock_recover(&state.live_teams, "state.live_teams")
        .values()
        .cloned()
        .collect();
    teams.sort_by(|a, b| a.name.cmp(&b.name));
    let mut output = format!(
        "**Teams:** {} definition{}\nStore: {}",
        teams.len(),
        if teams.len() == 1 { "" } else { "s" },
        state.team_store_path.display()
    );
    if teams.is_empty() {
        output.push_str("\nNo team definitions.");
    } else {
        output.push('\n');
        output.push_str(
            &teams
                .into_iter()
                .map(|team| {
                    let mut presets = Vec::new();
                    if let Some(max_workers) = team.max_workers {
                        presets.push(format!("maxWorkers={max_workers}"));
                    }
                    if let Some(budget_usd) = team.budget_usd {
                        presets.push(format!("budgetUsd={budget_usd:.2}"));
                    }
                    if presets.is_empty() {
                        format!("- {} ({})", team.name, team.members.join(", "))
                    } else {
                        format!(
                            "- {} ({}) [{}]",
                            team.name,
                            team.members.join(", "),
                            presets.join(", ")
                        )
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    command_ok(output)
}

pub fn render_messages_command(state: &DaemonState) -> CommandResult {
    let mut messages = lock_recover(&state.message_history, "state.message_history").clone();
    messages.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let total = messages.len();
    let mut output = format!(
        "**Messages:** {total} recorded\nStore: {}",
        state.message_store_path.display()
    );
    if messages.is_empty() {
        output.push_str("\nNo inter-session messages recorded.");
    } else {
        output.push('\n');
        output.push_str(
            &messages
                .into_iter()
                .take(10)
                .map(|m| {
                    format!(
                        "- {} -> {} [{}] {}",
                        m.from_session_id,
                        m.target_session_id.unwrap_or_else(|| "broadcast".into()),
                        m.channel,
                        m.message
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    command_ok(output)
}

fn command_ok(output: impl Into<String>) -> CommandResult {
    CommandResult {
        success: true,
        output: output.into(),
        action: CommandAction::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordination_openapi_components_cover_routes() {
        let components = coordination_openapi_components();
        for name in [
            "TeamDefinition",
            "TeamListResponse",
            "MessageHistoryRow",
            "MessageListResponse",
            "DeleteTeamResponse",
        ] {
            assert!(components.contains_key(name), "{name} schema missing");
        }
        assert_eq!(
            coordination_openapi_response_schema("get", "/teams").unwrap()["$ref"],
            "#/components/schemas/TeamListResponse"
        );
        assert_eq!(
            coordination_openapi_response_schema("get", "/messages").unwrap()["$ref"],
            "#/components/schemas/MessageListResponse"
        );
        assert_eq!(
            components["TeamListResponse"]["properties"]["teams"]["items"]["$ref"],
            "#/components/schemas/TeamDefinition"
        );
    }
}
