pub(crate) fn tool_invocation_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();

    schemas.insert(
        "ToolInvocationRecord".to_string(),
        json!({
            "type": "object",
            "required": [
                "id",
                "sessionId",
                "turnId",
                "toolName",
                "schemaVersion",
                "argsHash",
                "argsRedactedJson",
                "lifecycleState",
                "artifactIds"
            ],
            "properties": {
                "id": {"type": "string"},
                "sessionId": {"type": "string"},
                "turnId": {"type": "string"},
                "toolName": {"type": "string"},
                "schemaVersion": {"type": "string"},
                "argsHash": {"type": "string"},
                "argsRedactedJson": {"type": "object"},
                "lifecycleState": {"type": "string"},
                "policyVerdictId": {"type": "string"},
                "approvalId": {"type": "string"},
                "processId": {"type": "string"},
                "startedAt": {"type": "string", "format": "date-time"},
                "completedAt": {"type": "string", "format": "date-time"},
                "outputHash": {"type": "string"},
                "artifactIds": {
                    "type": "array",
                    "items": {"type": "string"}
                },
                "errorClass": {"type": "string"},
            },
            "additionalProperties": false
        }),
    );

    schemas.insert(
        "ToolInvocationEventRow".to_string(),
        json!({
            "type": "object",
            "required": [
                "invocationId",
                "sessionId",
                "seq",
                "kind",
                "createdAt",
                "payloadRedactedJson"
            ],
            "properties": {
                "invocationId": {"type": "string"},
                "sessionId": {"type": "string"},
                "seq": {"type": "integer"},
                "kind": {"type": "string"},
                "createdAt": {"type": "string", "format": "date-time"},
                "payloadRedactedJson": {"type": "object"},
            },
            "additionalProperties": false
        }),
    );

    schemas.insert(
        "CancelToolInvocationRequest".to_string(),
        json!({
            "type": "object",
            "properties": {
                "reason": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );

    schemas.insert(
        "CancelToolInvocationResponse".to_string(),
        json!({
            "type": "object",
            "required": ["cancelled", "toolInvocationId", "sessionId"],
            "properties": {
                "cancelled": {"type": "boolean"},
                "toolInvocationId": {"type": "string"},
                "sessionId": {"type": "string"},
                "reason": {"type": ["string", "null"]},
            },
            "additionalProperties": false
        }),
    );

    schemas
}

pub(crate) fn tool_invocations_list_response_schema() -> Value {
    json!({
        "type": "array",
        "items": {"$ref": "#/components/schemas/ToolInvocationRecord"}
    })
}

pub(crate) fn tool_invocation_response_schema() -> Value {
    json!({ "$ref": "#/components/schemas/ToolInvocationRecord" })
}

pub(crate) fn cancel_tool_invocation_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/CancelToolInvocationRequest"})
}

pub(crate) fn cancel_tool_invocation_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/CancelToolInvocationResponse"})
}

pub(crate) fn tool_invocation_events_response_schema() -> Value {
    json!({
        "type": "array",
        "items": {"$ref": "#/components/schemas/ToolInvocationEventRow"}
    })
}
