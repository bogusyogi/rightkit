pub(crate) fn settings_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    schemas.insert(
        "SettingsModelCatalogEntry".to_string(),
        json!({
            "type": "object",
            "required": ["id", "displayName", "provider", "modelSlug", "contextWindow", "supportsVision", "supportsTools", "enabled"],
            "properties": {
                "id": {"type": "string"},
                "displayName": {"type": "string"},
                "provider": {"type": "string"},
                "modelSlug": {"type": "string"},
                "contextWindow": {"type": "integer", "minimum": 0},
                "inputCostPer1M": {"type": "number"},
                "outputCostPer1M": {"type": "number"},
                "supportsVision": {"type": "boolean"},
                "supportsTools": {"type": "boolean"},
                "enabled": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SettingsAgentEntry".to_string(),
        json!({
            "type": "object",
            "required": ["id", "name", "role", "modelId", "enabled"],
            "properties": {
                "id": {"type": "string"},
                "name": {"type": "string"},
                "role": {"type": "string"},
                "modelId": {"type": "string"},
                "enabled": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SettingsBudget".to_string(),
        json!({
            "type": "object",
            "required": [
                "perTaskMaxUsd",
                "perGoalMaxUsd",
                "perSessionMaxUsd",
                "perAgentCallMaxUsd",
                "globalMonthlyMaxUsd",
                "warningAt",
                "maxJuryCallsPerRun",
                "maxEscalationDepth",
                "maxRetriesPerRole",
                "maxWallClockMinutes",
                "maxIterations",
                "maxWorkers"
            ],
            "properties": {
                "perTaskMaxUsd": {"type": "number"},
                "perGoalMaxUsd": {"type": "number"},
                "perSessionMaxUsd": {"type": "number"},
                "perAgentCallMaxUsd": {"type": "number"},
                "globalMonthlyMaxUsd": {"type": "number"},
                "warningAt": {"type": "number"},
                "maxJuryCallsPerRun": {"type": "integer", "minimum": 0},
                "maxEscalationDepth": {"type": "integer", "minimum": 0},
                "maxRetriesPerRole": {"type": "integer", "minimum": 0},
                "maxWallClockMinutes": {"type": "integer", "minimum": 0},
                "maxIterations": {"type": "integer", "minimum": 0},
                "maxWorkers": {"type": "integer", "minimum": 0}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SettingsBenchmarks".to_string(),
        json!({
            "type": "object",
            "required": ["dailyHealthChecksEnabled", "promotionMinQualityDelta"],
            "properties": {
                "dailyHealthChecksEnabled": {"type": "boolean"},
                "promotionMinQualityDelta": {"type": "number"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SettingsRemote".to_string(),
        json!({
            "type": "object",
            "required": ["telegramTokenConfigured", "telegramAllowedUsers", "webEnabled", "webEnabledConfigured", "webEnabledSource"],
            "properties": {
                "telegramTokenConfigured": {"type": "boolean"},
                "telegramAllowedUsers": {
                    "type": "array",
                    "items": {"type": "integer", "minimum": 0}
                },
                "webEnabled": {"type": "boolean"},
                "webEnabledConfigured": {"type": ["boolean", "null"]},
                "webEnabledSource": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SettingsRouting".to_string(),
        json!({
            "type": "object",
            "required": ["capabilityRouter", "capabilityRouterConfigured", "capabilityRouterSource", "aggressiveness"],
            "properties": {
                "capabilityRouter": {"type": "boolean"},
                "capabilityRouterConfigured": {"type": ["boolean", "null"]},
                "capabilityRouterSource": {"type": "string"},
                "aggressiveness": {"type": "string", "enum": ["conservative", "balanced", "aggressive"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "OrchestrationSettingsResponse".to_string(),
        json!({
            "type": "object",
            "required": ["workspaceRoot", "defaultModel", "primaryOrchestratorModelId", "roleModelMode", "roleModelSplitAvailable", "models", "agents", "budget", "benchmarks", "remote", "orchestration", "routing", "keepDaemonResident"],
            "properties": {
                "workspaceRoot": {"type": "string"},
                "defaultModel": {"type": "string"},
                "primaryOrchestratorModelId": {"type": "string"},
                "roleModelMode": {"type": "string", "enum": ["single", "split"]},
                "roleModelSplitAvailable": {"type": "boolean"},
                "models": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/SettingsModelCatalogEntry"}
                },
                "agents": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/SettingsAgentEntry"}
                },
                "budget": {"$ref": "#/components/schemas/SettingsBudget"},
                "benchmarks": {"$ref": "#/components/schemas/SettingsBenchmarks"},
                "remote": {"$ref": "#/components/schemas/SettingsRemote"},
                "orchestration": {
                    "type": "object",
                    "required": ["conductorRouting", "conductorRoutingConfigured", "conductorRoutingSource"],
                    "properties": {
                        "conductorRouting": {"type": "boolean"},
                        "conductorRoutingConfigured": {"type": ["boolean", "null"]},
                        "conductorRoutingSource": {"type": "string"}
                    },
                    "additionalProperties": false
                },
                "routing": {"$ref": "#/components/schemas/SettingsRouting"},
                "keepDaemonResident": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SaveRemoteSettings".to_string(),
        json!({
            "type": "object",
            "properties": {
                "telegramToken": {"type": "string"},
                "clearTelegramToken": {"type": "boolean"},
                "telegramAllowedUsers": {
                    "type": "array",
                    "items": {"type": "integer", "minimum": 0}
                },
                "webEnabled": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SaveSettings".to_string(),
        json!({
            "type": "object",
            "properties": {
                "workspaceRoot": {"type": "string"},
                "primaryOrchestratorModelId": {"type": "string"},
                "roleModelMode": {"type": "string", "enum": ["single", "split"]},
                "budget": {"$ref": "#/components/schemas/SettingsBudget"},
                "benchmarks": {"type": "object", "additionalProperties": true},
                "remote": {"$ref": "#/components/schemas/SaveRemoteSettings"},
                "modelOverrides": {"type": "object", "additionalProperties": {"type": "string"}},
                "routing": {"$ref": "#/components/schemas/SettingsRouting"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SaveSettingsResponse".to_string(),
        json!({
            "type": "object",
            "required": ["saved"],
            "properties": {
                "saved": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ConfigFileStatus".to_string(),
        json!({
            "type": "object",
            "required": ["scope", "path", "exists", "modifiedMs", "bytes", "sha256"],
            "properties": {
                "scope": {"type": "string"},
                "path": {"type": "string"},
                "exists": {"type": "boolean"},
                "modifiedMs": {"type": ["integer", "null"], "minimum": 0},
                "bytes": {"type": ["integer", "null"], "minimum": 0},
                "sha256": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ConfigStatus".to_string(),
        json!({
            "type": "object",
            "required": ["workspaceRoot", "fingerprint", "loadedAt", "files"],
            "properties": {
                "workspaceRoot": {"type": "string"},
                "fingerprint": {"type": "string"},
                "loadedAt": {"type": "string"},
                "files": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/ConfigFileStatus"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ReloadSettingsRequest".to_string(),
        json!({
            "type": "object",
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ReloadSettingsResponse".to_string(),
        json!({
            "type": "object",
            "required": ["reloaded", "reloadedAt", "configStatus", "settings"],
            "properties": {
                "reloaded": {"type": "boolean"},
                "reloadedAt": {"type": "string"},
                "configStatus": {"$ref": "#/components/schemas/ConfigStatus"},
                "settings": {"$ref": "#/components/schemas/OrchestrationSettingsResponse"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ProviderKeyStatus".to_string(),
        json!({
            "type": "object",
            "required": ["provider", "configured", "path", "source"],
            "properties": {
                "provider": {"type": "string"},
                "configured": {"type": "boolean"},
                "path": {"type": "string"},
                "source": {"type": "string", "enum": ["env", "keychain", "file", "missing"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SaveProviderKeyRequest".to_string(),
        json!({
            "type": "object",
            "required": ["apiKey"],
            "properties": {
                "apiKey": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ProviderKeyTestResponse".to_string(),
        json!({
            "type": "object",
            "required": ["provider", "state", "valid", "checkedAt"],
            "properties": {
                "provider": {"type": "string"},
                "state": {"type": "string", "enum": ["accepted", "rejected", "unverified", "provider_error", "unsupported", "unreachable", "missing", "unreadable"]},
                "valid": {"type": "boolean"},
                "source": {"type": "string", "enum": ["env", "keychain", "file"]},
                "detail": {"type": "string"},
                "remedy": {"type": ["string", "null"]},
                "providerStatus": {"type": ["integer", "null"], "minimum": 100, "maximum": 599},
                "checkedAt": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ToolsetSettingsProfile".to_string(),
        json!({
            "type": "object",
            "required": ["id", "name", "description", "allowedTools", "disallowedTools", "readOnly", "defaultBehavioralSkills"],
            "properties": {
                "id": {"type": "string"},
                "name": {"type": "string"},
                "description": {"type": "string"},
                "allowedTools": {"type": "array", "items": {"type": "string"}},
                "disallowedTools": {"type": "array", "items": {"type": "string"}},
                "readOnly": {"type": "boolean"},
                "defaultBehavioralSkills": {"type": "array", "items": {"type": "string"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SettingsToolsetsResponse".to_string(),
        json!({
            "type": "object",
            "required": ["workspaceRoot", "path", "configured", "effective"],
            "properties": {
                "workspaceRoot": {"type": "string"},
                "path": {"type": "string"},
                "configured": {"type": "array", "items": {"$ref": "#/components/schemas/ToolsetSettingsProfile"}},
                "effective": {"type": "array", "items": {"$ref": "#/components/schemas/ToolsetProfile"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SaveToolsetConfig".to_string(),
        json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "description": {"type": "string"},
                "allowedTools": {"type": "array", "items": {"type": "string"}},
                "disallowedTools": {"type": "array", "items": {"type": "string"}},
                "readOnly": {"type": "boolean"},
                "defaultBehavioralSkills": {"type": "array", "items": {"type": "string"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SaveSettingsToolsetResponse".to_string(),
        json!({
            "type": "object",
            "required": ["saved", "toolset", "configStatus"],
            "properties": {
                "saved": {"type": "boolean"},
                "toolset": {"$ref": "#/components/schemas/ToolsetSettingsProfile"},
                "configStatus": {"$ref": "#/components/schemas/ConfigStatus"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "DeleteSettingsToolsetResponse".to_string(),
        json!({
            "type": "object",
            "required": ["deleted", "id", "configStatus"],
            "properties": {
                "deleted": {"type": "boolean"},
                "id": {"type": "string"},
                "configStatus": {"$ref": "#/components/schemas/ConfigStatus"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "InferenceProtocol".to_string(),
        json!({"type":"string","enum":["openai_compatible","anthropic_compatible"]}),
    );
    schemas.insert(
        "DirectAuthKind".to_string(),
        json!({"type":"string","enum":["none","bearer","x_api_key","api_key_header"]}),
    );
    schemas.insert("SaveDirectInferenceConnection".to_string(), json!({
        "type":"object", "required":["name","protocol","normalizedBaseUrl","authKind"],
        "properties": {"name":{"type":"string","minLength":1,"maxLength":80},"protocol":{"$ref":"#/components/schemas/InferenceProtocol"},"normalizedBaseUrl":{"type":"string"},"authKind":{"$ref":"#/components/schemas/DirectAuthKind"},"authHeaderName":{"type":["string","null"]},"publicHeaders":{"type":"object","maxProperties":32,"additionalProperties":{"type":"string","maxLength":1024}},"manualModelIds":{"type":"array","maxItems":500,"items":{"type":"string","minLength":1,"maxLength":256},"uniqueItems":true},"trustPrivateNetwork":{"type":"boolean"}}, "additionalProperties":false
    }));
    schemas.insert("SaveDirectInferenceSecret".to_string(), json!({"type":"object","required":["secret"],"properties":{"secret":{"type":"string","minLength":1,"maxLength":16384}},"additionalProperties":false}));
    schemas.insert("InferenceProbeResponse".to_string(), json!({"type":"object","required":["authority","connectionId","state","qualified","models"],"properties":{"authority":{"type":"string","enum":["direct"]},"connectionId":{"type":"string"},"state":{"type":"string","enum":["ready"]},"qualified":{"type":"boolean"},"models":{"type":"array","items":{"type":"object"}}},"additionalProperties":false}));
    schemas
}

pub(crate) fn orchestration_settings_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/OrchestrationSettingsResponse"})
}

pub(crate) fn save_settings_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/SaveSettings"})
}

pub(crate) fn save_settings_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/SaveSettingsResponse"})
}

pub(crate) fn config_status_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ConfigStatus"})
}

pub(crate) fn reload_settings_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/ReloadSettingsRequest"})
}

pub(crate) fn reload_settings_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ReloadSettingsResponse"})
}

pub(crate) fn save_provider_key_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/SaveProviderKeyRequest"})
}

pub(crate) fn provider_key_status_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ProviderKeyStatus"})
}

pub(crate) fn settings_openapi_request_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("put", "/settings/toolsets/{id}") => {
            Some(json!({"$ref": "#/components/schemas/SaveToolsetConfig"}))
        }
        ("post", "/inference/connections/direct")
        | ("put", "/inference/connections/direct/{id}") => {
            Some(json!({"$ref":"#/components/schemas/SaveDirectInferenceConnection"}))
        }
        ("put", "/inference/connections/direct/{id}/secret") => {
            Some(json!({"$ref":"#/components/schemas/SaveDirectInferenceSecret"}))
        }
        _ => None,
    }
}

pub(crate) fn inference_connections_response_schema() -> Value {
    json!({"type": "object", "additionalProperties": true})
}

pub(crate) fn inference_secret_response_schema() -> Value {
    json!({"type": "object", "additionalProperties": true})
}

pub(crate) fn inference_probe_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/InferenceProbeResponse"})
}

pub(crate) fn inference_catalog_response_schema() -> Value {
    json!({"type": "object", "additionalProperties": true})
}

pub(crate) fn inference_load_response_schema() -> Value {
    json!({"type": "object", "additionalProperties": true})
}

pub(crate) fn inference_connection_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/SaveDirectInferenceConnection"})
}

pub(crate) fn inference_secret_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/SaveDirectInferenceSecret"})
}

pub(crate) fn inference_probe_request_schema() -> Value {
    json!({"type": "object", "additionalProperties": true})
}

pub(crate) fn settings_openapi_response_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("get", "/settings/toolsets") => {
            Some(json!({"$ref": "#/components/schemas/SettingsToolsetsResponse"}))
        }
        ("put", "/settings/toolsets/{id}") => {
            Some(json!({"$ref": "#/components/schemas/SaveSettingsToolsetResponse"}))
        }
        ("delete", "/settings/toolsets/{id}") => {
            Some(json!({"$ref": "#/components/schemas/DeleteSettingsToolsetResponse"}))
        }
        ("post", "/inference/connections/{authority}/{id}/probe") => {
            Some(inference_probe_response_schema())
        }
        ("post", "/provider-keys/{provider}/test") => {
            Some(json!({"$ref": "#/components/schemas/ProviderKeyTestResponse"}))
        }
        _ => None,
    }
}
