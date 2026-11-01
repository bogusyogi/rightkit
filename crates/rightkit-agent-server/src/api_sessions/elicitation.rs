use crate::api::{lock_recover, DaemonState};
use crate::api_permissions::enforce_trusted_source_authority;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;

fn matches_schema(value: &serde_json::Value, schema: &serde_json::Value) -> bool {
    if let Some(options) = schema.get("enum").and_then(serde_json::Value::as_array) {
        if !options.contains(value) {
            return false;
        }
    }
    let valid_type = match schema.get("type").and_then(serde_json::Value::as_str) {
        Some("object") => value.is_object(),
        Some("array") => value.is_array(),
        Some("string") => value.is_string(),
        Some("number") => value.is_number(),
        Some("integer") => value.as_i64().is_some() || value.as_u64().is_some(),
        Some("boolean") => value.is_boolean(),
        Some("null") => value.is_null(),
        Some(_) => false,
        None => true,
    };
    if !valid_type {
        return false;
    }
    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(serde_json::Value::as_array) {
            if required
                .iter()
                .filter_map(serde_json::Value::as_str)
                .any(|key| !object.contains_key(key))
            {
                return false;
            }
        }
        if let Some(properties) = schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
        {
            for (key, child) in properties {
                if let Some(actual) = object.get(key) {
                    if !matches_schema(actual, child) {
                        return false;
                    }
                }
            }
        }
        if schema.get("additionalProperties") == Some(&serde_json::Value::Bool(false)) {
            if let Some(properties) = schema
                .get("properties")
                .and_then(serde_json::Value::as_object)
            {
                if object.keys().any(|key| !properties.contains_key(key)) {
                    return false;
                }
            }
        }
    }
    if let (Some(items), Some(array)) = (schema.get("items"), value.as_array()) {
        if array.iter().any(|item| !matches_schema(item, items)) {
            return false;
        }
    }
    true
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveElicitation {
    pub request_id: String,
    pub action: rightkit_agent_engine_protocol::ElicitationAction,
    #[serde(default)]
    pub content: Option<serde_json::Value>,
}

pub(crate) async fn resolve_elicitation(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<ResolveElicitation>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let live = state.get_session(&id).ok_or(StatusCode::NOT_FOUND)?;
    let schema = lock_recover(&live.pending_elicitations, "session.pending_elicitations")
        .get(&body.request_id)
        .ok_or(StatusCode::NOT_FOUND)?
        .requested_schema
        .clone();
    if body.action == rightkit_agent_engine_protocol::ElicitationAction::Accept {
        if let Some(schema) = schema.as_ref() {
            let content = body.content.as_ref().ok_or(StatusCode::BAD_REQUEST)?;
            if !matches_schema(content, schema) {
                return Err(StatusCode::BAD_REQUEST);
            }
        }
    }
    let pending = lock_recover(&live.pending_elicitations, "session.pending_elicitations")
        .remove(&body.request_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let mut response = serde_json::json!({ "action": body.action });
    if body.action == rightkit_agent_engine_protocol::ElicitationAction::Accept {
        if let Some(content) = body.content {
            response["content"] = content;
        }
    }
    pending
        .response
        .send(response)
        .map_err(|_| StatusCode::NOT_FOUND)?;
    Ok(Json(serde_json::json!({ "resolved": true })))
}
