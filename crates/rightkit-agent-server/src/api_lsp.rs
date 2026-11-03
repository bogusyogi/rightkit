use axum::{
    extract::Query,
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::get,
    Router,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::path::Path;

use crate::api::DaemonState;
use crate::api_permissions::enforce_trusted_source_authority;

pub fn lsp_routes() -> Router<DaemonState> {
    Router::new().route("/lsp", get(get_lsp_handler).put(save_lsp_handler))
}

pub(crate) fn lsp_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    schemas.insert(
        "LspWorkspaceServer".to_string(),
        json!({
            "type": "object",
            "required": ["language", "extensions", "serverPath", "serverArgs"],
            "properties": {
                "language": {"type": "string"},
                "extensions": {"type": "array", "items": {"type": "string"}},
                "serverPath": {"type": "string"},
                "serverArgs": {"type": "array", "items": {"type": "string"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "LspServerStatus".to_string(),
        json!({
            "type": "object",
            "required": [
                "language",
                "executable",
                "extensions",
                "serverArgs",
                "source",
                "available"
            ],
            "properties": {
                "language": {"type": "string"},
                "executable": {"type": "string"},
                "extensions": {"type": "array", "items": {"type": "string"}},
                "configuredPath": {"type": ["string", "null"]},
                "discoveredPath": {"type": ["string", "null"]},
                "effectivePath": {"type": ["string", "null"]},
                "serverArgs": {"type": "array", "items": {"type": "string"}},
                "source": {"type": "string"},
                "available": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "LspStatusView".to_string(),
        json!({
            "type": "object",
            "required": ["workspaceRoot", "configPath", "fallbackEnabled", "servers"],
            "properties": {
                "workspaceRoot": {"type": "string"},
                "configPath": {"type": "string"},
                "fallbackEnabled": {"type": "boolean"},
                "servers": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/LspServerStatus"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SaveLspConfigRequest".to_string(),
        json!({
            "type": "object",
            "required": ["workspaceRoot", "servers"],
            "properties": {
                "workspaceRoot": {"type": "string"},
                "servers": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/LspWorkspaceServer"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas
}

pub(crate) fn lsp_openapi_request_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("put", "/lsp") => Some(json!({"$ref": "#/components/schemas/SaveLspConfigRequest"})),
        _ => None,
    }
}

pub(crate) fn lsp_openapi_response_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("get", "/lsp") | ("put", "/lsp") => {
            Some(json!({"$ref": "#/components/schemas/LspStatusView"}))
        }
        _ => None,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LspQuery {
    pub workspace_root: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveLspConfigRequest {
    pub workspace_root: String,
    #[serde(default)]
    pub servers: Vec<rightkit_agent_tools::coding_tools::lsp_tool::LspWorkspaceServer>,
}

async fn get_lsp_handler(
    Query(query): Query<LspQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let workspace = query.workspace_root.trim();
    if workspace.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let status = rightkit_agent_tools::coding_tools::lsp_tool::lsp_status(Path::new(workspace));
    Ok(Json(
        serde_json::to_value(status).unwrap_or(serde_json::Value::Null),
    ))
}

async fn save_lsp_handler(
    headers: HeaderMap,
    Json(body): Json<SaveLspConfigRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let workspace = body.workspace_root.trim();
    if workspace.is_empty()
        || body
            .servers
            .iter()
            .any(|server| server.language.trim().is_empty() || server.server_path.trim().is_empty())
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let config = rightkit_agent_tools::coding_tools::lsp_tool::LspWorkspaceConfig {
        servers: body.servers,
    };
    rightkit_agent_tools::coding_tools::lsp_tool::write_lsp_workspace_config(
        Path::new(workspace),
        &config,
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let status = rightkit_agent_tools::coding_tools::lsp_tool::lsp_status(Path::new(workspace));
    Ok(Json(
        serde_json::to_value(status).unwrap_or(serde_json::Value::Null),
    ))
}
