fn insert_corpus_bakeoff_openapi_components(schemas: &mut Map<String, Value>) {
    schemas.insert(
        "CorpusRequiredArtifact".to_string(),
        json!({
            "type": "object",
            "required": ["name", "path", "expectedOutputPath"],
            "properties": {
                "name": {"type": "string", "minLength": 1, "maxLength": 128},
                "path": {"type": "string", "minLength": 1},
                "expectedOutputPath": {"type": "string", "minLength": 1, "description": "Verifier-only expected bytes outside the task workspace Git repository."}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CorpusCommandVerifier".to_string(),
        json!({
            "type": "object",
            "required": ["kind", "command"],
            "properties": {
                "kind": {"type": "string", "const": "command"},
                "command": {"type": "array", "minItems": 1, "maxItems": MAX_VERIFIER_ARGS, "items": {"type": "string", "minLength": 1, "maxLength": MAX_VERIFIER_ARG_BYTES}},
                "requiredArtifacts": {"type": "array", "maxItems": 16, "items": {"$ref": "#/components/schemas/CorpusRequiredArtifact"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CorpusBakeoffRequest".to_string(),
        json!({
            "type": "object",
            "required": ["manifestPath"],
            "properties": {
                "manifestPath": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Operator-selected local path to a coderight.eval-corpus.v1 JSON manifest."
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CorpusExactResponseVerifier".to_string(),
        json!({
            "type": "object",
            "required": ["kind", "expectedOutputPath"],
            "properties": {
                "kind": {"type": "string", "const": "exactResponse"},
                "expectedOutputPath": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Relative path to verifier-only expected output outside the agent workspace."
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "PrivateCorpusTaskSource".to_string(),
        json!({
            "type": "object",
            "required": ["kind", "instructionPath", "workspaceRoot"],
            "properties": {
                "kind": {"type": "string", "const": "private"},
                "instructionPath": {"type": "string", "minLength": 1},
                "workspaceRoot": {"type": "string", "minLength": 1}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "HarborCorpusTaskSource".to_string(),
        json!({
            "type": "object",
            "required": ["kind", "taskRoot", "workspaceRoot"],
            "properties": {
                "kind": {"type": "string", "const": "harbor"},
                "taskRoot": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Relative Harbor task directory containing task.toml and instruction.md."
                },
                "workspaceRoot": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Relative path to an already-materialized local Git workspace; RightKit does not invoke Harbor Docker environments."
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CorpusBakeoffTask".to_string(),
        json!({
            "type": "object",
            "required": ["caseNonce", "source", "verifier"],
            "properties": {
                "caseNonce": {"type": "string", "minLength": 1, "maxLength": 128},
                "source": {
                    "oneOf": [
                        {"$ref": "#/components/schemas/PrivateCorpusTaskSource"},
                        {"$ref": "#/components/schemas/HarborCorpusTaskSource"}
                    ]
                },
                "verifier": {"oneOf": [
                    {"$ref": "#/components/schemas/CorpusExactResponseVerifier"},
                    {"$ref": "#/components/schemas/CorpusCommandVerifier"}
                ]},
                "contextTokens": {"type": "integer", "minimum": 0, "maximum": MAX_CONTEXT_TOKENS},
                "contextClass": {"type": "string", "enum": ["standard", "long"]},
                "contextCohort": {"type": ["string", "null"], "maxLength": 128},
                "candidateCostLimitUsd": {"type": ["number", "null"], "exclusiveMinimum": 0, "maximum": MAX_COST_LIMIT_USD}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CorpusBakeoffManifest".to_string(),
        json!({
            "$id": "https://coderight.local/schemas/eval-corpus-v1.json",
            "type": "object",
            "required": ["schemaVersion", "suiteNamespace", "candidateRoles", "baselineRole", "tasks"],
            "properties": {
                "schemaVersion": {"type": "string", "const": CORPUS_SCHEMA_VERSION},
                "suiteNamespace": {"type": "string", "minLength": 1, "maxLength": 128},
                "candidateRoles": {"type": "array", "minItems": 2, "maxItems": 4, "uniqueItems": true, "items": {"type": "string", "minLength": 1, "maxLength": 64}},
                "baselineRole": {"type": "string", "minLength": 1, "maxLength": 64},
                "tasks": {"type": "array", "minItems": 1, "maxItems": MAX_TASKS, "items": {"$ref": "#/components/schemas/CorpusBakeoffTask"}},
                "gate": {"$ref": "#/components/schemas/ExecutedBakeoffGate"},
                "timeoutSeconds": {"type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_SECONDS, "default": DEFAULT_TIMEOUT_SECONDS}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CorpusBakeoffResponse".to_string(),
        json!({
            "type": "object",
            "required": ["schemaVersion", "manifestSha256", "taskCount", "sources", "report", "decision"],
            "properties": {
                "schemaVersion": {"type": "string", "const": CORPUS_REPORT_SCHEMA_VERSION},
                "manifestSha256": {"type": "string", "pattern": "^[0-9a-f]{64}$", "description": "Corpus snapshot digest bound to manifest bytes, resolved instructions, verifier expectations, and clean workspace Git HEAD/subdirectory identity."},
                "taskCount": {"type": "integer", "minimum": 1, "maximum": MAX_TASKS},
                "sources": {
                    "type": "object",
                    "required": ["private", "harbor"],
                    "properties": {
                        "private": {"type": "integer", "minimum": 0},
                        "harbor": {"type": "integer", "minimum": 0}
                    },
                    "additionalProperties": false
                },
                "report": {
                    "type": "object",
                    "description": "Aggregate coderight.executed-bakeoff.v1 report with verified completion, failures, cost, latency, escalation, tool efficiency, review false positives, long-context degradation, and same-cost baseline metrics."
                },
                "decision": {
                    "type": "object",
                    "required": ["passed", "failures"],
                    "properties": {
                        "passed": {"type": "boolean"},
                        "failures": {"type": "array", "items": {"type": "string"}}
                    },
                    "additionalProperties": false
                }
            },
            "additionalProperties": false
        }),
    );
}
