use serde_json::{json, Map, Value};

pub fn health_openapi_components(allowed_sandbox_tiers: &[&str]) -> Map<String, Value> {
    let mut schemas = Map::new();
    schemas.insert("HealthResponse".into(), json!({"type":"object","required":["ok","service","port","databasePath","utcNow","storageReadPoolIdle","providerKeyStorage"],"properties":{"ok":{"type":"boolean"},"service":{"type":"string"},"port":{"type":"integer"},"databasePath":{"type":"string"},"utcNow":{"type":"string","format":"date-time"},"storageReadPoolIdle":{"type":["integer","null"],"minimum":0},"providerKeyStorage":{"$ref":"#/components/schemas/ProviderKeyStorageStatus"}},"additionalProperties":false}));
    schemas.insert("ProviderKeyStorageStatus".into(), json!({"type":"object","required":["keychainFailureObserved"],"properties":{"keychainFailureObserved":{"type":"boolean"}},"additionalProperties":false}));
    schemas.insert(
        "HealthDiagnosticStatus".into(),
        json!({"type":"string","enum":["pass","warn","block"]}),
    );
    schemas.insert("HealthDiagnosticCheck".into(), json!({"type":"object","required":["id","label","status","detail","fix"],"properties":{"id":{"type":"string"},"label":{"type":"string"},"status":{"$ref":"#/components/schemas/HealthDiagnosticStatus"},"detail":{"type":"string"},"fix":{"type":"string"}},"additionalProperties":false}));
    schemas.insert("HealthDiagnosticsResponse".into(), json!({"type":"object","required":["verdict","checks"],"properties":{"verdict":{"$ref":"#/components/schemas/HealthDiagnosticStatus"},"checks":{"type":"array","items":{"$ref":"#/components/schemas/HealthDiagnosticCheck"}}},"additionalProperties":false}));
    schemas.insert(
        "ReadinessVerdict".into(),
        json!({"type":"string","enum":["pass","warn","block"]}),
    );
    schemas.insert(
        "ReadinessState".into(),
        json!({"type":"string","enum":["ready","degraded","unavailable","blocked"]}),
    );
    schemas.insert("ReadinessRow".into(), json!({"type":"object","required":["id","label","verdict","state","detail","fix"],"properties":{"id":{"type":"string"},"label":{"type":"string"},"verdict":{"$ref":"#/components/schemas/ReadinessVerdict"},"state":{"$ref":"#/components/schemas/ReadinessState"},"detail":{"type":"string"},"fix":{"type":"string"}},"additionalProperties":false}));
    schemas.insert("ReadinessSnapshot".into(), json!({"type":"object","required":["verdict","rows"],"properties":{"verdict":{"$ref":"#/components/schemas/ReadinessVerdict"},"rows":{"type":"array","items":{"$ref":"#/components/schemas/ReadinessRow"}}},"additionalProperties":false}));
    schemas.insert(
        "SandboxTier".into(),
        json!({"type":"string","enum":allowed_sandbox_tiers}),
    );
    schemas.insert(
        "SandboxSettingsSource".into(),
        json!({"type":"string","enum":["workspace","environment","environment+workspace","default"]}),
    );
    schemas.insert("SaveSandboxSettingsRequest".into(), json!({"type":"object","required":["tier"],"properties":{"tier":{"$ref":"#/components/schemas/SandboxTier"}},"additionalProperties":false}));
    schemas.insert("SandboxSettingsResponse".into(), json!({"type":"object","required":["workspaceRoot","storePath","tier","workspaceTier","operatorTier","source","operatorValid","workspaceValid","allowedTiers"],"properties":{"workspaceRoot":{"type":"string"},"storePath":{"type":"string"},"tier":{"$ref":"#/components/schemas/SandboxTier"},"workspaceTier":{"anyOf":[{"$ref":"#/components/schemas/SandboxTier"},{"type":"null"}]},"operatorTier":{"anyOf":[{"$ref":"#/components/schemas/SandboxTier"},{"type":"null"}]},"source":{"$ref":"#/components/schemas/SandboxSettingsSource"},"operatorValid":{"type":"boolean"},"workspaceValid":{"type":"boolean"},"allowedTiers":{"type":"array","items":{"$ref":"#/components/schemas/SandboxTier"}}},"additionalProperties":false}));
    schemas.insert("DiagnosticsBundleSummary".into(), json!({"type":"object","required":["bundleId","fileName","byteLength","crashReports","nativeArtifacts","logEvents","downloadUrl"],"properties":{"bundleId":{"type":"string"},"fileName":{"type":"string"},"byteLength":{"type":"integer","minimum":0},"crashReports":{"type":"integer","minimum":0},"nativeArtifacts":{"type":"integer","minimum":0},"logEvents":{"type":"integer","minimum":0},"downloadUrl":{"type":"string"}},"additionalProperties":false}));
    schemas.insert("DiagnosticsBundleDownload".into(), json!({"type":"object","required":["fileName","contentType","contentBase64"],"properties":{"fileName":{"type":"string"},"contentType":{"type":"string","const":"application/zip"},"contentBase64":{"type":"string"}},"additionalProperties":false}));
    schemas.insert(
        "MachineStatusSchemaVersion".into(),
        json!({"type":"string","const":"coderight.machine-status.v1"}),
    );
    schemas.insert(
        "MachineStatusLifecycle".into(),
        json!({"type":"string","enum":["healthy","stopped","unreachable","degraded"]}),
    );
    schemas.insert("MachineStatusResponse".into(), json!({"type":"object","required":["schemaVersion","status","ok","service","port","databasePath","utcNow","degraded","storageAvailable","storageReadPoolIdle","providerKeyStorage","version"],"properties":{"schemaVersion":{"$ref":"#/components/schemas/MachineStatusSchemaVersion"},"status":{"$ref":"#/components/schemas/MachineStatusLifecycle"},"ok":{"type":"boolean"},"service":{"type":"string"},"port":{"type":"integer"},"databasePath":{"type":"string"},"utcNow":{"type":"string","format":"date-time"},"degraded":{"type":"boolean"},"storageAvailable":{"type":"boolean"},"storageReadPoolIdle":{"type":["integer","null"],"minimum":0},"providerKeyStorage":{"$ref":"#/components/schemas/ProviderKeyStorageStatus"},"version":{"type":"string"},"degradedReasons":{"type":"array","items":{"type":"string"}}},"additionalProperties":false}));
    schemas.insert("MachineStatusUnavailable".into(), json!({"type":"object","required":["schemaVersion","status","ok","code","error","service"],"properties":{"schemaVersion":{"$ref":"#/components/schemas/MachineStatusSchemaVersion"},"status":{"$ref":"#/components/schemas/MachineStatusLifecycle"},"ok":{"type":"boolean","const":false},"code":{"type":"string","const":"unavailable"},"error":{"type":"string"},"service":{"type":"string"}},"additionalProperties":false}));
    schemas.insert("MachineInfoResponse".into(), json!({"type":"object","required":["service","version","os","arch","pid","databasePath","port","utcNow","storageAvailable"],"properties":{"service":{"type":"string"},"version":{"type":"string"},"os":{"type":"string"},"arch":{"type":"string"},"pid":{"type":"integer","minimum":0},"hostname":{"type":["string","null"]},"databasePath":{"type":"string"},"port":{"type":"integer"},"utcNow":{"type":"string","format":"date-time"},"storageAvailable":{"type":"boolean"}},"additionalProperties":false}));
    schemas
}

pub fn health_response_schema() -> Value {
    json!({"$ref":"#/components/schemas/HealthResponse"})
}
pub fn health_diagnostics_response_schema() -> Value {
    json!({"$ref":"#/components/schemas/HealthDiagnosticsResponse"})
}
pub fn readiness_snapshot_response_schema() -> Value {
    json!({"$ref":"#/components/schemas/ReadinessSnapshot"})
}
pub fn save_sandbox_settings_request_schema() -> Value {
    json!({"$ref":"#/components/schemas/SaveSandboxSettingsRequest"})
}
pub fn sandbox_settings_response_schema() -> Value {
    json!({"$ref":"#/components/schemas/SandboxSettingsResponse"})
}
pub fn diagnostics_bundle_response_schema() -> Value {
    json!({"$ref":"#/components/schemas/DiagnosticsBundleSummary"})
}
pub fn diagnostics_bundle_download_response_schema() -> Value {
    json!({"$ref":"#/components/schemas/DiagnosticsBundleDownload"})
}
pub fn machine_status_response_schema() -> Value {
    json!({"$ref":"#/components/schemas/MachineStatusResponse"})
}
pub fn machine_status_unavailable_response_schema() -> Value {
    json!({"$ref":"#/components/schemas/MachineStatusUnavailable"})
}
pub fn machine_info_response_schema() -> Value {
    json!({"$ref":"#/components/schemas/MachineInfoResponse"})
}
