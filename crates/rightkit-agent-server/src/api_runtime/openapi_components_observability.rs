fn insert_runtime_observability_openapi_components(schemas: &mut Map<String, Value>) {
    schemas.insert(
        "RuntimeRoleMetric".to_string(),
        json!({
            "type": "object",
            "required": ["role", "runs", "successes", "successRate"],
            "properties": {
                "role": {"type": "string"},
                "runs": {"type": "integer"},
                "successes": {"type": "integer"},
                "successRate": {"type": "number"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RuntimeMetricsResponse".to_string(),
        json!({
            "type": "array",
            "items": {"$ref": "#/components/schemas/RuntimeRoleMetric"}
        }),
    );
    schemas.insert(
        "RuntimeAuditEntry".to_string(),
        json!({
            "type": "object",
            "required": ["seq", "created_at", "kind", "subject", "summary", "prev_hash", "hash", "agent_version"],
            "properties": {
                "seq": {"type": "integer"},
                "created_at": {"type": "string"},
                "kind": {"type": "string"},
                "subject": {"type": "string"},
                "summary": {"type": "string"},
                "prev_hash": {"type": "string"},
                "hash": {"type": "string"},
                "agent_version": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RuntimeAuditResponse".to_string(),
        json!({
            "type": "object",
            "required": ["verified", "entries"],
            "properties": {
                "verified": {"type": "boolean"},
                "entries": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/RuntimeAuditEntry"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RuntimeTasteScore".to_string(),
        json!({
            "type": "object",
            "required": [
                "role",
                "samples",
                "overall",
                "gds",
                "longHorizonReliability",
                "reliabilityDecaySlope",
                "quality",
                "reliability",
                "consistency",
                "robustness",
                "predictability",
                "safety",
                "contextEfficiency",
                "vaf",
                "mopRate"
            ],
            "properties": {
                "role": {"type": "string"},
                "samples": {"type": "integer"},
                "overall": {"type": "number"},
                "gds": {"type": "number"},
                "longHorizonReliability": {"type": "number"},
                "reliabilityDecaySlope": {"type": "number"},
                "quality": {"type": "number"},
                "reliability": {"type": "number"},
                "consistency": {"type": "number"},
                "robustness": {"type": "number"},
                "predictability": {"type": "number"},
                "safety": {"type": "number"},
                "contextEfficiency": {"type": "number"},
                "vaf": {"type": "number"},
                "mopRate": {"type": "number"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RuntimeTasteResponse".to_string(),
        json!({
            "type": "array",
            "items": {"$ref": "#/components/schemas/RuntimeTasteScore"}
        }),
    );
    schemas.insert(
        "RuntimeTasteProposal".to_string(),
        json!({
            "type": "object",
            "required": ["role", "kind", "rationale", "confidence"],
            "properties": {
                "role": {"type": "string"},
                "kind": {"type": "string"},
                "rationale": {"type": "string"},
                "confidence": {"type": "number"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RuntimeTasteProposalsResponse".to_string(),
        json!({
            "type": "array",
            "items": {"$ref": "#/components/schemas/RuntimeTasteProposal"}
        }),
    );
    schemas.insert(
        "RuntimeRollout".to_string(),
        json!({
            "type": "object",
            "required": ["role", "fromModel", "toModel", "stage", "baselineRate"],
            "properties": {
                "role": {"type": "string"},
                "fromModel": {"type": "string"},
                "toModel": {"type": "string"},
                "stage": {"type": "string"},
                "baselineRate": {"type": "number"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RuntimeRolloutsResponse".to_string(),
        json!({
            "type": "array",
            "items": {"$ref": "#/components/schemas/RuntimeRollout"}
        }),
    );
    schemas.insert(
        "RuntimeFinetuningRow".to_string(),
        json!({
            "type": "object",
            "required": ["role", "model", "qualifyingRecords", "threshold", "ready"],
            "properties": {
                "role": {"type": "string"},
                "model": {"type": "string"},
                "qualifyingRecords": {"type": "integer"},
                "threshold": {"type": "number"},
                "ready": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RuntimeFinetuningResponse".to_string(),
        json!({
            "type": "array",
            "items": {"$ref": "#/components/schemas/RuntimeFinetuningRow"}
        }),
    );
    schemas.insert(
        "RuntimeMiddlewareListResponse".to_string(),
        json!({
            "type": "object",
            "required": ["middleware", "spans", "observations"],
            "properties": {
                "middleware": {"type": "array", "items": {"type": "object"}},
                "spans": {"type": "array", "items": {"$ref": "#/components/schemas/RuntimeSpan"}},
                "observations": {"type": "array", "items": {"$ref": "#/components/schemas/RuntimeObservation"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SetRuntimeMiddlewareEnabledRequest".to_string(),
        json!({
            "type": "object",
            "required": ["enabled"],
            "properties": {"enabled": {"type": "boolean"}},
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RuntimeMiddlewareUpdateResponse".to_string(),
        json!({
            "type": "object",
            "required": ["updated", "middleware"],
            "properties": {
                "updated": {"type": "boolean"},
                "middleware": {"type": "object"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RuntimeSpansResponse".to_string(),
        json!({
            "type": "object",
            "required": ["spans"],
            "properties": {
                "spans": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/RuntimeSpan"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RuntimeSpanResponse".to_string(),
        json!({
            "type": "object",
            "required": ["span", "observations"],
            "properties": {
                "span": {"$ref": "#/components/schemas/RuntimeSpan"},
                "observations": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/RuntimeObservation"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RuntimeObservationsResponse".to_string(),
        json!({
            "type": "object",
            "required": ["observations"],
            "properties": {
                "observations": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/RuntimeObservation"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RuntimeSpan".to_string(),
        json!({
            "type": "object",
            "required": ["id", "phase", "source", "status", "startedAt"],
            "properties": {
                "id": {"type": "string"},
                "parentId": {"type": "string"},
                "sessionId": {"type": "string"},
                "phase": {"type": "string"},
                "source": {"type": "string"},
                "status": {"type": "string"},
                "startedAt": {"type": "string"},
                "finishedAt": {"type": "string"},
                "attributes": {"type": "object"}
            }
        }),
    );
    schemas.insert(
        "RuntimeObservation".to_string(),
        json!({
            "type": "object",
            "required": ["id", "spanId", "sessionId", "severity", "message", "payload", "createdAt"],
            "properties": {
                "id": {"type": "string"},
                "spanId": {"type": "string"},
                "sessionId": {"type": "string"},
                "severity": {"type": "string"},
                "message": {"type": "string"},
                "payload": {},
                "createdAt": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
}
