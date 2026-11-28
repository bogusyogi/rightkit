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
            "description": "A manifest with a `profile` answers 409 `profile lock missing: run once with freeze:true` when no lock exists and freeze is false, 409 `profile drifted from its lock (manifest|profile)` when the manifest or the profile changed since the lock was written, and 409 `prompt drift: manifest pins <a>, the shipping prompt is <b>` when profile.systemPromptSha256 is not the shipping prompt digest.",
            "properties": {
                "manifestPath": {
                    "type": "string",
                    "minLength": 1,
                    "description": format!("Operator-selected local path to a {app}.eval-corpus.v1 JSON manifest. The manifest may carry a measurement block.", app = crate::host_identity::app())
                },
                "freeze": {
                    "type": "boolean",
                    "default": false,
                    "description": "Write the profile lock (<manifest path>.lock.json) before running. Only meaningful when the manifest carries a profile."
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
                    "description": "Relative path to an already-materialized local Git workspace; the daemon does not invoke Harbor Docker environments."
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
                "candidateCostLimitUsd": {"type": ["number", "null"], "exclusiveMinimum": 0, "maximum": MAX_COST_LIMIT_USD},
                "family": {"type": "string", "maxLength": 64, "description": "Task family; required when the manifest declares a measurement block."},
                "fixtureTree": {"type": "string", "pattern": "^[0-9a-f]{40}$", "description": "Git tree id of the task workspace directory; required when the manifest declares a measurement block."}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CorpusIdentity".to_string(),
        json!({
            "type": "object",
            "required": ["policyVersion", "providerId", "modelId"],
            "properties": {
                "policyVersion": {"type": "string", "minLength": 1, "maxLength": 128},
                "providerId": {"type": "string", "minLength": 1, "maxLength": 128},
                "modelId": {"type": "string", "minLength": 1, "maxLength": 128}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CorpusBudgets".to_string(),
        json!({
            "type": "object",
            "required": ["perTaskUsd", "totalUsd"],
            "properties": {
                "perTaskUsd": {"type": "number", "exclusiveMinimum": 0, "maximum": MAX_COST_LIMIT_USD},
                "totalUsd": {"type": "number", "exclusiveMinimum": 0, "maximum": MAX_COST_LIMIT_USD, "description": "At least perTaskUsd; the sum of the per-task limits must not exceed it."}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CorpusMeasurement".to_string(),
        json!({
            "type": "object",
            "required": ["identity", "seed", "repetitions", "budgets", "heldOutFamilies", "verifierVersion"],
            "properties": {
                "identity": {"$ref": "#/components/schemas/CorpusIdentity"},
                "seed": {"type": "integer", "minimum": 0},
                "repetitions": {"type": "integer", "minimum": 1, "maximum": 10},
                "budgets": {"$ref": "#/components/schemas/CorpusBudgets"},
                "heldOutFamilies": {"type": "array", "minItems": 1, "items": {"type": "string", "minLength": 1, "maxLength": 64}},
                "verifierVersion": {"type": "string", "minLength": 1, "maxLength": 128}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CorpusProfile".to_string(),
        json!({
            "type": "object",
            "required": ["modelId", "contextWindow", "effort", "systemPromptSha256"],
            "properties": {
                "modelId": {"type": "string", "minLength": 1, "maxLength": 128},
                "contextWindow": {"type": "integer", "minimum": 1},
                "effort": {"type": "string", "minLength": 1, "maxLength": 64},
                "systemPromptSha256": {"type": "string", "pattern": "^[0-9a-f]{64}$", "description": "Digest of the constant part of the shipping system prompt; a different value is refused with 409 prompt drift."}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CorpusRunArtifact".to_string(),
        json!({
            "type": "object",
            "required": ["caseNonce", "role", "classification", "contextTokens"],
            "properties": {
                "caseNonce": {"type": "string"},
                "role": {"type": "string"},
                "classification": {"type": "string", "enum": ["completed", "aborted", "deadline"]},
                "contextTokens": {"type": "integer", "minimum": 0},
                "inputTokens": {"type": "integer", "minimum": 0, "description": "Absent when the provider reported no usage."},
                "cachedInputTokens": {"type": "integer", "minimum": 0, "description": "Absent when the provider reported no usage."},
                "outputTokens": {"type": "integer", "minimum": 0, "description": "Absent when the provider reported no usage."},
                "costUsd": {"type": "number", "minimum": 0, "description": "Absent when the cost is unknown."},
                "tracePath": {"type": "string", "description": "Raw event trace file (one JSON object per line)."}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CorpusBakeoffManifest".to_string(),
        json!({
            "$id": format!("https://{app}.local/schemas/eval-corpus-v1.json", app = crate::host_identity::app()),
            "type": "object",
            "required": ["schemaVersion", "suiteNamespace", "candidateRoles", "baselineRole", "tasks"],
            "properties": {
                "schemaVersion": {"type": "string", "const": CORPUS_SCHEMA_VERSION.as_str()},
                "suiteNamespace": {"type": "string", "minLength": 1, "maxLength": 128},
                "candidateRoles": {"type": "array", "minItems": 2, "maxItems": 4, "uniqueItems": true, "items": {"type": "string", "minLength": 1, "maxLength": 64}},
                "baselineRole": {"type": "string", "minLength": 1, "maxLength": 64},
                "tasks": {"type": "array", "minItems": 1, "maxItems": MAX_TASKS, "items": {"$ref": "#/components/schemas/CorpusBakeoffTask"}},
                "gate": {"$ref": "#/components/schemas/ExecutedBakeoffGate"},
                "timeoutSeconds": {"type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_SECONDS, "default": DEFAULT_TIMEOUT_SECONDS},
                "profile": {"$ref": "#/components/schemas/CorpusProfile"},
                "measurement": {"$ref": "#/components/schemas/CorpusMeasurement"}
            },
            "description": format!("{app}.eval-corpus.v1 JSON manifest with an optional measurement block.", app = crate::host_identity::app()),
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CorpusBakeoffResponse".to_string(),
        json!({
            "type": "object",
            "required": ["schemaVersion", "manifestSha256", "taskCount", "sources", "report", "decision", "systemPromptSha256", "runs"],
            "properties": {
                "schemaVersion": {"type": "string", "const": CORPUS_REPORT_SCHEMA_VERSION.as_str()},
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
                    "description": format!("Aggregate {app}.executed-bakeoff.v1 report with verified completion, failures, cost, latency, escalation, tool efficiency, review false positives, long-context degradation, and same-cost baseline metrics.", app = crate::host_identity::app())
                },
                "decision": {
                    "type": "object",
                    "required": ["passed", "failures"],
                    "properties": {
                        "passed": {"type": "boolean"},
                        "failures": {"type": "array", "items": {"type": "string"}}
                    },
                    "additionalProperties": false
                },
                "systemPromptSha256": {"type": "string", "pattern": "^[0-9a-f]{64}$", "description": "Digest of the constant part of the shipping system prompt, read live on every run."},
                "runs": {"type": "array", "items": {"$ref": "#/components/schemas/CorpusRunArtifact"}},
                "measurement": {
                    "type": "object",
                    "description": "Echo of the validated measurement block: identity, seed, repetitions, budgets, verifierVersion, splits {development, heldout} and tasks [{caseNonce, family, split, fixtureTree}]. Present only when the manifest declares one."
                },
                "repetitionReports": {
                    "type": "array",
                    "description": "Every repetition's report in order (the first is also `report`). Present only with a measurement block.",
                    "items": {"type": "object"}
                }
            },
            "additionalProperties": false
        }),
    );
}
