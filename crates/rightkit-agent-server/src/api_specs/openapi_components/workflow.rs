use super::*;

pub(super) fn insert(schemas: &mut Map<String, Value>) {
    schemas.insert(
        "SpecTaskInput".to_string(),
        json!({
            "type": "object",
            "required": ["description"],
            "properties": {
                "id": {"type": "string"},
                "storyId": {"type": "string", "nullable": true},
                "requirementIds": {"type": "array", "items": {"type": "string"}},
                "successCriterionIds": {"type": "array", "items": {"type": "string"}},
                "foundationPhase": {"type": "string", "nullable": true},
                "description": {"type": "string"},
                "paths": {"type": "array", "items": {"type": "string"}},
                "parallelizable": {"type": "boolean"},
                "status": {"$ref": "#/components/schemas/TaskStatus"},
                "evidenceIds": {"type": "array", "items": {"type": "string"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "AddSpecTasksRequest".to_string(),
        json!({
            "type": "object",
            "required": ["tasks"],
            "properties": {
                "tasks": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/SpecTaskInput"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ConstitutionDecisionRequest".to_string(),
        json!({
            "type": "object",
            "required": ["rule", "verdict", "rationale"],
            "properties": {
                "id": {"type": "string"},
                "sourcePath": {"type": "string", "nullable": true},
                "sourceSha256": {"type": "string", "nullable": true},
                "rule": {"type": "string"},
                "verdict": {"$ref": "#/components/schemas/ConstitutionDecisionVerdict"},
                "rationale": {"type": "string"},
                "evidenceIds": {"type": "array", "items": {"type": "string"}}
            },
            "additionalProperties": false
        }),
    );

    schemas.insert("FeatureSpec".to_string(), feature_spec_schema());
    schemas.insert("FeatureTask".to_string(), feature_task_schema());
    schemas.insert(
        "ConstitutionDecision".to_string(),
        constitution_decision_schema(),
    );
    schemas.insert(
        "SpecAnalysisRun".to_string(),
        specs_object_schema("Coverage analysis result for a feature spec."),
    );
    schemas.insert(
        "SpecTrace".to_string(),
        specs_object_schema("Requirement, task, and evidence trace for a feature spec."),
    );
    schemas.insert(
        "SpecEnvelope".to_string(),
        json!({
            "type": "object",
            "required": ["spec", "tasks"],
            "properties": {
                "spec": {"$ref": "#/components/schemas/FeatureSpec"},
                "tasks": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/FeatureTask"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SpecClarification".to_string(),
        json!({
            "type": "object",
            "required": ["id", "sourceKind", "sourceId", "question", "required"],
            "properties": {
                "id": {"type": "string"},
                "sourceKind": {"$ref": "#/components/schemas/TraceKind"},
                "sourceId": {"type": "string"},
                "question": {"type": "string"},
                "required": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SpecClarifyResponse".to_string(),
        json!({
            "type": "object",
            "required": ["specId", "questions", "blocking", "nextAction"],
            "properties": {
                "specId": {"type": "string"},
                "questions": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/SpecClarification"}
                },
                "blocking": {"type": "boolean"},
                "nextAction": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SpecPlanResponse".to_string(),
        json!({
            "type": "object",
            "required": ["spec", "tasks", "createdTaskCount", "skippedExistingCount", "analysis"],
            "properties": {
                "spec": {"$ref": "#/components/schemas/FeatureSpec"},
                "tasks": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/FeatureTask"}
                },
                "createdTaskCount": {"type": "integer", "minimum": 0},
                "skippedExistingCount": {"type": "integer", "minimum": 0},
                "analysis": {"$ref": "#/components/schemas/SpecAnalysisRun"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SpecChecklistItem".to_string(),
        json!({
            "type": "object",
            "required": ["id", "label", "status", "sourceKind", "sourceId", "detail"],
            "properties": {
                "id": {"type": "string"},
                "label": {"type": "string"},
                "status": {"type": "string"},
                "sourceKind": {"$ref": "#/components/schemas/TraceKind"},
                "sourceId": {"type": "string"},
                "detail": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SpecChecklistResponse".to_string(),
        json!({
            "type": "object",
            "required": ["specId", "ready", "blockerCount", "warningCount", "items", "analysis"],
            "properties": {
                "specId": {"type": "string"},
                "focus": {"type": "string", "nullable": true},
                "ready": {"type": "boolean"},
                "blockerCount": {"type": "integer", "minimum": 0},
                "warningCount": {"type": "integer", "minimum": 0},
                "items": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/SpecChecklistItem"}
                },
                "analysis": {"$ref": "#/components/schemas/SpecAnalysisRun"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SpecImplementResponse".to_string(),
        json!({
            "type": "object",
            "required": ["specId", "accepted", "dryRun", "prompt", "analysis"],
            "properties": {
                "specId": {"type": "string"},
                "sessionId": {"type": "string", "nullable": true},
                "accepted": {"type": "boolean"},
                "dryRun": {"type": "boolean"},
                "prompt": {"type": "string"},
                "analysis": {"$ref": "#/components/schemas/SpecAnalysisRun"}
            },
            "additionalProperties": false
        }),
    );
}
