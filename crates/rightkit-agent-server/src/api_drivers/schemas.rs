//! Driver/capability request and response JSON Schemas.
//!
//! Decomposed out of `api_drivers.rs` (ra-016). Pure schema-assembly
//! functions with no I/O or state; `api_drivers.rs` re-exports them
//! unqualified (`pub(crate) use schemas::*;`) so every schema wired into
//! `response_schemas.rs`/OpenAPI stays reachable at the same call sites,
//! with identical output, as before the split.

use serde_json::{json, Value};

use super::MAX_LEASE_TTL_SECONDS;

pub(crate) fn issue_capability_lease_request_schema() -> Value {
    json!({
        "type": "object",
        "required": ["driverKind", "binaryVersion", "schemaHash"],
        "properties": {
            "driverKind": {"type": "string", "enum": ["codex", "claude"]},
            "binaryVersion": {"type": "string", "minLength": 1},
            "schemaHash": {"type": "string", "minLength": 1},
            "tools": {"type": "array", "items": {"type": "string"}, "uniqueItems": true},
            "ttlSeconds": {"type": "integer", "minimum": 1, "maximum": MAX_LEASE_TTL_SECONDS}
        },
        "additionalProperties": false
    })
}

pub(crate) fn driver_discovery_response_schema() -> Value {
    let lifecycle =
        json!({"type": "string", "enum": ["ready", "unavailable", "authentication_required"]});
    let capabilities = driver_capabilities_schema();
    json!({
        "type": "object",
        "required": ["driverKind", "installed", "authRequired", "lifecycle", "diagnostic", "capabilities", "version"],
        "properties": {
            "driverKind": {"type": "string", "enum": ["codex", "claude"]},
            "installed": {"type": "boolean"},
            "authRequired": {"type": "boolean"},
            "lifecycle": lifecycle,
            "diagnostic": {"type": ["string", "null"]},
            "capabilities": capabilities,
            "version": driver_version_schema(),
        },
        "additionalProperties": false,
    })
}

pub(crate) fn driver_models_response_schema() -> Value {
    json!({
        "type": "object",
        "required": ["models", "source"],
        "properties": {
            "models": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["id", "isDefault"],
                    "properties": {
                        "id": {"type": "string"},
                        "model": {"type": ["string", "null"]},
                        "displayName": {"type": ["string", "null"]},
                        "description": {"type": ["string", "null"]},
                        "isDefault": {"type": "boolean"},
                    },
                    "additionalProperties": false,
                },
            },
            "source": {"type": "string", "enum": ["codex-app-server", "static"]},
        },
        "additionalProperties": false,
    })
}

pub(crate) fn capability_call_request_schema() -> Value {
    json!({
        "type": "object",
        "required": ["callId", "name"],
        "properties": {
            "callId": {"oneOf": [{"type": "string", "minLength": 1}, {"type": "number"}]},
            "name": {"type": "string", "minLength": 1},
            "arguments": {"type": "object"}
        },
        "additionalProperties": false
    })
}

pub(crate) fn start_driver_request_schema() -> Value {
    json!({
        "type": "object",
        "required": ["bindingId"],
        "properties": {
            "bindingId": {"type": "string", "minLength": 1},
            "executable": {"type": ["string", "null"]},
            "recoverClaimed": {"type": "boolean", "default": false},
            "model": {"type": ["string", "null"]}
        },
        "additionalProperties": false
    })
}

pub(crate) fn driver_prompt_request_schema() -> Value {
    json!({
        "type": "object",
        "required": ["bindingId", "vendorSessionId", "text"],
        "properties": {
            "bindingId": {"type": "string", "minLength": 1},
            "vendorSessionId": {"type": "string", "minLength": 1},
            "text": {"type": "string", "minLength": 1},
            "requestLabel": {"type": ["string", "null"], "minLength": 1}
        },
        "additionalProperties": false
    })
}

pub(crate) fn driver_interrupt_request_schema() -> Value {
    json!({
        "type": "object",
        "required": ["bindingId", "vendorSessionId"],
        "properties": {
            "bindingId": {"type": "string", "minLength": 1},
            "vendorSessionId": {"type": "string", "minLength": 1},
            "vendorTurnId": {"type": ["string", "null"]}
        },
        "additionalProperties": false
    })
}

pub(crate) fn driver_binding_request_schema() -> Value {
    json!({
        "type": "object",
        "required": ["bindingId"],
        "properties": {
            "bindingId": {"type": "string", "minLength": 1},
            "vendorSessionId": {"type": ["string", "null"]}
        },
        "additionalProperties": false
    })
}

pub(crate) fn issue_capability_lease_response_schema() -> Value {
    json!({
        "type": "object",
        "required": ["leaseId", "bindingId", "sessionId", "driverKind", "expiresAt", "tools", "capabilityToken", "mcp"],
        "properties": {
            "leaseId": {"type": "string"}, "bindingId": {"type": "string"},
            "sessionId": {"type": "string"}, "driverKind": {"type": "string"},
            "expiresAt": {"type": "string"}, "tools": {"type": "array", "items": {"type": "string"}},
            "capabilityToken": {"type": "string", "writeOnly": true},
            "mcp": {"type": "object"}
        }
    })
}

pub(crate) fn driver_capabilities_response_schema() -> Value {
    json!({"type": "object", "required": ["tools"], "properties": {"tools": {"type": "array", "items": {"type": "object"}}}})
}

pub(crate) fn capability_call_response_schema() -> Value {
    json!({
        "type": "object", "required": ["content", "isError", "_meta"],
        "properties": {"content": {"type": "array", "items": {"type": "object"}}, "isError": {"type": "boolean"}, "_meta": {"type": "object"}}
    })
}

pub(crate) fn start_driver_response_schema() -> Value {
    json!({
        "type": "object", "required": ["started", "bindingId", "driverKind", "pid", "binaryVersion", "launchEvidence"],
        "properties": {
            "started": {"type": "boolean"}, "bindingId": {"type": "string"}, "driverKind": {"type": "string"}, "pid": {"type": ["integer", "null"]}, "binaryVersion": {"type": "string"},
            "launchEvidence": {
                "type": "object", "required": ["mcpServerCount", "nativeMutationRoutesDisabled", "disabledFeatures"],
                "properties": {"mcpServerCount": {"type": "integer", "const": 1}, "nativeMutationRoutesDisabled": {"type": "boolean"}, "disabledFeatures": {"type": "array", "items": {"type": "string"}}},
                "additionalProperties": false
            }
        }
    })
}

pub(crate) fn driver_request_response_schema() -> Value {
    json!({
        "type": "object", "required": ["requestId"],
        "properties": {"accepted": {"type": "boolean"}, "interrupted": {"type": "boolean"}, "resumed": {"type": "boolean"}, "requestId": {"type": ["string", "number"]}}
    })
}

pub(crate) fn stop_driver_response_schema() -> Value {
    json!({"type": "object", "required": ["stopped"], "properties": {"stopped": {"type": "boolean"}}})
}

pub(crate) fn driver_status_response_schema() -> Value {
    json!({
        "type": "object", "required": ["sessionId", "driverKind", "authRequired", "lifecycle", "drivers"],
        "properties": {
            "sessionId": {"type": "string"},
            "driverKind": {"type": "string", "enum": ["native", "codex", "claude"]},
            "authRequired": {"type": "boolean"},
            "lifecycle": {"type": "string", "enum": ["native", "ready", "authentication_required", "not_resumed"]},
            "auth": {
                "type": ["object", "null"],
                "properties": {
                    "provider": {"type": "string"},
                    "method": {"type": "string", "enum": ["subscription", "api_key", "api_key_env", "oauth_token_env", "ambient_login", "unavailable"]},
                    "accountId": {"type": ["string", "null"]},
                    "label": {"type": ["string", "null"]},
                    "strippedEnv": {"type": "array", "items": {"type": "string"}},
                    "warnings": {"type": "array", "items": {"type": "string"}}
                }
            },
            "drivers": {"type": "array", "items": driver_binding_status_schema()}
        },
        "additionalProperties": false
    })
}

pub(crate) fn driver_version_schema() -> Value {
    json!({
        "type": "object",
        "required": ["executable", "installed", "binaryVersion", "protocolVersion", "schemaHash"],
        "properties": {
            "executable": {"type": "string"},
            "installed": {"type": "boolean"},
            "binaryVersion": {"type": ["string", "null"]},
            "protocolVersion": {"type": "string"},
            "schemaHash": {"type": ["string", "null"]}
        },
        "additionalProperties": false
    })
}

pub(crate) fn driver_capabilities_schema() -> Value {
    json!({
        "type": "object",
        "required": ["transport", "requiresLease", "tools"],
        "properties": {
            "transport": {"const": crate::host_identity::prefixed("mcp")},
            "requiresLease": {"const": true},
            "tools": {"type": "array", "items": {
                "type": "object", "required": ["id", "requiresApproval"],
                "properties": {"id": {"type": "string"}, "requiresApproval": {"type": "boolean"}},
                "additionalProperties": false
            }}
        },
        "additionalProperties": false
    })
}

pub(crate) fn driver_binding_status_schema() -> Value {
    json!({
        "type": "object",
        "required": ["bindingId", "driverKind", "lifecycle", "authRequired", "diagnostic", "failureReason", "capabilities", "version", "vendorSessionId", "vendorTurnId", "running"],
        "properties": {
            "bindingId": {"type": "string"},
            "driverKind": {"type": "string", "enum": ["codex", "claude"]},
            "lifecycle": {"type": "string", "enum": ["leased", "starting", "ready", "running", "waiting_for_approval", "interrupted", "completed", "recovery_required", "failed", "stopped"]},
            "authRequired": {"type": "boolean"},
            "diagnostic": {"type": ["string", "null"]},
            "failureReason": {"type": ["string", "null"], "enum": ["unsupported_schema", "lease_expired", null]},
            "capabilities": driver_capabilities_schema(),
            "version": driver_version_schema(),
            "vendorSessionId": {"type": ["string", "null"]},
            "vendorTurnId": {"type": ["string", "null"]},
            "running": {"type": "boolean"},
            "gatewayState": {"type": "object"},
            "processGeneration": {"type": ["string", "null"]},
            "rateLimit": {"type": "object", "required": ["state"], "properties": {"state": {"type": "string", "enum": ["noData", "probeFailed", "unsupported", "known"]}}}
        },
        "additionalProperties": false
    })
}
