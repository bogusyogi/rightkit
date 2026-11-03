pub(crate) fn execution_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    schemas.insert(
        "ExecutionBackendRegistry".to_string(),
        json!({
            "type": "object",
            "required": ["defaultBackend", "backends"],
            "properties": {
                "defaultBackend": {"type": "string"},
                "backends": {"type": "array", "items": {"type": "object"}},
                "networkProfiles": {"type": "array", "items": {"type": "object"}}
            }
        }),
    );
    schemas.insert(
        "SetExecutionBackendDefaultRequest".to_string(),
        json!({
            "type": "object",
            "required": ["backendId"],
            "properties": {"backendId": {"type": "string"}},
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "UpdateExecutionBackendRequest".to_string(),
        json!({
            "type": "object",
            "properties": {
                "enabled": {"type": "boolean"},
                "resourceLimits": {"type": "object"},
                "networkProfile": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SaveExecutionNetworkProfileRequest".to_string(),
        json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "default": {"type": "string", "enum": ["allow", "deny"]},
                "allowHosts": {"type": "array", "items": {"type": "string"}},
                "allowPrivateNetworks": {"type": "boolean"},
                "denyPrivateNetworks": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ExecutionWorkspacesResponse".to_string(),
        json!({
            "type": "object",
            "required": ["workspaceRoot", "storePath", "workspaces"],
            "properties": {
                "workspaceRoot": {"type": "string"},
                "storePath": {"type": "string"},
                "workspaces": {"type": "array", "items": {"$ref": "#/components/schemas/ExecutionWorkspaceRecord"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CreateExecutionWorkspaceRequest".to_string(),
        json!({
            "type": "object",
            "required": ["backendId"],
            "properties": {
                "backendId": {"type": "string"},
                "workspaceId": {"type": "string"},
                "name": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CreateExecutionWorkspaceResponse".to_string(),
        json!({
            "type": "object",
            "required": ["workspace"],
            "properties": {"workspace": {"$ref": "#/components/schemas/ExecutionWorkspaceRecord"}},
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ExecutionWorkspaceRecord".to_string(),
        json!({
            "type": "object",
            "required": ["id", "backendId", "name", "status", "paths", "createdAt", "updatedAt"],
            "properties": {
                "id": {"type": "string"},
                "backendId": {"type": "string"},
                "name": {"type": "string"},
                "status": {"type": "string"},
                "paths": {"$ref": "#/components/schemas/ExecutionWorkspacePaths"},
                "createdAt": {"type": "string"},
                "updatedAt": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ExecutionWorkspacePaths".to_string(),
        json!({
            "type": "object",
            "required": ["root", "uploads", "workspace", "outputs", "logs", "skills", "metadata"],
            "properties": {
                "root": {"type": "string"},
                "uploads": {"type": "string"},
                "workspace": {"type": "string"},
                "outputs": {"type": "string"},
                "logs": {"type": "string"},
                "skills": {"type": "string"},
                "metadata": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ExecutionProcessesResponse".to_string(),
        json!({
            "type": "object",
            "required": ["processes"],
            "properties": {
                "storePath": {"type": ["string", "null"]},
                "processes": {"type": "array", "items": {"$ref": "#/components/schemas/ExecutionProcessRow"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CreateExecutionProcessRequest".to_string(),
        json!({
            "type": "object",
            "required": ["sessionId"],
            "properties": {
                "sessionId": {"type": "string"},
                "resourceProfile": {"type": "string", "enum": ["strict"], "description": "Optional strict launch restriction; host admission remains authoritative."},
                "executionChannel": {"type": "string", "enum": ["pipe", "pty"], "default": "pipe"},
                "ptySize": {"type": "object", "properties": {"rows": {"type": "integer", "minimum": 1}, "cols": {"type": "integer", "minimum": 1}}, "required": ["rows", "cols"]},
                "persistence": {"type": "string", "enum": ["inline", "service"], "default": "inline"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ExecutionProcessResponse".to_string(),
        json!({
            "type": "object",
            "required": ["process"],
            "properties": {"process": {"$ref": "#/components/schemas/ExecutionProcessRow"}},
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ExecutionProcessLogsResponse".to_string(),
        json!({
            "type": "object",
            "required": ["process", "logs"],
            "properties": {
                "process": {"$ref": "#/components/schemas/ExecutionProcessRow"},
                "logs": {"$ref": "#/components/schemas/ExecutionProcessLogs"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ExecutionProcessLogs".to_string(),
        json!({
            "type": "object",
            "required": ["processId", "output", "byteLength", "tailBytes", "truncated"],
            "properties": {
                "processId": {"type": "string"},
                "output": {"type": "string"},
                "byteLength": {"type": "integer"},
                "tailBytes": {"type": "integer"},
                "truncated": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ExecutionProcessRow".to_string(),
        json!({
            "type": "object",
            "required": ["id", "kind", "sessionId", "workspaceRoot", "backendId", "command", "executionChannel", "persistence", "attachment", "leaseEpoch", "nextInputSequence", "lastAckedInputSequence", "status", "statusLabel", "live", "controlMode"],
            "properties": {
                "id": {"type": "string"},
                "kind": {"type": "string"},
                "sessionId": {"type": "string"},
                "workspaceRoot": {"type": "string"},
                "backendId": {"type": "string"},
                "executionWorkspaceId": {"type": ["string", "null"]},
                "networkProfile": {"type": ["string", "null"]},
                "command": {"type": "string"},
                "executionChannel": {"type": "string", "enum": ["pipe", "pty"]},
                "ptySize": {"type": "object", "properties": {"rows": {"type": "integer"}, "cols": {"type": "integer"}, "pixelWidth": {"type": "integer"}, "pixelHeight": {"type": "integer"}}},
                "persistence": {"type": "string", "enum": ["inline", "service"]},
                "attachment": {"type": "string", "enum": ["attached", "detached"]},
                "leaseEpoch": {"type": "integer"},
                "nextInputSequence": {"type": "integer"},
                "lastAckedInputSequence": {"type": "integer"},
                "status": {"type": "string"},
                "statusLabel": {"type": "string"},
                "live": {"type": "boolean"},
                "controlMode": {"type": "string"},
                "controlUnavailableReason": {"type": ["string", "null"]},
                "lastSeenAt": {"type": "string"},
                "output": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "WriteExecutionProcessStdinRequest".to_string(),
        json!({
            "type": "object",
            "required": ["leaseEpoch", "inputSequence"],
            "anyOf": [{"required": ["input"]}, {"required": ["inputBase64"]}, {"required": ["eof"], "properties": {"eof": {"const": true}}}],
            "properties": {
                "input": {"type": "string"},
                "inputBase64": {"type": "string"},
                "eof": {"type": "boolean", "default": false},
                "leaseEpoch": {"type": "integer", "minimum": 1},
                "inputSequence": {"type": "integer", "minimum": 0},
                "appendNewline": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "AcknowledgeExecutionProcessRequest".to_string(),
        json!({
            "type": "object",
            "required": ["leaseEpoch", "sequence"],
            "properties": {
                "leaseEpoch": {"type": "integer", "minimum": 1},
                "sequence": {"type": "integer", "minimum": 0}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ExecutionActionRequest".to_string(),
        json!({
            "type": "object",
            "additionalProperties": false
        }),
    );
    schemas.insert("BackendDoctorReport".to_string(), json!({"type": "object"}));
    schemas
}

pub(crate) fn execution_backend_registry_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ExecutionBackendRegistry"})
}

pub(crate) fn set_execution_backend_default_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/SetExecutionBackendDefaultRequest"})
}

pub(crate) fn update_execution_backend_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/UpdateExecutionBackendRequest"})
}

pub(crate) fn save_execution_network_profile_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/SaveExecutionNetworkProfileRequest"})
}

pub(crate) fn execution_workspaces_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ExecutionWorkspacesResponse"})
}

pub(crate) fn create_execution_workspace_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/CreateExecutionWorkspaceRequest"})
}

pub(crate) fn create_execution_workspace_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/CreateExecutionWorkspaceResponse"})
}

pub(crate) fn execution_processes_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ExecutionProcessesResponse"})
}

pub(crate) fn create_execution_process_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/CreateExecutionProcessRequest"})
}

pub(crate) fn execution_process_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ExecutionProcessResponse"})
}

pub(crate) fn execution_process_logs_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ExecutionProcessLogsResponse"})
}

pub(crate) fn write_execution_process_stdin_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/WriteExecutionProcessStdinRequest"})
}

pub(crate) fn resize_execution_process_request_schema() -> Value {
    json!({"type": "object", "required": ["rows", "cols"], "properties": {
        "rows": {"type": "integer", "minimum": 1, "maximum": 32767},
        "cols": {"type": "integer", "minimum": 1, "maximum": 32767},
        "pixelWidth": {"type": "integer", "minimum": 0},
        "pixelHeight": {"type": "integer", "minimum": 0}
    }, "additionalProperties": false})
}

pub(crate) fn execution_action_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/ExecutionActionRequest"})
}

pub(crate) fn backend_doctor_report_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/BackendDoctorReport"})
}
