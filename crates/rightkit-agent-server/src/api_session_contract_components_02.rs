use super::*;

pub(crate) fn insert_session_components_02(schemas: &mut Map<String, Value>) {
    schemas.insert(
        "EditGoalRequest".to_string(),
        json!({
            "type": "object",
            "properties": {
                "text": {"type": "string"},
                "budgetTokens": {"type": "integer"},
                "budgetUsd": {"type": "number"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "EditGoalResponse".to_string(),
        json!({
            "type": "object",
            "required": ["edited"],
            "properties": {
                "edited": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RewindSessionResponse".to_string(),
        json!({
            "type": "object",
            "required": ["rewound", "text"],
            "properties": {
                "rewound": {"type": "boolean"},
                "text": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "OrchestrateSessionRequest".to_string(),
        json!({
            "type": "object",
            "required": ["goal"],
            "properties": {
                "goal": {"type": "string"},
                "dryRun": {"type": "boolean"},
                "requestLabel": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "OrchestrateSessionResponse".to_string(),
        json!({
            "type": "object",
            "required": ["accepted"],
            "properties": {
                "accepted": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "DeleteSessionResponse".to_string(),
        json!({
            "type": "object",
            "required": ["deleted", "id"],
            "properties": {
                "deleted": {"type": "boolean"},
                "id": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SessionSpecCompletionResponse".to_string(),
        json!({
            "type": "object",
            "required": ["sessionId", "bound", "status", "canComplete", "blockerCount", "warningCount"],
            "properties": {
                "sessionId": {"type": "string"},
                "bound": {"type": "boolean"},
                "specId": {"type": ["string", "null"]},
                "specTitle": {"type": "string"},
                "status": {"type": "string"},
                "canComplete": {"type": "boolean"},
                "analysisId": {"type": "string"},
                "coverage": {"type": "object", "additionalProperties": true},
                "blockerCount": {"type": "integer"},
                "warningCount": {"type": "integer"},
                "blockers": {"type": "array", "items": {"type": "object", "additionalProperties": true}},
                "warnings": {"type": "array", "items": {"type": "object", "additionalProperties": true}}
            },
            "additionalProperties": true
        }),
    );
    schemas.insert(
        "TranscriptMessage".to_string(),
        json!({
            "type": "object",
            "required": ["$kind"],
            "properties": {
                "$kind": {"type": "string"},
                "id": {"type": "string"},
                "createdAt": {"type": "string"},
                "content": {"type": "string"},
                "summary": {"type": "string"},
                "toolCallId": {"type": "string"},
                "pinned": {"type": "boolean"}
            },
            "additionalProperties": true
        }),
    );
    schemas.insert(
        "ProtocolEvent".to_string(),
        json!({
            "type": "object",
            "required": ["$kind"],
            "properties": {
                "$kind": {"type": "string"},
                "sessionId": {"type": "string"},
                "seq": {"type": "integer"},
                "createdAt": {"type": "string"}
            },
            "additionalProperties": true
        }),
    );
    schemas.insert(
        "UntrustedContentResponse".to_string(),
        json!({
            "type": "object",
            "required": ["sessionId", "items"],
            "properties": {
                "sessionId": {"type": "string"},
                "untrustedCount": {"type": "integer", "minimum": 0},
                "items": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["index", "messageId", "messageKind", "createdAt", "trust", "policyAction", "preview"],
                        "properties": {
                            "index": {"type": "integer", "minimum": 0},
                            "messageId": {"type": "string"},
                            "messageKind": {"type": "string", "enum": ["user", "tool"]},
                            "createdAt": {"type": "string"},
                            "trust": {"type": "object", "additionalProperties": true},
                            "policyAction": {
                                "type": "object",
                                "required": ["action", "approvalAuthority", "policyChangeAuthority", "instructionsAreData", "blockedAuthorityRequests", "reason"],
                                "properties": {
                                    "action": {"type": "string", "enum": ["review_required", "treat_as_data", "none"]},
                                    "approvalAuthority": {"type": "boolean"},
                                    "policyChangeAuthority": {"type": "boolean"},
                                    "instructionsAreData": {"type": "boolean"},
                                    "blockedAuthorityRequests": {
                                        "type": "array",
                                        "items": {
                                            "type": "object",
                                            "required": ["kind", "blocked", "reason"],
                                            "properties": {
                                                "kind": {"type": "string", "enum": ["approval_authority", "policy_change_authority"]},
                                                "blocked": {"type": "boolean"},
                                                "reason": {"type": "string"}
                                            },
                                            "additionalProperties": false
                                        }
                                    },
                                    "reason": {"type": "string"}
                                },
                                "additionalProperties": false
                            },
                            "preview": {"type": "string"},
                            "toolCallId": {"type": "string"},
                            "isError": {"type": "boolean"}
                        },
                        "additionalProperties": false
                    }
                }
            },
            "additionalProperties": true
        }),
    );
    schemas.insert(
        "AwaySummaryResponse".to_string(),
        json!({
            "type": "object",
            "required": ["messageCount", "hasActivity", "bullets"],
            "properties": {
                "messageCount": {"type": "integer"},
                "userMessages": {"type": "integer"},
                "assistantMessages": {"type": "integer"},
                "toolResults": {"type": "integer"},
                "hasActivity": {"type": "boolean"},
                "bullets": {"type": "array", "items": {"type": "string"}}
            },
            "additionalProperties": true
        }),
    );
    schemas.insert(
        "TaskNode".to_string(),
        json!({
            "type": "object",
            "required": ["id"],
            "properties": {
                "id": {"type": "string"},
                "title": {"type": "string"},
                "status": {"type": "string"}
            },
            "additionalProperties": true
        }),
    );
    schemas.insert(
        "ArtifactRow".to_string(),
        json!({
            "type": "object",
            "required": ["id"],
            "properties": {
                "id": {"type": "string"},
                "kind": {"type": "string"},
                "title": {"type": "string"},
                "path": {"type": "string"}
            },
            "additionalProperties": true
        }),
    );
    schemas.insert(
        "InstructionSourceRow".to_string(),
        json!({
            "type": "object",
            "required": ["scope", "path", "bodyBytes", "bodyLines", "globs", "paths", "includes", "hookRefs"],
            "properties": {
                "scope": {"type": "string"},
                "path": {"type": "string"},
                "bodyBytes": {"type": "integer", "minimum": 0},
                "bodyLines": {"type": "integer", "minimum": 0},
                "globs": {"type": "array", "items": {"type": "string"}},
                "paths": {"type": "array", "items": {"type": "string"}},
                "includes": {"type": "array", "items": {"type": "string"}},
                "hookRefs": {"type": "array", "items": {"type": "string"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "InstructionStatus".to_string(),
        json!({
            "type": "object",
            "required": ["workspaceRoot", "fileCount", "composedBytes", "files"],
            "properties": {
                "workspaceRoot": {"type": "string"},
                "fileCount": {"type": "integer", "minimum": 0},
                "composedBytes": {"type": "integer", "minimum": 0},
                "files": {"type": "array", "items": {"$ref": "#/components/schemas/InstructionSourceRow"}}
            },
            "additionalProperties": false
        }),
    );
}
