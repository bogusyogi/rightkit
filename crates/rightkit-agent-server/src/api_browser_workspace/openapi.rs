//! OpenAPI component/request/response schemas — Agent 4 merges these into
//! the central `api_contract_openapi_components()` output. Pure data; no
//! dependency on the router or the trait.

use serde_json::{json, Map, Value};

/// OpenAPI component map — Agent 4 merges into the central
/// `api_contract_openapi_components()` output.
pub fn browser_workspace_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    schemas.insert(
        "BrowserWorkspaceSummary".to_string(),
        json!({
            "type": "object",
            "required": ["paused", "profileKind", "actionLogLen", "sessions"],
            "properties": {
                "viewport": {
                    "oneOf": [
                        {"type": "null"},
                        {"$ref": "#/components/schemas/BrowserViewportInfo"}
                    ]
                },
                "actionLogLen": {"type": "integer", "minimum": 0},
                "paused": {"type": "boolean"},
                "profileKind": {
                    "type": "string",
                    "enum": ["ephemeral", "named_shared"]
                },
                "profileLabel": {"type": ["string", "null"]},
                "sessions": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/BrowserSessionSummary"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "BrowserSessionSummary".to_string(),
        json!({
            "type": "object",
            "required": ["sessionId", "ownerSessionId", "mode", "profile", "controlLease", "url", "title", "consoleErrorCount", "zoom"],
            "properties": {
                "sessionId": {"type": "string"},
                "ownerSessionId": {"type": "string"},
                "mode": {"type": "string", "enum": ["headless", "visible"]},
                "profile": {
                    "type": "object",
                    "required": ["kind", "ephemeral"],
                    "properties": {
                        "kind": {"type": "string"},
                        "label": {"type": ["string", "null"]},
                        "ephemeral": {"type": "boolean"}
                    },
                    "additionalProperties": false
                },
                "controlLease": {
                    "type": "object",
                    "required": ["owner", "generation"],
                    "properties": {
                        "owner": {"type": ["string", "null"]},
                        "generation": {"type": "integer", "minimum": 1}
                    },
                    "additionalProperties": false
                },
                "url": {"type": "string"},
                "title": {"type": "string"},
                "consoleErrorCount": {"type": "integer", "minimum": 0},
                "zoom": {"type": ["number", "null"], "minimum": 0.25, "maximum": 5.0}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "BrowserViewportInfo".to_string(),
        json!({
            "type": "object",
            "required": ["width", "height", "deviceScaleFactor"],
            "properties": {
                "width": {"type": "integer", "minimum": 1},
                "height": {"type": "integer", "minimum": 1},
                "deviceScaleFactor": {"type": "number", "minimum": 0.1}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "BrowserObservation".to_string(),
        json!({
            "type": "object",
            "required": ["schemaVersion", "browserSessionId", "ownerSessionId", "tabId", "frameIds", "epoch", "url", "title", "viewport", "axNodes", "domFallback", "diff", "consoleErrors"],
            "properties": {
                "schemaVersion": {"type": "integer", "minimum": 1},
                "browserSessionId": {"type": "string"},
                "ownerSessionId": {"type": "string"},
                "tabId": {"type": "string"},
                "frameIds": {"type": "array", "items": {"type": "string"}},
                "epoch": {"type": "integer", "minimum": 1},
                "url": {"type": "string"},
                "title": {"type": "string"},
                "viewport": {"type": "object"},
                "axNodes": {"type": "array", "items": {"type": "object"}},
                "domFallback": {"type": "array", "items": {"type": "object"}},
                "diff": {"type": "object"},
                "screenshotArtifact": {"type": ["string", "null"]},
                "consoleErrors": {"type": "integer", "minimum": 0}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "BrowserHostPolicy".to_string(),
        json!({
            "type": "object",
            "required": ["host", "decision"],
            "properties": {
                "host": {"type": "string", "description": "Hostname (no scheme, no port)."},
                "decision": {
                    "type": "string",
                    "enum": ["allow", "ask", "block"]
                },
                "trustedPrivateNetwork": {
                    "type": "boolean",
                    "description": "Explicitly permit this domain and its subdomains to resolve to private addresses; requires decision=allow."
                },
                "trustedCidrs": {
                    "type": "array",
                    "items": {"type": "string", "description": "CIDR"},
                    "description": "Replace session-scoped trusted CIDR exceptions. Every blocked resolved address must match one."
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "BrowserAnnotation".to_string(),
        json!({
            "type": "object",
            "required": ["atMs", "note"],
            "properties": {
                "atMs": {"type": "integer", "minimum": 0},
                "screenshotBase64": {"type": "string"},
                "locator": {"type": "string", "description": "CSS selector or role+name pair."},
                "viewport": {
                    "oneOf": [
                        {"type": "null"},
                        {"$ref": "#/components/schemas/BrowserViewportInfo"}
                    ]
                },
                "note": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "BrowserActionEntry".to_string(),
        json!({
            "type": "object",
            "required": ["atMs", "actor", "kind", "detail"],
            "properties": {
                "atMs": {"type": "integer", "minimum": 0},
                "actor": {
                    "type": "string",
                    "enum": ["agent", "human", "workspace"]
                },
                "kind": {"type": "string"},
                "detail": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "BrowserProfileConsent".to_string(),
        json!({
            "type": "object",
            "required": ["kind"],
            "properties": {
                "kind": {
                    "type": "string",
                    "enum": ["ephemeral", "named_shared"]
                },
                "label": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "BrowserWorkspaceError".to_string(),
        json!({
            "type": "object",
            "required": ["code", "message"],
            "properties": {
                "code": {
                    "type": "string",
                    "enum": [
                        "ssrf_refused",
                        "host_policy_blocked",
                        "browser_paused",
                        "session_not_found",
                        "consent_required",
                        "capability_denied"
                    ]
                },
                "message": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas
}

/// OpenAPI request schemas — looked up by `(method, path)` from
/// `api_contract::request_schemas`. Agent 4 may extend the lookup table.
pub fn browser_workspace_openapi_request_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("post", "/sessions/{id}/browser/workspace/open") => Some(json!({
            "type": "object",
            "required": ["url"],
            "properties": {
                "url": {"type": "string"},
                "name": {"type": "string", "description": "Optional named-profile label. Ephemeral by default."}
            }
        })),
        ("post", "/sessions/{id}/browser/workspace/navigate") => Some(json!({
            "type": "object",
            "required": ["browserSessionId", "url"],
            "properties": {
                "browserSessionId": {"type": "string"},
                "url": {"type": "string"}
            }
        })),
        ("post", "/sessions/{id}/browser/workspace/close")
        | ("post", "/sessions/{id}/browser/workspace/reload")
        | ("post", "/sessions/{id}/browser/workspace/back")
        | ("post", "/sessions/{id}/browser/workspace/forward")
        | ("post", "/sessions/{id}/browser/workspace/screenshot")
        | ("post", "/sessions/{id}/browser/workspace/console")
        | ("post", "/sessions/{id}/browser/workspace/errors") => Some(json!({
            "type": "object",
            "required": ["sessionId"],
            "properties": {"sessionId": {"type": "string"}},
            "additionalProperties": false
        })),
        ("post", "/sessions/{id}/browser/workspace/viewport") => Some(json!({
            "$ref": "#/components/schemas/BrowserViewportInfo"
        })),
        ("post", "/sessions/{id}/browser/workspace/zoom") => Some(json!({
            "type": "object",
            "required": ["zoom"],
            "properties": {"zoom": {"type": "number", "minimum": 0.25, "maximum": 5.0}}
        })),
        ("post", "/sessions/{id}/browser/workspace/annotations") => Some(json!({
            "$ref": "#/components/schemas/BrowserAnnotation"
        })),
        ("post", "/sessions/{id}/browser/workspace/host-policy") => {
            Some(json!({"$ref": "#/components/schemas/BrowserHostPolicy"}))
        }
        ("post", "/sessions/{id}/browser/workspace/profile") => {
            Some(json!({"$ref": "#/components/schemas/BrowserProfileConsent"}))
        }
        _ => None,
    }
}

/// OpenAPI response schemas — same lookup mechanism.
pub fn browser_workspace_openapi_response_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("get", "/sessions/{id}/browser/workspace") => {
            Some(json!({"$ref": "#/components/schemas/BrowserWorkspaceSummary"}))
        }
        ("post", "/sessions/{id}/browser/workspace/navigate") => Some(json!({
            "type": "object",
            "required": ["navigated", "observation"],
            "properties": {
                "navigated": {"type": "string"},
                "observation": {"$ref": "#/components/schemas/BrowserObservation"}
            },
            "additionalProperties": false
        })),
        ("post", "/sessions/{id}/browser/workspace/reload") => Some(json!({
            "type": "object",
            "required": ["reloaded", "observation"],
            "properties": {
                "reloaded": {"type": "string"},
                "observation": {"$ref": "#/components/schemas/BrowserObservation"}
            },
            "additionalProperties": false
        })),
        ("post", "/sessions/{id}/browser/workspace/back")
        | ("post", "/sessions/{id}/browser/workspace/forward") => Some(json!({
            "type": "object",
            "required": ["direction", "browserSessionId", "observation"],
            "properties": {
                "direction": {"type": "string", "enum": ["back", "forward"]},
                "browserSessionId": {"type": "string"},
                "observation": {"$ref": "#/components/schemas/BrowserObservation"}
            },
            "additionalProperties": false
        })),
        ("get", "/sessions/{id}/browser/workspace/action-log") => Some(json!({
            "type": "object",
            "required": ["sessionId", "actionLogLen", "actions"],
            "properties": {
                "sessionId": {"type": "string"},
                "actionLogLen": {"type": "integer", "minimum": 0},
                "actions": {"type": "array", "items": {"$ref": "#/components/schemas/BrowserActionEntry"}}
            },
            "additionalProperties": false
        })),
        _ => None,
    }
}
