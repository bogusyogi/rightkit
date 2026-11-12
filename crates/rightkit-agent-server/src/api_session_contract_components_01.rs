use super::*;

pub(crate) fn insert_session_components_01(schemas: &mut Map<String, Value>) {
    schemas.insert(
        "InferenceSelection".to_string(),
        json!({
            "type": "object",
            "required": ["schemaVersion", "authority", "connectionId", "protocol", "modelId", "routePolicyId"],
            "properties": {
                "schemaVersion": {"type": "integer", "const": 1},
                "authority": {"type": "string", "enum": ["direct"]},
                "connectionId": {"type": "string", "minLength": 1},
                "protocol": {"type": "string", "enum": ["openai_compatible", "anthropic_compatible"]},
                "modelId": {"type": "string", "minLength": 1},
                "routePolicyId": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SessionRow".to_string(),
        json!({
            "type": "object",
            "required": ["id", "title", "model", "workspaceRoot", "createdAt", "updatedAt", "status", "driverKind"],
            "properties": {
                "id": {"type": "string"},
                "title": {"type": "string"},
                "preview": {"type": ["string", "null"], "description": "First user message, one line, at most 120 characters."},
                "model": {"type": "string"},
                "inferenceSelection": {"oneOf": [{"$ref": "#/components/schemas/InferenceSelection"}, {"type": "null"}]},
                "workspaceRoot": {"type": "string"},
                "role": {"type": ["string", "null"]},
                "parentSessionId": {"type": ["string", "null"]},
                "forkedFromSessionId": {"type": ["string", "null"]},
                "createdAt": {"type": "string"},
                "updatedAt": {"type": "string"},
				"hasHistory": {"type": "boolean"},
                "status": {"type": "string"},
                "turnActive": {"type": "boolean", "description": "Live engine turn activity; absent for stored-only sessions."},
                "allowedTools": {"type": "array", "items": {"type": "string"}},
                "disallowedTools": {"type": "array", "items": {"type": "string"}},
                "toolset": {"type": ["string", "null"]},
                "activeSkills": {"type": "array", "items": {"type": "string"}},
                "executionBackend": {"type": ["string", "null"]},
                "executionWorkspaceId": {"type": ["string", "null"]},
                "executionNetworkProfile": {"type": ["string", "null"]},
                "driverKind": {"type": "string", "enum": ["native", "codex", "claude"]},
                "mcpChannel": {"type": "string"},
                "workspaceTrust": {"type": "string"},
                "specId": {"type": ["string", "null"]},
                "tags": {"type": "array", "items": {"type": "string"}},
                "effort": {"type": ["string", "null"]},
                "thinkingBudgetTokens": {"type": ["integer", "null"]},
                "disableThinking": {"type": "boolean"},
                "safeMode": {"type": "boolean"}
            },
            "additionalProperties": true
        }),
    );
    schemas.insert(
        "CreateSessionRequest".to_string(),
        json!({
            "type": "object",
            "required": ["workspaceRoot", "title"],
            "properties": {
                "id": {"type": ["string", "null"]},
                "role": {"type": ["string", "null"]},
                "workspaceRoot": {"type": "string"},
                "title": {"type": "string"},
                "parentSessionId": {"type": ["string", "null"]},
                "model": {"type": ["string", "null"]},
                "inferenceSelection": {"$ref": "#/components/schemas/InferenceSelection"},
                "allowedTools": {"type": "array", "items": {"type": "string"}},
                "disallowedTools": {"type": "array", "items": {"type": "string"}},
                "toolset": {"type": ["string", "null"]},
                "executionBackend": {"type": ["string", "null"]},
                "executionWorkspaceId": {"type": ["string", "null"]},
                "driverKind": {"type": ["string", "null"], "enum": ["native", "codex", "claude", null]},
                "mcpChannel": {"type": ["string", "null"]},
                "specId": {"type": ["string", "null"]},
                "effort": {"type": ["string", "null"]},
                "thinkingBudgetTokens": {"type": ["integer", "null"]},
                "disableThinking": {"type": ["boolean", "null"]},
                "safeMode": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ForkSessionRequest".to_string(),
        json!({
            "type": "object",
            "properties": {
                "id": {"type": ["string", "null"]},
                "title": {"type": ["string", "null"]},
                "nonce": {"type": ["string", "null"], "description": "Durable idempotency key. An identical retry replays the original response; reuse with a different body returns 409."}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ForkSessionResponse".to_string(),
        json!({
            "type": "object",
            "required": ["id", "parentSessionId", "title", "workspaceRoot", "session"],
            "properties": {
                "id": {"type": "string"},
                "parentSessionId": {"type": "string"},
                "title": {"type": "string"},
                "workspaceRoot": {"type": "string"},
                "session": {"$ref": "#/components/schemas/SessionRow"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "PatchSessionRequest".to_string(),
        json!({
            "type": "object",
            "properties": {
                "title": {"type": ["string", "null"]},
                "tags": {"type": ["array", "null"], "items": {"type": "string"}},
                "model": {"type": ["string", "null"]},
                "inferenceSelection": {"$ref": "#/components/schemas/InferenceSelection"},
                "status": {"type": ["string", "null"]},
                "driverKind": {"type": ["string", "null"], "enum": ["native", "codex", "claude", null]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "BindSessionSpecRequest".to_string(),
        json!({
            "type": "object",
            "required": ["specId"],
            "properties": {
                "specId": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ReferenceProvenance".to_string(),
        json!({
            "type": "object",
            "required": ["source", "createdAt", "trust"],
            "properties": {
                "source": {"type": "string"},
                "createdAt": {"type": "string"},
                "trust": {"type": "string"},
                "contentHash": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ReferenceMeta".to_string(),
        json!({
            "type": "object",
            "required": ["id", "kind", "destination", "provenance"],
            "properties": {
                "id": {"type": "string"},
                "kind": {"type": "string"},
                "destination": {"type": "string"},
                "mime": {"type": ["string", "null"]},
                "filename": {"type": ["string", "null"]},
                "sizeBytes": {"type": ["integer", "null"], "minimum": 0},
                "provenance": {"$ref": "#/components/schemas/ReferenceProvenance"},
                "actions": {"type": "array", "items": {"type": "string"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "PostMessageRequest".to_string(),
        json!({
            "type": "object",
            "required": ["text"],
            "properties": {
                "text": {"type": "string"},
                "nonce": {"type": ["string", "null"], "description": format!("Durable idempotency key. Identical retries replay the original response; conflicting reuse returns {app}.error.v1.", app = crate::host_identity::app())},
                "attachments": {"type": "array", "items": {"$ref": "#/components/schemas/MessageAttachment"}},
                "references": {"type": "array", "items": {"$ref": "#/components/schemas/ReferenceMeta"}},
                "workspaceRoot": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "PostMessageResponse".to_string(),
        json!({
            "type": "object",
            "required": ["schemaVersion", "sessionId", "requestId", "correlationId", "effectState", "accepted", "seq"],
            "properties": {
                "schemaVersion": {"const": crate::host_identity::schema("machine.v1")},
                "sessionId": {"type": "string"},
                "requestId": {"type": ["string", "null"]},
                "correlationId": {"type": "string"},
                "effectState": {"const": "committed"},
                "accepted": {"type": "boolean"},
                "seq": {"type": "integer"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "MessageAttachment".to_string(),
        json!({
            "type": "object",
            "required": ["type", "data", "mimeType"],
            "properties": {
                "type": {"type": "string"},
                "data": {"type": "string", "contentEncoding": "base64"},
                "mimeType": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "MachineErrorEnvelopeV1".to_string(),
        json!({
            "type": "object",
            "required": ["schemaVersion", "code", "detail", "correlationId", "retryable", "effectState", "remediation"],
            "properties": {
                "schemaVersion": {"const": crate::host_identity::schema("error.v1")},
                "code": {"type": "string"},
                "detail": {"type": "string"},
                "correlationId": {"type": "string"},
                "retryable": {"type": "boolean"},
                "effectState": {"type": "string", "enum": ["none", "committed", "in_progress", "unknown"]},
                "remediation": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CompactSessionRequest".to_string(),
        json!({
            "type": "object",
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CompactSessionResponse".to_string(),
        json!({
            "type": "object",
            "required": ["compacted", "summary", "droppedCount", "archive", "compactionPolicy"],
            "properties": {
                "compacted": {"type": "boolean"},
                "summary": {"type": "string"},
                "droppedCount": {"type": "integer", "minimum": 0},
                "archive": {
                    "oneOf": [
                        {"$ref": "#/components/schemas/CompactionArchiveReceipt"},
                        {"type": "null"}
                    ]
                },
                "compactionPolicy": {
                    "oneOf": [
                        {"$ref": "#/components/schemas/CompactionModelPolicy"},
                        {"type": "null"}
                    ]
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ResolveToolApprovalRequest".to_string(),
        json!({
            "type": "object",
            "required": ["toolCallId", "approved"],
            "properties": {
                "toolCallId": {"type": "string"},
                "approved": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ResolveToolApprovalResponse".to_string(),
        json!({
            "type": "object",
            "required": ["resolved", "approved"],
            "properties": {
                "resolved": {"type": "boolean"},
                "approved": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "AnswerQuestionRequest".to_string(),
        json!({
            "type": "object",
            "required": ["questionId", "answers"],
            "properties": {
                "questionId": {"type": "string"},
                "answers": {"type": "object", "additionalProperties": true}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "AnswerQuestionResponse".to_string(),
        json!({
            "type": "object",
            "required": ["resolved"],
            "properties": {
                "resolved": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ResolvePlanRequest".to_string(),
        json!({
            "type": "object",
            "required": ["planId", "approved"],
            "properties": {
                "planId": {"type": "string"},
                "approved": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ResolvePlanResponse".to_string(),
        json!({
            "type": "object",
            "required": ["resolved", "approved"],
            "properties": {
                "resolved": {"type": "boolean"},
                "approved": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ResumeSessionResponse".to_string(),
        json!({
            "type": "object",
            "required": ["resumed", "id"],
            "properties": {
                "resumed": {"type": "boolean"},
                "id": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "InterruptSessionResponse".to_string(),
        json!({
            "type": "object",
            "required": ["interrupted"],
            "properties": {
                "interrupted": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SteerSessionRequest".to_string(),
        json!({
            "type": "object",
            "required": ["text"],
            "properties": {
                "text": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SteerSessionResponse".to_string(),
        json!({
            "type": "object",
            "properties": {
                "steered": {"type": "boolean"},
                "queued": {"oneOf": [
                    {"$ref": "#/components/schemas/QueuedSteer"},
                    {"type": "array", "items": {"$ref": "#/components/schemas/QueuedSteer"}}
                ]},
                "edited": {"type": "boolean"},
                "item": {"$ref": "#/components/schemas/QueuedSteer"},
                "reordered": {"type": "boolean"},
                "removed": {"type": "boolean"},
                "sentNow": {"type": "boolean"}
            },
            "additionalProperties": false,
            "minProperties": 1
        }),
    );
    schemas.insert(
        "QueuedSteer".to_string(),
        json!({
            "type": "object",
            "required": ["id", "text", "dispatched"],
            "properties": {
                "id": {"type": "string"},
                "text": {"type": "string"},
                "dispatched": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "GoalStepV1".to_string(),
        json!({"type":"object","required":["id","label","weight","state","verificationState"],"properties":{"id":{"type":"string"},"label":{"type":"string"},"weight":{"type":"integer","minimum":1},"state":{"type":"string"},"verificationState":{"type":"string"}}}),
    );
    schemas.insert(
        "GoalSnapshotV1".to_string(),
        json!({"type":"object","required":["goalId","objective","status","startedAt","closedAt","dismissedAt","decompositionState","planRevision","completionPercent","now","next","steps","verificationState","budgetTokens","tokensUsed","continuationCount","noProgressCount"],"properties":{"goalId":{"type":"string"},"objective":{"type":"string"},"status":{"type":"string"},"startedAt":{"type":"string"},"closedAt":{"type":["string","null"]},"dismissedAt":{"type":["string","null"]},"decompositionState":{"type":"string"},"planRevision":{"type":"integer"},"completionPercent":{"type":["integer","null"]},"now":{"type":["string","null"]},"next":{"type":["string","null"]},"steps":{"type":"array","items":{"$ref":"#/components/schemas/GoalStepV1"}},"verificationState":{"type":"string"},"budgetTokens":{"type":"integer"},"tokensUsed":{"type":"integer"},"continuationCount":{"type":"integer"},"noProgressCount":{"type":"integer"}}}),
    );
    schemas.insert(
        "GetGoalResponse".to_string(),
        json!({"type":"object","required":["goal"],"properties":{"goal":{"anyOf":[{"$ref":"#/components/schemas/GoalSnapshotV1"},{"type":"null"}]}}}),
    );
    schemas.insert(
        "SetGoalRequest".to_string(),
        json!({
            "type": "object",
            "required": ["text"],
            "properties": {
                "text": {"type": "string"},
                "nonce": {"type": ["string", "null"], "description": "Durable idempotency key. An identical retry replays the original response; reuse with a different body returns 409."}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SetGoalResponse".to_string(),
        json!({
            "type": "object",
            "required": ["set", "goal"],
            "properties": {
                "set": {"type": "boolean"},
                "goal": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ClearGoalResponse".to_string(),
        json!({
            "type": "object",
            "required": ["cleared"],
            "properties": {
                "cleared": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "PauseGoalResponse".to_string(),
        json!({
            "type": "object",
            "required": ["paused"],
            "properties": {
                "paused": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ResumeGoalResponse".to_string(),
        json!({
            "type": "object",
            "required": ["resumed"],
            "properties": {
                "resumed": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
}
