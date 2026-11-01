pub(crate) fn schedules_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();

    schemas.insert(
        "ScheduleDeliveryTarget".to_string(),
        json!({
            "type": "object",
            "required": ["kind", "allowPrivateNetwork"],
            "properties": {
                "kind": {"type": "string"},
                "target": {"type": "string", "nullable": true},
                "allowPrivateNetwork": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ScheduleRunResourceLimits".to_string(),
        json!({
            "type": "object",
            "properties": {
                "cpu": {"type": "integer", "nullable": true},
                "memoryMb": {"type": "integer", "nullable": true},
                "diskMb": {"type": "integer", "nullable": true}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ScheduleRunEvent".to_string(),
        json!({
            "type": "object",
            "required": ["scheduleId", "epochMinute", "status", "sessionId", "message"],
            "properties": {
                "scheduleId": {"type": "string"},
                "epochMinute": {"type": "integer", "format": "int64"},
                "status": {"type": "string"},
                "sessionId": {"type": "string"},
                "message": {"type": "string"},
                "mode": {"type": "string", "enum": ["agent", "scriptOnly"]},
                "toolset": {"type": "string", "nullable": true},
                "executionBackend": {"type": "string", "nullable": true},
                "resolvedExecutionBackend": {"type": "string", "nullable": true},
                "executionWorkspaceId": {"type": "string", "nullable": true},
                "executionWorkspacePath": {"type": "string", "nullable": true},
                "resourceLimits": {
                    "oneOf": [
                        {"$ref": "#/components/schemas/ScheduleRunResourceLimits"},
                        {"type": "null"}
                    ]
                },
                "workdir": {"type": "string", "nullable": true},
                "skills": {"type": "array", "items": {"type": "string"}},
                "deliveryKinds": {"type": "array", "items": {"type": "string"}},
                "sourceRecipeId": {"type": "string", "nullable": true},
                "exitCode": {"type": "integer", "nullable": true},
                "artifactPath": {"type": "string", "nullable": true},
                "deliveryStatus": {"type": "string", "nullable": true},
                "deliveryErrors": {"type": "array", "items": {"type": "string"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ScheduleEntry".to_string(),
        json!({
            "type": "object",
            "required": [
                "id",
                "cronExpr",
                "prompt",
                "timeZone",
                "skills",
                "delivery",
                "scriptAllowlist",
                "mode",
                "paused",
                "recipeParams"
            ],
            "properties": {
                "id": {"type": "string"},
                "cronExpr": {"type": "string"},
                "prompt": {"type": "string"},
                "timeZone": {"type": "string"},
                "skills": {"type": "array", "items": {"type": "string"}},
                "workdir": {"type": "string", "nullable": true},
                "toolset": {"type": "string", "nullable": true},
                "executionBackend": {"type": "string", "nullable": true},
                "delivery": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/ScheduleDeliveryTarget"}
                },
                "scriptAllowlist": {"type": "array", "items": {"type": "string"}},
                "mode": {"type": "string", "enum": ["agent", "scriptOnly"]},
                "paused": {"type": "boolean"},
                "sourceRecipeId": {"type": "string", "nullable": true},
                "recipeParams": {
                    "type": "object",
                    "additionalProperties": {"type": "string"}
                },
                "maxRuntimeSeconds": {"type": "integer", "nullable": true}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ScheduleRow".to_string(),
        json!({
            "allOf": [
                {"$ref": "#/components/schemas/ScheduleEntry"},
                {
                    "type": "object",
                    "required": ["nextDueError", "latestRun", "runs"],
                    "properties": {
                        "nextDueEpochMinute": {"type": "integer", "format": "int64", "nullable": true},
                        "nextDueError": {"nullable": true},
                        "lastClaimedMinute": {"type": "integer", "format": "int64", "nullable": true},
                        "latestRun": {
                            "oneOf": [
                                {"$ref": "#/components/schemas/ScheduleRunEvent"},
                                {"type": "null"}
                            ]
                        },
                        "runs": {
                            "type": "array",
                            "items": {"$ref": "#/components/schemas/ScheduleRunEvent"}
                        }
                    },
                    "additionalProperties": false
                }
            ]
        }),
    );
    schemas.insert(
        "ListSchedulesResponse".to_string(),
        json!({
            "type": "object",
            "required": ["workspaceRoot", "scheduleStorePath", "runStorePath", "adminAuditStorePath", "schedules"],
            "properties": {
                "workspaceRoot": {"type": "string"},
                "scheduleStorePath": {"type": "string"},
                "runStorePath": {"type": "string"},
                "adminAuditStorePath": {"type": "string"},
                "schedules": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/ScheduleRow"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ScheduleAdminAuditEvent".to_string(),
        json!({
            "type": "object",
            "required": [
                "id",
                "scheduleId",
                "action",
                "actor",
                "source",
                "createdAt",
                "status",
                "details"
            ],
            "properties": {
                "id": {"type": "string"},
                "scheduleId": {"type": "string"},
                "action": {"type": "string"},
                "actor": {"type": "string"},
                "source": {"type": "string"},
                "createdAt": {"type": "string"},
                "status": {"type": "string"},
                "details": {"type": "object", "additionalProperties": true}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ScheduleAdminAuditResponse".to_string(),
        json!({
            "type": "object",
            "required": ["workspaceRoot", "auditStorePath", "history"],
            "properties": {
                "workspaceRoot": {"type": "string"},
                "auditStorePath": {"type": "string"},
                "history": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/ScheduleAdminAuditEvent"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ScheduleRunsResponse".to_string(),
        json!({
            "type": "object",
            "required": ["id", "workspaceRoot", "runStorePath", "latestRun", "runs"],
            "properties": {
                "id": {"type": "string"},
                "workspaceRoot": {"type": "string"},
                "runStorePath": {"type": "string"},
                "latestRun": {
                    "oneOf": [
                        {"$ref": "#/components/schemas/ScheduleRunEvent"},
                        {"type": "null"}
                    ]
                },
                "runs": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/ScheduleRunEvent"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CreateScheduleRequest".to_string(),
        json!({
            "type": "object",
            "required": ["cronExpr", "prompt"],
            "properties": schedule_write_properties(true),
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "PatchScheduleRequest".to_string(),
        json!({
            "type": "object",
            "properties": schedule_write_properties(false),
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "UpdateScheduleTimeZoneRequest".to_string(),
        json!({
            "type": "object",
            "required": ["timeZone"],
            "properties": {
                "timeZone": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ScheduleActionRequest".to_string(),
        json!({
            "type": "object",
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ScheduleMutationResponse".to_string(),
        json!({
            "type": "object",
            "required": ["scheduleStorePath", "schedule"],
            "properties": {
                "created": {"type": "boolean"},
                "updated": {"type": "boolean"},
                "id": {"type": "string"},
                "paused": {"type": "boolean"},
                "scheduleStorePath": {"type": "string"},
                "schedule": {"$ref": "#/components/schemas/ScheduleEntry"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "DeleteScheduleResponse".to_string(),
        json!({
            "type": "object",
            "required": ["deleted", "id", "scheduleStorePath"],
            "properties": {
                "deleted": {"type": "boolean"},
                "id": {"type": "string"},
                "scheduleStorePath": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RunScheduleNowResponse".to_string(),
        json!({
            "type": "object",
            "required": ["submitted", "id", "runStorePath", "latestRun"],
            "properties": {
                "submitted": {"type": "boolean"},
                "id": {"type": "string"},
                "runStorePath": {"type": "string"},
                "latestRun": {
                    "oneOf": [
                        {"$ref": "#/components/schemas/ScheduleRunEvent"},
                        {"type": "null"}
                    ]
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "BotBrowserPolicy".to_string(),
        json!({
            "type": "object",
            "required": ["mode", "profile", "allowNetwork"],
            "properties": {
                "mode": {"type": "string"},
                "profile": {"type": "string", "nullable": true},
                "allowNetwork": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "BotBudget".to_string(),
        json!({
            "type": "object",
            "required": ["maxTokens", "maxSeconds", "maxCostMicros"],
            "properties": {
                "maxTokens": {"type": "integer", "nullable": true},
                "maxSeconds": {"type": "integer", "nullable": true},
                "maxCostMicros": {"type": "integer", "nullable": true}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "BotVerifierBinding".to_string(),
        json!({
            "type": "object",
            "required": ["rolePack", "model", "required"],
            "properties": {
                "rolePack": {"type": "string", "nullable": true},
                "model": {"type": "string", "nullable": true},
                "required": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "BotTriggerBinding".to_string(),
        json!({
            "type": "object",
            "required": ["id", "schedule", "expression", "paused"],
            "properties": {
                "id": {"type": "string"},
                "schedule": {"type": "string", "enum": ["cron", "once", "interval", "monitor"]},
                "expression": {"type": "string", "nullable": true},
                "paused": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "BotProfileV1".to_string(),
        json!({
            "type": "object",
            "required": ["schemaVersion", "id", "name", "rolePack", "model"],
            "properties": {
                "schemaVersion": {"type": "integer"},
                "id": {"type": "string"},
                "name": {"type": "string"},
                "glyph": {"type": "string"},
                "avatar": {"type": "string", "nullable": true},
                "description": {"type": "string"},
                "version": {"type": "integer"},
                "rolePack": {"type": "string"},
                "model": {"type": "string"},
                "fallbackModels": {"type": "array", "items": {"type": "string"}},
                "instructions": {"type": "string"},
                "memoryPolicy": {"type": "string"},
                "project": {"type": "string", "nullable": true},
                "defaultProject": {"type": "string", "nullable": true},
                "taskTemplates": {"type": "array", "items": {"type": "string"}},
                "tools": {"type": "array", "items": {"type": "string"}},
                "toolGrants": {"type": "array", "items": {"type": "string"}},
                "permissions": {"type": "array", "items": {"type": "string"}},
                "browser": {"$ref": "#/components/schemas/BotBrowserPolicy"},
                "budget": {"$ref": "#/components/schemas/BotBudget"},
                "verifier": {"$ref": "#/components/schemas/BotVerifierBinding"},
                "triggers": {"type": "array", "items": {"$ref": "#/components/schemas/BotTriggerBinding"}},
                "concurrency": {"type": "string"},
                "concurrencyCap": {"type": "integer"},
                "enabled": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ListBotsResponse".to_string(),
        json!({
            "type": "object",
            "required": ["bots"],
            "properties": {
                "bots": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["id", "name", "updatedAt"],
                        "properties": {
                            "id": {"type": "string"},
                            "name": {"type": "string"},
                            "updatedAt": {"type": "string"}
                        },
                        "additionalProperties": false
                    }
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "PausedBotSchedule".to_string(),
        json!({
            "type": "object",
            "required": ["id", "kind", "expression", "atUnixMs", "paused"],
            "properties": {
                "id": {"type": "string"},
                "kind": {"type": "string"},
                "expression": {"type": "string", "nullable": true},
                "atUnixMs": {"type": "integer", "nullable": true},
                "paused": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "AuthorityFreeBotPackage".to_string(),
        json!({
            "type": "object",
            "required": ["schemaVersion", "id", "displayName", "provenance", "contentHash"],
            "properties": {
                "schemaVersion": {"type": "integer"},
                "id": {"type": "string"},
                "displayName": {"type": "string"},
                "instructions": {"type": "string"},
                "taskTemplates": {"type": "array", "items": {"type": "string"}},
                "capabilityLabels": {"type": "array", "items": {"type": "string"}},
                "schedules": {"type": "array", "items": {"$ref": "#/components/schemas/PausedBotSchedule"}},
                "provenance": {"type": "string"},
                "contentHash": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RunBotResponse".to_string(),
        json!({
            "type": "object",
            "required": ["submitted", "botId", "scheduleId"],
            "properties": {
                "submitted": {"type": "boolean"},
                "botId": {"type": "string"},
                "scheduleId": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );

    schemas
}

fn schedule_write_properties(include_id: bool) -> Map<String, Value> {
    let mut properties = Map::new();
    if include_id {
        properties.insert("id".to_string(), json!({"type": "string"}));
    }
    properties.insert("cronExpr".to_string(), json!({"type": "string"}));
    properties.insert("prompt".to_string(), json!({"type": "string"}));
    properties.insert("timeZone".to_string(), json!({"type": "string"}));
    properties.insert(
        "skills".to_string(),
        json!({"type": "array", "items": {"type": "string"}}),
    );
    properties.insert(
        "workdir".to_string(),
        json!({"type": "string", "nullable": true}),
    );
    properties.insert(
        "toolset".to_string(),
        json!({"type": "string", "nullable": true}),
    );
    properties.insert(
        "executionBackend".to_string(),
        json!({"type": "string", "nullable": true}),
    );
    properties.insert(
        "delivery".to_string(),
        json!({
            "type": "array",
            "items": {"$ref": "#/components/schemas/ScheduleDeliveryTarget"}
        }),
    );
    properties.insert(
        "scriptAllowlist".to_string(),
        json!({"type": "array", "items": {"type": "string"}}),
    );
    properties.insert(
        "mode".to_string(),
        json!({"type": "string", "enum": ["agent", "scriptOnly"]}),
    );
    properties.insert("paused".to_string(), json!({"type": "boolean"}));
    properties.insert(
        "maxRuntimeSeconds".to_string(),
        json!({"type": "integer", "nullable": true, "description": "Maximum runtime in seconds for scriptOnly schedules; script is killed if it exceeds this limit"}),
    );
    properties
}
