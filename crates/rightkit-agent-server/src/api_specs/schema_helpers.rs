fn feature_spec_schema() -> Value {
    json!({
        "type": "object",
        "required": ["id", "workspaceRoot", "title", "status", "sourcePromptHash", "userStories", "functionalRequirements", "successCriteria", "assumptions", "edgeCases", "constitutionSources", "createdAt", "updatedAt"],
        "properties": {
            "id": {"type": "string"},
            "workspaceRoot": {"type": "string"},
            "branch": {"type": ["string", "null"]},
            "title": {"type": "string"},
            "status": {"$ref": "#/components/schemas/SpecStatus"},
            "sourcePromptHash": {"type": "string"},
            "userStories": {"type": "array", "items": {"type": "object"}},
            "functionalRequirements": {"type": "array", "items": {"type": "object"}},
            "successCriteria": {"type": "array", "items": {"type": "object"}},
            "assumptions": {"type": "array", "items": {"type": "string"}},
            "edgeCases": {"type": "array", "items": {"type": "string"}},
            "constitutionSources": {"type": "array", "items": {"type": "object"}},
            "createdAt": {"type": "string"},
            "updatedAt": {"type": "string"}
        }
    })
}

fn feature_task_schema() -> Value {
    json!({
        "type": "object",
        "required": ["id", "specId", "description", "paths", "parallelizable", "status", "evidenceIds", "createdAt", "updatedAt"],
        "properties": {
            "id": {"type": "string"},
            "specId": {"type": "string"},
            "storyId": {"type": ["string", "null"]},
            "requirementIds": {"type": "array", "items": {"type": "string"}},
            "successCriterionIds": {"type": "array", "items": {"type": "string"}},
            "foundationPhase": {"type": ["string", "null"]},
            "description": {"type": "string"},
            "paths": {"type": "array", "items": {"type": "string"}},
            "parallelizable": {"type": "boolean"},
            "status": {"$ref": "#/components/schemas/TaskStatus"},
            "evidenceIds": {"type": "array", "items": {"type": "string"}},
            "createdAt": {"type": "string"},
            "updatedAt": {"type": "string"}
        }
    })
}

fn constitution_decision_schema() -> Value {
    json!({
        "type": "object",
        "required": ["id", "specId", "sourcePath", "sourceSha256", "rule", "verdict", "rationale", "evidenceIds", "createdAt", "updatedAt"],
        "properties": {
            "id": {"type": "string"},
            "specId": {"type": "string"},
            "sourcePath": {"type": "string"},
            "sourceSha256": {"type": "string"},
            "rule": {"type": "string"},
            "verdict": {"$ref": "#/components/schemas/ConstitutionDecisionVerdict"},
            "rationale": {"type": "string"},
            "evidenceIds": {"type": "array", "items": {"type": "string"}},
            "createdAt": {"type": "string"},
            "updatedAt": {"type": "string"}
        },
        "additionalProperties": false
    })
}

fn coderight_specs_object_schema(description: &str) -> Value {
    json!({
        "type": "object",
        "description": description,
        "additionalProperties": true
    })
}
