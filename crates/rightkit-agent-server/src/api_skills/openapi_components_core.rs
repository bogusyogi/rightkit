fn insert_skill_openapi_core_components(schemas: &mut Map<String, Value>) {
    schemas.insert(
        "SkillItem".to_string(),
        json!({
            "type": "object",
            "required": ["slug", "name", "description", "trigger", "enabled", "category", "priority", "tags", "pathAffinity", "path", "directory", "kind", "valid"],
            "properties": {
                "slug": {"type": "string"},
                "name": {"type": "string"},
                "description": {"type": "string"},
                "trigger": {"type": "string"},
                "enabled": {"type": "boolean"},
                "lifecycle": {"$ref": "#/components/schemas/SkillLifecycle"},
                "active": {"type": "boolean"},
                "version": {"type": ["string", "null"]},
                "author": {"type": ["string", "null"]},
                "compatibility": {"type": "array", "items": {"type": "string"}},
                "platforms": {"type": "array", "items": {"type": "string"}},
                "requiredEnvironmentVariables": {"type": "array", "items": {"type": "string"}},
                "metadata": {"type": "object"},
                "category": {"type": "string", "enum": ["tool", "behavioral"]},
                "priority": {"type": "integer"},
                "disableModelInvocation": {"type": "boolean"},
                "tags": {"type": "array", "items": {"type": "string"}},
                "pathAffinity": {"type": "array", "items": {"type": "string"}},
                "effects": {"type": "object"},
                "path": {"type": "string"},
                "directory": {"type": "string"},
                "kind": {"type": "string"},
                "valid": {"type": "boolean"},
                "error": {"type": ["string", "null"]}
            }
        }),
    );
    schemas.insert(
        "SkillLifecycle".to_string(),
        json!({
            "type": "string",
            "enum": ["proposed", "quarantined", "benchmarked", "approved", "active", "retired"]
        }),
    );
    schemas.insert(
        "SkillGateMode".to_string(),
        json!({"type": "string", "enum": ["hard", "soft", "mixed"]}),
    );
    schemas.insert(
        "SkillListResponse".to_string(),
        json!({
            "type": "object",
            "required": ["skillDirs", "skills"],
            "properties": {
                "skillDirs": {"type": "array", "items": {"type": "string"}},
                "skills": {"type": "array", "items": {"$ref": "#/components/schemas/SkillItem"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "InstallSkillRequest".to_string(),
        json!({
            "type": "object",
            "required": ["slug", "content"],
            "properties": {
                "slug": {"type": "string"},
                "content": {"type": "string"},
                "name": {"type": "string"},
                "description": {"type": "string"},
                "trigger": {"type": "string"},
                "enabled": {"type": "boolean"},
                "category": {"type": "string", "enum": ["tool", "behavioral"]},
                "priority": {"type": "integer"},
                "tags": {"type": "array", "items": {"type": "string"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "InstallSkillResponse".to_string(),
        json!({
            "type": "object",
            "required": ["installed", "skillDir", "skill"],
            "properties": {
                "installed": {"type": "boolean"},
                "skillDir": {"type": "string"},
                "skill": {"$ref": "#/components/schemas/SkillItem"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SkillProvenanceReviewRequest".to_string(),
        json!({
            "type": "object",
            "required": ["sourceUrl", "content"],
            "properties": {
                "sourceUrl": {"type": "string"},
                "content": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SkillProvenanceFinding".to_string(),
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
        "SkillProvenanceReviewResponse".to_string(),
        json!({
            "type": "object",
            "required": ["ok", "sourceUrl", "normalizedSourceUrl", "contentHash", "recommendedLifecycle", "findings"],
            "properties": {
                "ok": {"type": "boolean"},
                "sourceUrl": {"type": "string"},
                "normalizedSourceUrl": {"type": ["string", "null"]},
                "contentHash": {"type": "string"},
                "manifestName": {"type": ["string", "null"]},
                "manifestDescription": {"type": ["string", "null"]},
                "recommendedLifecycle": {"type": "string"},
                "findings": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/SkillProvenanceFinding"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SkillReadResponse".to_string(),
        json!({
            "type": "object",
            "required": ["skill", "content"],
            "properties": {
                "skill": {"$ref": "#/components/schemas/SkillItem"},
                "content": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SkillSaveRequest".to_string(),
        json!({
            "type": "object",
            "required": ["content"],
            "properties": {
                "content": {"type": "string"},
                "enabled": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SkillSaveResponse".to_string(),
        json!({
            "type": "object",
            "required": ["saved", "skill", "content"],
            "properties": {
                "saved": {"type": "boolean"},
                "skill": {"$ref": "#/components/schemas/SkillItem"},
                "content": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SkillDeleteResponse".to_string(),
        json!({
            "type": "object",
            "required": ["deleted", "slug", "path"],
            "properties": {
                "deleted": {"type": "boolean"},
                "slug": {"type": "string"},
                "path": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SkillProposalRecord".to_string(),
        json!({
            "type": "object",
            "required": ["id", "slug", "title", "summary", "proposedContent", "contentHash", "status", "createdAtUnixSecs", "updatedAtUnixSecs"],
            "properties": {
                "id": {"type": "string"},
                "slug": {"type": "string"},
                "title": {"type": "string"},
                "summary": {"type": "string"},
                "proposedContent": {"type": "string"},
                "contentHash": {"type": "string"},
                "status": {"type": "string", "enum": ["proposed", "quarantined", "benchmarked", "active", "retired"]},
                "sourceSessionId": {"type": ["string", "null"]},
                "sourceTaskId": {"type": ["string", "null"]},
                "createdAtUnixSecs": {"type": "integer"},
                "updatedAtUnixSecs": {"type": "integer"}
            }
        }),
    );
    schemas.insert(
        "SkillVersionRecord".to_string(),
        json!({
            "type": "object",
            "required": ["id", "slug", "version", "contentHash", "changeReason", "createdAtUnixSecs"],
            "properties": {
                "id": {"type": "string"},
                "slug": {"type": "string"},
                "version": {"type": "integer"},
                "contentHash": {"type": "string"},
                "parentHash": {"type": "string"},
                "proposalId": {"type": "string"},
                "changeReason": {"type": "string"},
                "createdAtUnixSecs": {"type": "integer"}
            }
        }),
    );
}
