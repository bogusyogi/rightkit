// Workspace module — OpenAPI component/request/response schemas.
//
// Extracted from `api_workspace.rs` (ra-019 decomposition). Pure structural
// move: schema shapes and the request/response lookup tables are
// byte-identical to the pre-split version; `api_contract/document.rs`,
// `request_schemas.rs`, and `response_schemas.rs` call these three
// functions through the `crate::api_workspace::` facade unchanged.

use serde_json::{json, Map, Value};

pub(crate) fn workspace_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    schemas.insert(
        "WorkspaceEntry".to_string(),
        json!({
            "type": "object",
            "required": ["name", "path", "kind"],
            "properties": {
                "name": {"type": "string"},
                "path": {"type": "string"},
                "kind": {"type": "string", "enum": ["directory", "file"]},
                "suffix": {"type": ["string", "null"]},
                "sizeBytes": {"type": ["integer", "null"], "minimum": 0, "format": "int64"},
                "modifiedAt": {"type": ["string", "null"]},
                "gitStatus": {"type": ["string", "null"]},
                "childCount": {"type": ["integer", "null"], "minimum": 0},
                "truncated": {"type": ["boolean", "null"]},
                "lastKnown": {"type": ["boolean", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "WorkspaceTreeResponse".to_string(),
        json!({
            "type": "object",
            "required": ["workspaceRoot", "path", "entries", "truncated"],
            "properties": {
                "workspaceRoot": {"type": "string"},
                "path": {"type": "string"},
                "entries": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/WorkspaceEntry"}
                },
                "truncated": {"type": "boolean"},
                "lastKnown": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "WorkspaceFileResponse".to_string(),
        json!({
            "type": "object",
            "required": ["workspaceRoot", "path", "kind", "sizeBytes", "modifiedAt", "fingerprint", "permission"],
            "properties": {
                "workspaceRoot": {"type": "string"},
                "path": {"type": "string"},
                "kind": {"type": "string", "enum": ["file", "missing", "binary", "directory", "tooLarge"]},
                "sizeBytes": {"type": "integer", "minimum": 0, "format": "int64"},
                "modifiedAt": {"type": ["string", "null"]},
                "fingerprint": {"type": ["string", "null"], "description": "SHA-256 hex of the file content at read time"},
                "content": {"type": ["string", "null"], "description": "UTF-8 content when kind=file; null for binary/tooLarge/missing"},
                "permission": {"type": "string", "description": "POSIX mode bits in octal, e.g. 0644"},
                "truncated": {"type": "boolean"},
                "reason": {"type": ["string", "null"], "description": "Human-readable policy reason for binary/tooLarge/permission denials"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "WorkspaceFileSaveRequest".to_string(),
        json!({
            "type": "object",
            "required": ["workspaceRoot", "path", "content", "expectedFingerprint"],
            "properties": {
                "workspaceRoot": {"type": "string"},
                "path": {"type": "string"},
                "content": {"type": "string", "maxLength": 1048576},
                "expectedFingerprint": {"type": "string", "description": "SHA-256 hex the client observed; mismatch → 409 stale buffer"},
                "baseRevision": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "WorkspaceFileSaveResponse".to_string(),
        json!({
            "type": "object",
            "required": ["ok", "path", "fingerprint", "sizeBytes"],
            "properties": {
                "ok": {"type": "boolean"},
                "path": {"type": "string"},
                "fingerprint": {"type": "string"},
                "sizeBytes": {"type": "integer", "minimum": 0, "format": "int64"},
                "modifiedAt": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "WorkspaceSearchRequest".to_string(),
        json!({
            "type": "object",
            "required": ["workspaceRoot", "query"],
            "properties": {
                "workspaceRoot": {"type": "string"},
                "query": {"type": "string", "minLength": 1, "maxLength": 256},
                "requestId": {"type": ["string", "null"], "description": "Cancellation token; reuse to cancel an in-flight search"},
                "kind": {"type": ["string", "null"], "enum": ["file", "content", null]},
                "limit": {"type": ["integer", "null"], "minimum": 1, "maximum": 256},
                "includeBinary": {"type": ["boolean", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "WorkspaceSearchHit".to_string(),
        json!({
            "type": "object",
            "required": ["path", "name", "line", "column", "snippet"],
            "properties": {
                "path": {"type": "string"},
                "name": {"type": "string"},
                "line": {"type": "integer", "minimum": 0},
                "column": {"type": "integer", "minimum": 0},
                "snippet": {"type": "string"},
                "kind": {"type": "string", "enum": ["file", "content"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "WorkspaceSearchResponse".to_string(),
        json!({
            "type": "object",
            "required": ["workspaceRoot", "query", "hits", "cancelled", "scannedBytes"],
            "properties": {
                "workspaceRoot": {"type": "string"},
                "query": {"type": "string"},
                "requestId": {"type": ["string", "null"]},
                "hits": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/WorkspaceSearchHit"}
                },
                "cancelled": {"type": "boolean"},
                "scannedBytes": {"type": "integer", "minimum": 0, "format": "int64"},
                "truncated": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "WorkspaceArtifact".to_string(),
        json!({
            "type": "object",
            "required": ["id", "kind", "path", "fingerprint"],
            "properties": {
                "id": {"type": "string"},
                "kind": {"type": "string"},
                "path": {"type": "string"},
                "title": {"type": ["string", "null"]},
                "provenance": {"type": ["string", "null"]},
                "verdict": {"type": ["string", "null"]},
                "fingerprint": {"type": "string"},
                "sizeBytes": {"type": ["integer", "null"], "minimum": 0, "format": "int64"},
                "createdAt": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "WorkspaceArtifactsResponse".to_string(),
        json!({
            "type": "object",
            "required": ["workspaceRoot", "artifacts"],
            "properties": {
                "workspaceRoot": {"type": "string"},
                "artifacts": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/WorkspaceArtifact"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "WorkspaceRecentResponse".to_string(),
        json!({
            "type": "object",
            "required": ["workspaceRoot", "recents"],
            "properties": {
                "workspaceRoot": {"type": "string"},
                "recents": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["path", "name", "openedAt"],
                        "properties": {
                            "path": {"type": "string"},
                            "name": {"type": "string"},
                            "openedAt": {"type": "string"}
                        },
                        "additionalProperties": false
                    }
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "WorkspaceWatchEvent".to_string(),
        json!({
            "type": "object",
            "required": ["kind", "path", "at"],
            "properties": {
                "kind": {"type": "string", "enum": ["created", "modified", "removed", "rescan"]},
                "path": {"type": "string"},
                "at": {"type": "string"},
                "fingerprint": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas
}

pub(crate) fn workspace_openapi_request_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("put", "/workspace/file") => {
            Some(json!({"$ref": "#/components/schemas/WorkspaceFileSaveRequest"}))
        }
        ("post", "/workspace/search") => {
            Some(json!({"$ref": "#/components/schemas/WorkspaceSearchRequest"}))
        }
        ("post", "/workspace/tree/expand") => Some(json!({
            "type": "object",
            "required": ["workspaceRoot"],
            "properties": {
                "workspaceRoot": {"type": "string"},
                "path": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        })),
        _ => None,
    }
}

pub(crate) fn workspace_openapi_response_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("get", "/workspace/tree") | ("post", "/workspace/tree/expand") => {
            Some(json!({"$ref": "#/components/schemas/WorkspaceTreeResponse"}))
        }
        ("get", "/workspace/file") | ("put", "/workspace/file") => {
            Some(json!({"$ref": "#/components/schemas/WorkspaceFileResponse"}))
        }
        ("post", "/workspace/search") => {
            Some(json!({"$ref": "#/components/schemas/WorkspaceSearchResponse"}))
        }
        ("get", "/workspace/artifacts") => {
            Some(json!({"$ref": "#/components/schemas/WorkspaceArtifactsResponse"}))
        }
        ("get", "/workspace/recent") => {
            Some(json!({"$ref": "#/components/schemas/WorkspaceRecentResponse"}))
        }
        _ => None,
    }
}
