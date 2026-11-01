use super::*;

pub(super) fn insert(schemas: &mut Map<String, Value>) {
    schemas.insert(
        "SpecStatus".to_string(),
        json!({"type": "string", "enum": ["draft", "planned", "implementing", "done", "blocked"]}),
    );
    schemas.insert(
        "TaskStatus".to_string(),
        json!({"type": "string", "enum": ["todo", "inProgress", "done", "blocked"]}),
    );
    schemas.insert(
        "TraceKind".to_string(),
        json!({"type": "string", "enum": ["story", "requirement", "successCriterion", "task", "evidence", "constitution"]}),
    );
    schemas.insert(
        "ConstitutionDecisionVerdict".to_string(),
        json!({"type": "string", "enum": ["compliant", "violation", "overridden"]}),
    );
    schemas.insert(
        "SpecEmptyRequest".to_string(),
        json!({
            "type": "object",
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SpecAcceptanceScenario".to_string(),
        json!({
            "type": "object",
            "required": ["id", "given", "when", "then"],
            "properties": {
                "id": {"type": "string"},
                "given": {"type": "string"},
                "when": {"type": "string"},
                "then": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SpecUserStoryInput".to_string(),
        json!({
            "type": "object",
            "required": ["title", "narrative", "independentTest"],
            "properties": {
                "id": {"type": "string"},
                "priority": {"type": "integer", "minimum": 0, "maximum": 255},
                "title": {"type": "string"},
                "narrative": {"type": "string"},
                "independentTest": {"type": "string"},
                "acceptanceScenarios": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/SpecAcceptanceScenario"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SpecRequirementInput".to_string(),
        json!({
            "type": "object",
            "required": ["text"],
            "properties": {
                "id": {"type": "string"},
                "text": {"type": "string"},
                "sourceStoryId": {"type": "string", "nullable": true},
                "needsClarification": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SpecSuccessCriterionInput".to_string(),
        json!({
            "type": "object",
            "required": ["text"],
            "properties": {
                "id": {"type": "string"},
                "text": {"type": "string"},
                "buildable": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CreateSpecRequest".to_string(),
        json!({
            "type": "object",
            "required": ["workspaceRoot", "description"],
            "properties": {
                "workspaceRoot": {"type": "string"},
                "title": {"type": "string", "nullable": true},
                "description": {"type": "string"},
                "branch": {"type": "string", "nullable": true},
                "userStories": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/SpecUserStoryInput"}
                },
                "functionalRequirements": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/SpecRequirementInput"}
                },
                "successCriteria": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/SpecSuccessCriterionInput"}
                },
                "assumptions": {"type": "array", "items": {"type": "string"}},
                "edgeCases": {"type": "array", "items": {"type": "string"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "PatchSpecRequest".to_string(),
        json!({
            "type": "object",
            "properties": {
                "title": {"type": "string"},
                "status": {"$ref": "#/components/schemas/SpecStatus"},
                "assumptions": {"type": "array", "items": {"type": "string"}},
                "edgeCases": {"type": "array", "items": {"type": "string"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "PlanSpecRequest".to_string(),
        json!({
            "type": "object",
            "properties": {
                "constraints": {"type": "string", "nullable": true},
                "includeVerification": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ChecklistSpecRequest".to_string(),
        json!({
            "type": "object",
            "properties": {
                "focus": {"type": "string", "nullable": true}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ImplementSpecRequest".to_string(),
        json!({
            "type": "object",
            "properties": {
                "title": {"type": "string", "nullable": true},
                "dryRun": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
}
