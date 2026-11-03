fn insert_skill_openapi_governance_components(schemas: &mut Map<String, Value>) {
    schemas.insert(
        "SkillBenchmarkVerdict".to_string(),
        json!({
            "type": "object",
            "required": ["id", "slug", "benchmarkId", "gateMode", "scoreContinuous", "passed", "verdictHash", "createdAtUnixSecs"],
            "properties": {
                "id": {"type": "string"},
                "slug": {"type": "string"},
                "proposalId": {"type": "string"},
                "benchmarkId": {"type": "string"},
                "gateMode": {"$ref": "#/components/schemas/SkillGateMode"},
                "scoreContinuous": {"type": "number"},
                "passed": {"type": "boolean"},
                "verdictHash": {"type": "string"},
                "createdAtUnixSecs": {"type": "integer"}
            }
        }),
    );
    schemas.insert(
        "SkillGovernanceAuditEntry".to_string(),
        json!({
            "type": "object",
            "required": ["id", "targetType", "targetId", "action", "actor", "operatorOverride", "timestampUnixSecs"],
            "properties": {
                "id": {"type": "string"},
                "targetType": {"type": "string"},
                "targetId": {"type": "string"},
                "slug": {"type": "string"},
                "action": {"type": "string"},
                "reason": {"type": "string"},
                "actor": {"type": "string"},
                "operatorOverride": {"type": "boolean"},
                "timestampUnixSecs": {"type": "integer"}
            }
        }),
    );
    schemas.insert(
        "SkillGovernanceResponse".to_string(),
        json!({
            "type": "object",
            "required": ["storePath", "proposals", "versions", "benchmarkVerdicts", "auditLog"],
            "properties": {
                "storePath": {"type": "string"},
                "proposals": {"type": "array", "items": {"$ref": "#/components/schemas/SkillProposalRecord"}},
                "versions": {"type": "array", "items": {"$ref": "#/components/schemas/SkillVersionRecord"}},
                "benchmarkVerdicts": {"type": "array", "items": {"$ref": "#/components/schemas/SkillBenchmarkVerdict"}},
                "auditLog": {"type": "array", "items": {"$ref": "#/components/schemas/SkillGovernanceAuditEntry"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CreateSkillProposalRequest".to_string(),
        json!({
            "type": "object",
            "required": ["slug", "summary", "content"],
            "properties": {
                "id": {"type": "string"},
                "slug": {"type": "string"},
                "title": {"type": "string"},
                "summary": {"type": "string"},
                "content": {"type": "string"},
                "sourceSessionId": {"type": ["string", "null"]},
                "sourceTaskId": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CreateSkillProposalResponse".to_string(),
        json!({
            "type": "object",
            "required": ["created", "storePath", "proposal"],
            "properties": {
                "created": {"type": "boolean"},
                "storePath": {"type": "string"},
                "proposal": {"$ref": "#/components/schemas/SkillProposalRecord"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SkillProposalQuarantineRequest".to_string(),
        json!({
            "type": "object",
            "properties": {"reason": {"type": "string"}},
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SkillProposalQuarantineResponse".to_string(),
        json!({
            "type": "object",
            "required": ["quarantined", "proposal", "version", "skill", "content"],
            "properties": {
                "quarantined": {"type": "boolean"},
                "proposal": {"$ref": "#/components/schemas/SkillProposalRecord"},
                "version": {"$ref": "#/components/schemas/SkillVersionRecord"},
                "skill": {"$ref": "#/components/schemas/SkillItem"},
                "content": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SkillBenchmarkRequest".to_string(),
        json!({
            "type": "object",
            "required": ["benchmarkId", "passed"],
            "properties": {
                "slug": {"type": "string"},
                "benchmarkId": {"type": "string"},
                "gateMode": {"$ref": "#/components/schemas/SkillGateMode"},
                "scoreContinuous": {"type": "number"},
                "passed": {"type": "boolean"},
                "verdict": {"type": "object"},
                "reason": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SkillBenchmarkResponse".to_string(),
        json!({
            "type": "object",
            "required": ["recorded", "proposal", "benchmarkVerdict"],
            "properties": {
                "recorded": {"type": "boolean"},
                "proposal": {"$ref": "#/components/schemas/SkillProposalRecord"},
                "benchmarkVerdict": {"$ref": "#/components/schemas/SkillBenchmarkVerdict"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SkillProposalBenchmarkRunRequest".to_string(),
        json!({
            "type": "object",
            "properties": {
                "kind": {"type": "string", "enum": ["role", "terminalBench", "terminal-bench", "tbench"]},
                "sessionId": {"type": "string"},
                "role": {"type": "string"},
                "model": {"type": "string"},
                "goal": {"type": "string"},
                "repeats": {"type": "integer"},
                "taskId": {"type": "string"},
                "agent": {"type": "string"},
                "benchmarkId": {"type": "string"},
                "gateMode": {"$ref": "#/components/schemas/SkillGateMode"},
                "threshold": {"type": "number"},
                "reason": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SkillProposalBenchmarkRunResponse".to_string(),
        json!({
            "type": "object",
            "required": ["ran", "gateMode", "threshold", "scoreContinuous", "runnerPassed", "record"],
            "properties": {
                "ran": {"type": "boolean"},
                "gateMode": {"$ref": "#/components/schemas/SkillGateMode"},
                "threshold": {"type": "number"},
                "scoreContinuous": {"type": "number"},
                "runnerPassed": {"type": "boolean"},
                "record": {"$ref": "#/components/schemas/SkillBenchmarkResponse"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SkillLifecycleRequest".to_string(),
        json!({
            "type": "object",
            "properties": {
                "reason": {"type": "string"},
                "operatorOverride": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SkillLifecycleResponse".to_string(),
        json!({
            "type": "object",
            "required": ["updated", "action", "skill", "content"],
            "properties": {
                "updated": {"type": "boolean"},
                "action": {"type": "string"},
                "skill": {"$ref": "#/components/schemas/SkillItem"},
                "content": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
}
