use axum::{
    extract::State,
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use rightkit_agent_coding_tools::registry::ToolRegistry;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashMap;

use crate::api::DaemonState;

pub(crate) fn tool_schema_routes() -> Router<DaemonState> {
    Router::new()
        .route("/tool-schemas", get(list_tool_schemas_handler))
        .route("/tool-schemas/validate", post(validate_tool_schema_handler))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolSchemaRow {
    name: String,
    description: String,
    category: String,
    read_only: bool,
    deferred_by_default: bool,
    schema_version: String,
    input_schema: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ValidateToolSchemaRequest {
    tool_name: String,
    #[serde(default)]
    arguments: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ValidateToolSchemaResponse {
    tool_name: String,
    schema_version: String,
    valid: bool,
    errors: Vec<String>,
}

async fn list_tool_schemas_handler(State(state): State<DaemonState>) -> Json<Vec<ToolSchemaRow>> {
    Json(tool_schema_rows(&state))
}

async fn validate_tool_schema_handler(
    State(state): State<DaemonState>,
    Json(body): Json<ValidateToolSchemaRequest>,
) -> Result<Json<ValidateToolSchemaResponse>, StatusCode> {
    let registry = ToolRegistry::build_default_with_plugins(state.plugin_dir);
    let Some(tool) = registry.get(&body.tool_name) else {
        return Err(StatusCode::NOT_FOUND);
    };
    let schema = tool.input_schema();
    let mut errors = Vec::new();
    validate_json_schema_value(&schema, &body.arguments, "$", &mut errors);
    Ok(Json(ValidateToolSchemaResponse {
        tool_name: body.tool_name,
        schema_version: schema_version(tool.name(), &schema),
        valid: errors.is_empty(),
        errors,
    }))
}

fn tool_schema_rows(state: &DaemonState) -> Vec<ToolSchemaRow> {
    let registry = ToolRegistry::build_default_with_plugins(state.plugin_dir.clone());
    let categories: HashMap<_, _> = registry
        .capabilities()
        .into_iter()
        .map(|cap| (cap.name, cap.category))
        .collect();
    let mut rows = registry
        .all()
        .iter()
        .map(|tool| {
            let input_schema = tool.input_schema();
            ToolSchemaRow {
                name: tool.name().to_string(),
                description: tool.description().to_string(),
                category: categories
                    .get(tool.name())
                    .cloned()
                    .unwrap_or_else(|| "Other".to_string()),
                read_only: tool.is_read_only(),
                deferred_by_default: tool.defer_schema_by_default(),
                schema_version: schema_version(tool.name(), &input_schema),
                input_schema,
            }
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| left.name.cmp(&right.name));
    rows
}

pub(crate) fn schema_version(tool_name: &str, schema: &Value) -> String {
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    hasher.update(tool_name.as_bytes());
    hasher.update(b"\0");
    hasher.update(serde_json::to_string(schema).unwrap_or_default().as_bytes());
    // The first 8 bytes of the digest: 16 lowercase hex characters.
    let digest = hasher.finalize();
    format!("sha256:{}", &digest[..16])
}

pub(crate) fn tool_schemas_openapi_components() -> Map<String, Value> {
    let mut components = Map::new();

    components.insert(
        "ToolSchemaRow".to_string(),
        json!({
            "type": "object",
            "required": [
                "name",
                "description",
                "category",
                "readOnly",
                "deferredByDefault",
                "schemaVersion",
                "inputSchema"
            ],
            "properties": {
                "name": {"type": "string"},
                "description": {"type": "string"},
                "category": {"type": "string"},
                "readOnly": {"type": "boolean"},
                "deferredByDefault": {"type": "boolean"},
                "schemaVersion": {"type": "string"},
                "inputSchema": {"type": "object"}
            },
            "additionalProperties": false
        }),
    );

    components.insert(
        "ValidateToolSchemaRequest".to_string(),
        json!({
            "type": "object",
            "required": ["toolName"],
            "properties": {
                "toolName": {"type": "string"},
                "arguments": {"type": "object"}
            },
            "additionalProperties": false
        }),
    );

    components.insert(
        "ValidateToolSchemaResponse".to_string(),
        json!({
            "type": "object",
            "required": ["toolName", "schemaVersion", "valid", "errors"],
            "properties": {
                "toolName": {"type": "string"},
                "schemaVersion": {"type": "string"},
                "valid": {"type": "boolean"},
                "errors": {
                    "type": "array",
                    "items": {"type": "string"}
                }
            },
            "additionalProperties": false
        }),
    );

    components
}

pub(crate) fn tool_schemas_list_response_schema() -> Value {
    json!({
        "type": "array",
        "items": {"$ref": "#/components/schemas/ToolSchemaRow"}
    })
}

pub(crate) fn validate_tool_schema_request_schema() -> Value {
    json!({ "$ref": "#/components/schemas/ValidateToolSchemaRequest" })
}

pub(crate) fn validate_tool_schema_response_schema() -> Value {
    json!({ "$ref": "#/components/schemas/ValidateToolSchemaResponse" })
}

fn validate_json_schema_value(schema: &Value, value: &Value, path: &str, errors: &mut Vec<String>) {
    if let Some(enum_values) = schema.get("enum").and_then(Value::as_array) {
        if !enum_values.iter().any(|allowed| allowed == value) {
            errors.push(format!(
                "{path}: value is not one of the allowed enum values"
            ));
            return;
        }
    }

    let expected_type = schema.get("type").and_then(Value::as_str);
    match expected_type {
        Some("object") => validate_object(schema, value, path, errors),
        Some("array") => validate_array(schema, value, path, errors),
        Some("string") => require_type(value.is_string(), path, "string", errors),
        Some("boolean") => require_type(value.is_boolean(), path, "boolean", errors),
        Some("integer") => require_type(is_json_integer(value), path, "integer", errors),
        Some("number") => require_type(value.is_number(), path, "number", errors),
        Some(other) => errors.push(format!("{path}: unsupported schema type `{other}`")),
        None if schema.get("properties").is_some() || schema.get("required").is_some() => {
            validate_object(schema, value, path, errors)
        }
        None => {}
    }
}

fn validate_object(schema: &Value, value: &Value, path: &str, errors: &mut Vec<String>) {
    let Some(object) = value.as_object() else {
        errors.push(format!("{path}: expected object"));
        return;
    };

    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for key in required.iter().filter_map(Value::as_str) {
            if !object.contains_key(key) {
                errors.push(format!("{path}: missing required property `{key}`"));
            }
        }
    }

    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (key, property_schema) in properties {
            if let Some(property_value) = object.get(key) {
                validate_json_schema_value(
                    property_schema,
                    property_value,
                    &format!("{path}.{key}"),
                    errors,
                );
            }
        }
    }
}

fn validate_array(schema: &Value, value: &Value, path: &str, errors: &mut Vec<String>) {
    let Some(items) = value.as_array() else {
        errors.push(format!("{path}: expected array"));
        return;
    };
    if let Some(item_schema) = schema.get("items") {
        for (idx, item) in items.iter().enumerate() {
            validate_json_schema_value(item_schema, item, &format!("{path}[{idx}]"), errors);
        }
    }
}

fn require_type(ok: bool, path: &str, expected: &str, errors: &mut Vec<String>) {
    if !ok {
        errors.push(format!("{path}: expected {expected}"));
    }
}

fn is_json_integer(value: &Value) -> bool {
    value.as_i64().is_some() || value.as_u64().is_some()
}
