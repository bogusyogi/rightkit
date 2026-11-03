fn insert_runtime_skillopt_openapi_components(schemas: &mut Map<String, Value>) {
    schemas.insert(
        "SkillOptHistoryEntry".to_string(),
        json!({
            "type": "object",
            "required": ["event", "epoch", "message", "createdAt"],
            "properties": {
                "event": {"type": "string"},
                "epoch": {"type": "integer"},
                "message": {"type": "string"},
                "createdAt": {"type": "string"},
                "checkpoint": {"type": "object"}
            }
        }),
    );
    schemas.insert(
        "SkillOptRunsResponse".to_string(),
        json!({
            "type": "object",
            "required": ["runs"],
            "properties": {
                "storePath": {"type": "string"},
                "runs": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/SkillOptRun"}
                }
            }
        }),
    );
    schemas.insert(
        "SkillOptRunResponse".to_string(),
        json!({
            "type": "object",
            "required": ["run"],
            "properties": {
                "storePath": {"type": "string"},
                "run": {"$ref": "#/components/schemas/SkillOptRun"}
            }
        }),
    );
    schemas.insert(
        "CreateSkillOptRunResponse".to_string(),
        json!({"$ref": "#/components/schemas/SkillOptRun"}),
    );
    schemas.insert(
        "ResumeSkillOptRunResponse".to_string(),
        json!({
            "type": "object",
            "required": ["resumed", "run"],
            "properties": {
                "resumed": {"type": "boolean"},
                "run": {"$ref": "#/components/schemas/SkillOptRun"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ExecuteSkillOptEpochResponse".to_string(),
        json!({
            "type": "object",
            "required": ["executed", "accepted", "candidate", "run"],
            "properties": {
                "executed": {"type": "boolean"},
                "accepted": {"type": "boolean"},
                "candidate": {"type": "object"},
                "run": {"$ref": "#/components/schemas/SkillOptRun"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ProposeSkillOptCandidateResponse".to_string(),
        json!({
            "type": "object",
            "required": ["proposed", "proposal", "run"],
            "properties": {
                "proposed": {"type": "boolean"},
                "proposal": {"type": "object"},
                "run": {"$ref": "#/components/schemas/SkillOptRun"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CancelSkillOptRunResponse".to_string(),
        json!({
            "type": "object",
            "required": ["cancelled", "run"],
            "properties": {
                "cancelled": {"type": "boolean"},
                "run": {"$ref": "#/components/schemas/SkillOptRun"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SkillOptRunHistoryResponse".to_string(),
        json!({
            "type": "object",
            "required": ["runId", "history"],
            "properties": {
                "runId": {"type": "string"},
                "history": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/SkillOptHistoryEntry"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SkillOptRejectedEditsResponse".to_string(),
        json!({
            "type": "object",
            "required": ["runId", "rejectedEdits"],
            "properties": {
                "runId": {"type": "string"},
                "rejectedEdits": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/SkillOptRejectedEdit"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "HarnessRadarResponse".to_string(),
        json!({
            "type": "object",
            "required": ["sources", "entries", "capabilities", "capabilityMap", "proposals", "changes", "validations", "notes"],
            "properties": {
                "sources": {"type": "array", "items": {"type": "object"}},
                "entries": {"type": "array", "items": {"type": "object"}},
                "capabilities": {"type": "array", "items": {"type": "string"}},
                "capabilityMap": {"type": "object", "additionalProperties": {"type": "integer"}},
                "proposals": {"type": "array", "items": {"type": "object"}},
                "changes": {"type": "array", "items": {"type": "object"}},
                "validations": {"type": "array", "items": {"type": "object"}},
                "notes": {"type": "array", "items": {"type": "string"}}
            }
        }),
    );
    schemas.insert(
        "PromoteHarnessRadarProposalResponse".to_string(),
        json!({"$ref": "#/components/schemas/RuntimeLearningProposal"}),
    );
    schemas.insert(
        "PromoteHarnessRadarProposalRequest".to_string(),
        json!({
            "type": "object",
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ScheduleHarnessRadarRefreshRequest".to_string(),
        json!({
            "type": "object",
            "properties": {
                "workspaceRoot": {"type": "string"},
                "cronExpr": {"type": "string"},
                "timeZone": {"type": "string"},
                "paused": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ScheduleHarnessRadarRefreshResponse".to_string(),
        json!({
            "type": "object",
            "required": ["id", "schedule"],
            "properties": {
                "id": {"type": "string"},
                "created": {"type": "boolean"},
                "updated": {"type": "boolean"},
                "schedule": {"type": "object"}
            }
        }),
    );
    schemas.insert(
        "RuntimeBenchmarksResponse".to_string(),
        json!({
            "type": "array",
            "items": {"$ref": "#/components/schemas/RuntimeBenchmarkReport"}
        }),
    );
    schemas.insert(
        "RuntimeBenchmarkReport".to_string(),
        json!({
            "type": "object",
            "required": ["id", "role", "model", "suite", "repeats", "successes", "successRate", "createdAt"],
            "properties": {
                "id": {"type": "string"},
                "role": {"type": "string"},
                "model": {"type": "string"},
                "suite": {"type": "string"},
                "repeats": {"type": "integer"},
                "successes": {"type": "integer"},
                "successRate": {"type": "number"},
                "taskId": {"type": "string"},
                "passed": {"type": "boolean"},
                "agentOutput": {"type": "string"},
                "verifyOutput": {"type": "string"},
                "verifyError": {"type": "string"},
                "createdAt": {"type": "string"}
            }
        }),
    );
}
