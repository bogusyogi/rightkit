fn insert_runtime_learning_openapi_components(schemas: &mut Map<String, Value>) {
    schemas.insert(
        "RuntimeLearningResponse".to_string(),
        json!({
            "type": "array",
            "items": {"$ref": "#/components/schemas/RuntimeLearningProposal"}
        }),
    );
    schemas.insert(
        "RuntimeLearningProposal".to_string(),
        json!({
            "type": "object",
            "required": ["id", "kind", "summary", "evidence", "score", "status"],
            "properties": {
                "id": {"type": "string"},
                "kind": {"type": "string"},
                "summary": {"type": "string"},
                "evidence": {"type": "string"},
                "score": {"type": "number"},
                "status": {"type": "string"},
                "created_at": {"type": "string"},
                "createdAt": {"type": "string"},
                "age_sessions": {"type": "integer"},
                "ageSessions": {"type": "integer"}
            }
        }),
    );
    schemas.insert(
        "RuntimeLearningDecisionResponse".to_string(),
        json!({
            "type": "object",
            "required": ["ok", "id", "status", "instinctWritten"],
            "properties": {
                "ok": {"type": "boolean"},
                "id": {"type": "string"},
                "status": {"type": "string"},
                "instinctWritten": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SkillOptTarget".to_string(),
        json!({
            "type": "object",
            "required": ["skillId", "objective", "optimizer"],
            "properties": {
                "skillId": {"type": "string"},
                "objective": {"type": "string"},
                "optimizer": {"type": "string"},
                "targetModel": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SkillOptRejectedEdit".to_string(),
        json!({
            "type": "object",
            "required": ["artifactId", "reason", "patchPreview"],
            "properties": {
                "artifactId": {"type": "string"},
                "reason": {"type": "string"},
                "patchPreview": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SkillOptBenchmarkGate".to_string(),
        json!({
            "type": "object",
            "required": ["name", "kind", "threshold"],
            "properties": {
                "name": {"type": "string"},
                "kind": {"type": "string", "enum": ["hard", "soft", "mixed"]},
                "threshold": {"type": "number"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CreateSkillOptRunRequest".to_string(),
        json!({
            "type": "object",
            "required": ["name", "target"],
            "properties": {
                "name": {"type": "string"},
                "target": {"$ref": "#/components/schemas/SkillOptTarget"},
                "rejectedEdits": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/SkillOptRejectedEdit"}
                },
                "gates": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/SkillOptBenchmarkGate"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ResumeSkillOptRunRequest".to_string(),
        json!({
            "type": "object",
            "properties": {
                "checkpoint": {"type": "object"},
                "message": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SkillOptEvalEvidence".to_string(),
        json!({
            "type": "object",
            "required": [
                "train_pass_rate_pct", "heldout_pass_rate_pct", "safety_pass_rate_pct",
                "cost_ratio_to_baseline", "latency_ratio_to_baseline", "pass_rate_pct",
                "regression_count", "new_untested_capability_requested", "total_runs",
                "expected_run_count_min"
            ],
            "properties": {
                "train_pass_rate_pct": {"type": "number"},
                "heldout_pass_rate_pct": {"type": "number"},
                "safety_pass_rate_pct": {"type": "number"},
                "cost_ratio_to_baseline": {"type": "number"},
                "latency_ratio_to_baseline": {"type": "number"},
                "pass_rate_pct": {"type": "number"},
                "regression_count": {"type": "integer", "minimum": 0},
                "new_untested_capability_requested": {"type": "boolean"},
                "total_runs": {"type": "integer", "minimum": 0},
                "expected_run_count_min": {"type": "integer", "minimum": 0}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ExecuteSkillOptEpochRequest".to_string(),
        json!({
            "type": "object",
            "required": ["proposedBody", "validationScore", "evalEvidence"],
            "properties": {
                "proposedBody": {"type": "string"},
                "validationScore": {"type": "number"},
                "operation": {"type": "string"},
                "summary": {"type": "string"},
                "gateMode": {"type": "string", "enum": ["hard", "soft", "mixed"]},
                "threshold": {"type": "number"},
                "evalEvidence": {"$ref": "#/components/schemas/SkillOptEvalEvidence"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ProposeSkillOptCandidateRequest".to_string(),
        json!({
            "type": "object",
            "properties": {
                "operation": {"type": "string"},
                "summary": {"type": "string"},
                "instructions": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CancelSkillOptRunRequest".to_string(),
        json!({
            "type": "object",
            "properties": {"reason": {"type": "string"}},
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SkillOptRun".to_string(),
        json!({
            "type": "object",
            "required": ["id", "name", "target", "status", "currentEpoch", "history", "createdAt", "updatedAt"],
            "properties": {
                "id": {"type": "string"},
                "name": {"type": "string"},
                "target": {"$ref": "#/components/schemas/SkillOptTarget"},
                "rejectedEdits": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/SkillOptRejectedEdit"}
                },
                "gates": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/SkillOptBenchmarkGate"}
                },
                "status": {"type": "string"},
                "currentEpoch": {"type": "integer"},
                "history": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/SkillOptHistoryEntry"}
                },
                "candidates": {"type": "array", "items": {"type": "object"}},
                "pendingCandidateProposals": {"type": "array", "items": {"type": "object"}},
                "resumeCheckpoint": {"type": "object"},
                "bestVersionHash": {"type": "string"},
                "createdAt": {"type": "string"},
                "updatedAt": {"type": "string"},
                "completedAt": {"type": "string"},
                "skillBundle": {"type": "object"},
                "metaSkill": {"type": "object"}
            }
        }),
    );
}
