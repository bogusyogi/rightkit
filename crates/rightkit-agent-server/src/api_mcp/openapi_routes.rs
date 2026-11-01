pub(crate) fn mcp_routes() -> Router<DaemonState> {
    Router::new()
        .route("/mcp", get(list_mcp_handler))
        .route("/mcp/server/status", get(mcp_server_status_handler))
        .route("/mcp/servers", post(save_mcp_server_handler))
        .route("/mcp/servers/{name}", delete(delete_mcp_server_handler))
        .route("/mcp/servers/{name}/probe", post(probe_mcp_server_handler))
        .route("/mcp/servers/{name}/login", post(login_mcp_server_handler))
}

pub(crate) fn mcp_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    schemas.insert(
        "McpServerConfig".to_string(),
        json!({
            "oneOf": [
                {
                    "type": "object",
                    "required": ["type", "url"],
                    "properties": {
                        "type": {"type": "string", "enum": ["http"]},
                        "url": {"type": "string"}
                    },
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "required": ["type", "url"],
                    "properties": {
                        "type": {"type": "string", "enum": ["sse"]},
                        "url": {"type": "string"}
                    },
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "required": ["type", "command"],
                    "properties": {
                        "type": {"type": "string", "enum": ["stdio"]},
                        "command": {"type": "string"},
                        "args": {"type": "array", "items": {"type": "string"}},
                        "env": {"type": "object", "additionalProperties": {"type": "string"}}
                    },
                    "additionalProperties": false
                }
            ]
        }),
    );
    schemas.insert(
        "McpTrustPolicy".to_string(),
        json!({
            "type": "object",
            "required": ["preset"],
            "properties": {
                "preset": {"type": "string"},
                "approvedBy": {"type": ["string", "null"]},
                "approvedAt": {"type": ["string", "null"]},
                "inferred": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "McpChannelPermissions".to_string(),
        json!({
            "type": "object",
            "required": ["allowedChannels", "allowMutating"],
            "properties": {
                "allowedChannels": {"type": "array", "items": {"type": "string"}},
                "allowMutating": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "McpPolicyDiagnostic".to_string(),
        json!({
            "type": "object",
            "required": ["severity", "code", "message"],
            "properties": {
                "severity": {"type": "string", "enum": ["info", "warning", "error"]},
                "code": {"type": "string"},
                "message": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "McpTrustPreset".to_string(),
        json!({
            "type": "object",
            "required": ["id", "label", "description"],
            "properties": {
                "id": {"type": "string"},
                "label": {"type": "string"},
                "description": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "McpServerRow".to_string(),
        json!({
            "type": "object",
            "required": ["scope", "name", "type", "config"],
            "properties": {
                "scope": {"type": "string", "enum": ["user", "workspace"]},
                "name": {"type": "string"},
                "type": {"type": "string", "enum": ["http", "sse", "stdio"]},
                "config": {"$ref": "#/components/schemas/McpServerConfig"},
                "trust": {"$ref": "#/components/schemas/McpTrustPolicy"},
                "channelPermissions": {
                    "oneOf": [
                        {"$ref": "#/components/schemas/McpChannelPermissions"},
                        {"type": "null"}
                    ]
                },
                "policyDiagnostics": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/McpPolicyDiagnostic"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "McpListResponse".to_string(),
        json!({
            "type": "object",
            "required": ["userConfigPath", "servers"],
            "properties": {
                "userConfigPath": {"type": "string"},
                "workspaceConfigPath": {"type": ["string", "null"]},
                "trustPresets": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/McpTrustPreset"}
                },
                "servers": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/McpServerRow"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "McpServerModeStatus".to_string(),
        json!({
            "type": "object",
            "required": [
                "command",
                "readOnlyCommand",
                "mutatingCommand",
                "defaultMode",
                "mutatingRequiresOptIn",
                "readOnlyToolCount",
                "mutatingToolCount",
                "totalToolCount",
                "readOnlyTools",
                "mutatingTools"
            ],
            "properties": {
                "command": {"type": "string"},
                "readOnlyCommand": {"type": "string"},
                "mutatingCommand": {"type": "string"},
                "defaultMode": {"type": "string"},
                "mutatingRequiresOptIn": {"type": "boolean"},
                "readOnlyToolCount": {"type": "integer"},
                "mutatingToolCount": {"type": "integer"},
                "totalToolCount": {"type": "integer"},
                "readOnlyTools": {"type": "array", "items": {"type": "string"}},
                "mutatingTools": {"type": "array", "items": {"type": "string"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SaveMcpServerRequest".to_string(),
        json!({
            "type": "object",
            "required": ["scope", "name", "config"],
            "properties": {
                "scope": {"type": "string", "enum": ["user", "workspace"]},
                "workspaceRoot": {"type": "string"},
                "name": {"type": "string"},
                "config": {"$ref": "#/components/schemas/McpServerConfig"},
                "trustPreset": {"type": "string"},
                "approvedBy": {"type": "string"},
                "channelPermissions": {"$ref": "#/components/schemas/McpChannelPermissions"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SaveMcpServerResponse".to_string(),
        json!({
            "type": "object",
            "required": ["saved", "scope", "name", "path"],
            "properties": {
                "saved": {"type": "boolean"},
                "scope": {"type": "string"},
                "name": {"type": "string"},
                "path": {"type": "string"},
                "trust": {"$ref": "#/components/schemas/McpTrustPolicy"},
                "channelPermissions": {
                    "oneOf": [
                        {"$ref": "#/components/schemas/McpChannelPermissions"},
                        {"type": "null"}
                    ]
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "DeleteMcpServerResponse".to_string(),
        json!({
            "type": "object",
            "required": ["removed", "scope", "name", "path"],
            "properties": {
                "removed": {"type": "boolean"},
                "scope": {"type": "string"},
                "name": {"type": "string"},
                "path": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "McpToolProbeRow".to_string(),
        json!({
            "type": "object",
            "required": ["name", "inputSchema"],
            "properties": {
                "name": {"type": "string"},
                "description": {"type": "string"},
                "inputSchema": {"type": "object", "additionalProperties": true}
            },
            "additionalProperties": true
        }),
    );
    schemas.insert(
        "McpProbeResult".to_string(),
        json!({
            "type": "object",
            "required": ["ok", "scope", "name", "type", "toolCount", "tools", "latencyMs"],
            "properties": {
                "ok": {"type": "boolean"},
                "scope": {"type": "string"},
                "name": {"type": "string"},
                "type": {"type": "string", "enum": ["http", "sse", "stdio"]},
                "toolCount": {"type": "integer"},
                "tools": {"type": "array", "items": {"$ref": "#/components/schemas/McpToolProbeRow"}},
                "latencyMs": {"type": "integer"},
                "error": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "McpLoginResult".to_string(),
        json!({
            "type": "object",
            "required": ["ok"],
            "properties": {
                "ok": {"type": "boolean"},
                "message": {"type": "string"},
                "authorizationUrl": {"type": "string"},
                "error": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas
}

pub(crate) fn mcp_openapi_request_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("post", "/mcp/servers") => {
            Some(json!({"$ref": "#/components/schemas/SaveMcpServerRequest"}))
        }
        _ => None,
    }
}

pub(crate) fn mcp_openapi_response_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("get", "/mcp") => Some(json!({"$ref": "#/components/schemas/McpListResponse"})),
        ("get", "/mcp/server/status") => {
            Some(json!({"$ref": "#/components/schemas/McpServerModeStatus"}))
        }
        ("post", "/mcp/servers") => {
            Some(json!({"$ref": "#/components/schemas/SaveMcpServerResponse"}))
        }
        ("delete", "/mcp/servers/{name}") => {
            Some(json!({"$ref": "#/components/schemas/DeleteMcpServerResponse"}))
        }
        ("post", "/mcp/servers/{name}/probe") => {
            Some(json!({"$ref": "#/components/schemas/McpProbeResult"}))
        }
        ("post", "/mcp/servers/{name}/login") => {
            Some(json!({"$ref": "#/components/schemas/McpLoginResult"}))
        }
        _ => None,
    }
}
