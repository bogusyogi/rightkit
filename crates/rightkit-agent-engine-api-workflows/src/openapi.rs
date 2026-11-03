//! OpenAPI 3.1 schema for the autonomous workflow control plane (WP-09 / ADR-0021).
//!
//! This module emits the full v1 schema as a [`serde_json::Value`] for
//! daemon-side use. The schema describes every public surface listed in
//! `docs/pending/workflow-control-plane.md` § WP-09:
//!
//! * `POST   /workflows` — create workflow definition (and optional version).
//! * `GET    /workflows/{workflow_id}` — fetch latest published version.
//! * `GET    /workflows/{workflow_id}/versions` — list versions.
//! * `POST   /workflows/{workflow_id}/runs` — start a run (gated by
//!   `features.workflows_enabled`).
//! * `GET    /runs/{run_id}` — current run state.
//! * `GET    /runs/{run_id}/journal` — paginated journal.
//! * `GET    /runs/{run_id}/node-states` — node-state projection.
//! * `POST   /runs/{run_id}/cancel` — cancel via harness.
//! * `POST   /runs/{run_id}/approve` — approve a pending approval node.
//! * `POST   /runs/{run_id}/pause` / `/resume` — lifecycle control.
//! * `POST   /runs/{run_id}/events` — server-sent events stream.
//!
//! The schema is intentionally emitted as a `serde_json::Value` rather than
//! a typed struct tree: it is consumed by the daemon's `GET /openapi.json`
//! (wired in WP-11) and by the SDK generator; a typed wrapper would only
//! duplicate what serde already does for free.
//!
//! Style: strict no-`unwrap()`, file header naming WP-09 and ADR-0021.

#![forbid(unsafe_code)]

use serde_json::{json, Map, Value};

/// Build the full OpenAPI 3.1 document for the v1 workflow API.
///
/// The returned `Value` is a JSON object ready to be serialized verbatim via
/// `serde_json::to_string_pretty`.
pub fn openapi_v1_schema() -> Value {
    let mut schemas = Map::new();
    populate_component_schemas(&mut schemas);

    let mut paths = build_paths();

    paths.insert(
        "/openapi.json".into(),
        json!({
                "get": {
                    "tags": ["workflows"],
                    "summary": "Self-describing OpenAPI 3.1 schema for this surface",
                    "operationId": "getWorkflowOpenapi",
                    "responses": {
                        "200": {
                            "description": "OpenAPI 3.1 schema (always served; the workflow feature gate does not gate the schema document).",
                            "content": {
                                "application/json": {
                                    "schema": { "type": "object" }
                                }
                            }
                        }
                    }
                }
            }),
    );

    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Workflow Control Plane API",
            "version": "1.0.0",
            "description": "Autonomous workflow control plane (ADR-0021). Every write endpoint is gated by the workflow feature flag (set `features.workflows_enabled = true` in config to enable); reads are always available but return empty results when the gate is off. Routes call into `RunSupervisor` rather than the pure reducer — see WP-03.",
            "license": {
                "name": "Internal"
            }
        },
        "servers": [
            { "url": "http://127.0.0.1:7101", "description": "Local daemon (default port)" }
        ],
        "tags": [
            { "name": "workflows", "description": "Workflow definitions, versions, and runs" },
            { "name": "runs",      "description": "Run lifecycle, journal, node-states, approvals" },
        ],
        "components": { "schemas": Value::Object(schemas) },
        "paths": Value::Object(paths),
    })
}
fn populate_component_schemas(schemas: &mut Map<String, Value>) {
    // ── Identifiers ──────────────────────────────────────────────────────
    schemas.insert(
        "WorkflowId".into(),
        json!({
            "type": "string",
            "pattern": "^wfd_[A-Za-z0-9_-]+$",
            "description": "Stable workflow definition id (`wfd_…`)."
        }),
    );
    schemas.insert(
        "WorkflowVersionId".into(),
        json!({
            "type": "string",
            "pattern": "^wfv_[A-Za-z0-9_-]+$",
            "description": "Immutable workflow version id (`wfv_…`)."
        }),
    );
    schemas.insert(
        "RunId".into(),
        json!({
            "type": "string",
            "pattern": "^wfr_[A-Za-z0-9_-]+$",
            "description": "Per-run identifier (`wfr_…`)."
        }),
    );

    // ── Enums (string unions) ───────────────────────────────────────────
    schemas.insert(
        "PublicationState".into(),
        json!({
            "type": "string",
            "enum": ["draft", "published", "retired"]
        }),
    );
    schemas.insert(
        "SourceKind".into(),
        json!({
            "type": "string",
            "enum": ["manual", "imported"],
            "description": "Provenance of a workflow definition."
        }),
    );
    schemas.insert(
        "LifecyclePhase".into(),
        json!({
            "type": "string",
            "enum": ["pending", "running", "suspended", "cancelled", "succeeded", "failed", "degraded"],
            "description": "Terminal phases: cancelled, succeeded, failed. Active: pending, running, suspended, degraded."
        }),
    );
    schemas.insert(
        "NodeKind".into(),
        json!({
            "type": "string",
            "enum": [
                "Agent", "Recipe", "Transform", "Condition", "Join",
                "Map", "Loop", "WaitTimer", "WaitEvent", "Approval",
                "Effect", "Subworkflow"
            ]
        }),
    );
    schemas.insert(
        "ApprovalDecision".into(),
        json!({
            "type": "string",
            "enum": ["approved", "denied"]
        }),
    );

    // ── Workflow rows ───────────────────────────────────────────────────
    schemas.insert(
        "WorkflowRow".into(),
        json!({
            "type": "object",
            "required": ["id", "name", "sourceKind", "createdAt", "updatedAt"],
            "properties": {
                "id":          { "$ref": "#/components/schemas/WorkflowId" },
                "name":        { "type": "string" },
                "sourceKind":  { "$ref": "#/components/schemas/SourceKind" },
                "createdAt":   { "type": "string", "format": "date-time" },
                "updatedAt":   { "type": "string", "format": "date-time" }
            }
        }),
    );
    schemas.insert(
        "WorkflowVersionRow".into(),
        json!({
            "type": "object",
            "required": ["id", "workflowId", "versionNumber", "contentHash", "publicationState", "createdAt"],
            "properties": {
                "id":                { "$ref": "#/components/schemas/WorkflowVersionId" },
                "workflowId":        { "$ref": "#/components/schemas/WorkflowId" },
                "versionNumber":     { "type": "integer", "minimum": 1 },
                "contentHash":       { "type": "string", "pattern": "^sha256:[A-Fa-f0-9]{64}$" },
                "definitionJson":    { "type": "string" },
                "compiledPlanJson":  { "type": "string" },
                "publicationState":  { "$ref": "#/components/schemas/PublicationState" },
                "createdAt":         { "type": "string", "format": "date-time" },
                "publishedAt":       { "type": ["string", "null"], "format": "date-time" },
                "createdBy":         { "type": "string" },
                "notes":             { "type": "string" }
            }
        }),
    );
    schemas.insert(
        "RunRow".into(),
        json!({
            "type": "object",
            "required": ["id", "workflowId", "workflowVersionId", "lifecyclePhase", "lifecycleRevision", "startedAt"],
            "properties": {
                "id":                    { "$ref": "#/components/schemas/RunId" },
                "workflowId":            { "$ref": "#/components/schemas/WorkflowId" },
                "workflowVersionId":     { "$ref": "#/components/schemas/WorkflowVersionId" },
                "parentRunId":           { "type": ["string", "null"] },
                "lifecyclePhase":        { "$ref": "#/components/schemas/LifecyclePhase" },
                "lifecycleRevision":     { "type": "integer", "minimum": 0 },
                "initiatedByKind":       { "type": "string" },
                "initiatedByRef":        { "type": ["string", "null"] },
                "triggerPayloadJson":    { "type": ["string", "null"] },
                "inputJson":             { "type": "string" },
                "startedAt":             { "type": "string", "format": "date-time" },
                "terminalAt":            { "type": ["string", "null"], "format": "date-time" },
                "terminalFailureClass":  { "type": ["string", "null"] },
                "runsRootDurable":       { "type": "boolean" }
            }
        }),
    );

    // ── Workflow row DTOs (returned by handlers) ────────────────────────
    schemas.insert(
        "WorkflowRowDto".into(),
        json!({
            "type": "object",
            "required": ["id", "name", "versionId", "contentHash", "publicationState", "createdAt"],
            "properties": {
                "id":              { "$ref": "#/components/schemas/WorkflowId" },
                "name":            { "type": "string" },
                "versionId":       { "$ref": "#/components/schemas/WorkflowVersionId" },
                "contentHash":     { "type": "string", "pattern": "^sha256:[A-Fa-f0-9]{64}$" },
                "publicationState":{ "$ref": "#/components/schemas/PublicationState" },
                "createdAt":       { "type": "string", "format": "date-time" }
            }
        }),
    );
    schemas.insert(
        "RunRowDto".into(),
        json!({
            "type": "object",
            "required": ["id", "lifecyclePhase", "lifecycleRevision"],
            "properties": {
                "id":                { "$ref": "#/components/schemas/RunId" },
                "lifecyclePhase":    { "$ref": "#/components/schemas/LifecyclePhase" },
                "lifecycleRevision": { "type": "integer", "minimum": 0 }
            }
        }),
    );

    // ── Request bodies ──────────────────────────────────────────────────
    schemas.insert(
        "CreateWorkflowRequest".into(),
        json!({
            "type": "object",
            "required": ["name", "sourceKind", "definitionYaml"],
            "properties": {
                "name":             { "type": "string", "minLength": 1 },
                "sourceKind":       { "$ref": "#/components/schemas/SourceKind" },
                "definitionYaml":   { "type": "string", "description": "Canonical YAML for the workflow definition." },
                "compiledPlan":     { "type": ["object", "null"], "description": "Optional pre-compiled plan (skips compile if present)." },
                "publicationState": { "$ref": "#/components/schemas/PublicationState" }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "StartRunRequest".into(),
        json!({
            "type": "object",
            "required": ["versionId", "inputJson"],
            "properties": {
                "requestId":           { "type": ["string", "null"], "description": "UUID v7 for retry dedupe." },
                "versionId":           { "$ref": "#/components/schemas/WorkflowVersionId" },
                "inputJson":           { "type": "string" },
                "triggerPayloadJson":  { "type": ["string", "null"] }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ApprovalRequest".into(),
        json!({
            "type": "object",
            "required": ["nodeId", "decision", "approverRef"],
            "properties": {
                "nodeId":      { "type": "string", "pattern": "^wfn_[A-Za-z0-9_-]+$" },
                "decision":    { "$ref": "#/components/schemas/ApprovalDecision" },
                "approverRef": { "type": "string", "description": "Principal/role/expiry-bound identifier." }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ImportWorkflowRequest".into(),
        json!({
            "type": "object",
            "required": ["definitionYaml"],
            "properties": {
                "definitionYaml": { "type": "string" },
                "name":           { "type": ["string", "null"] }
            },
            "additionalProperties": false
        }),
    );

    // ── Journal + node states ───────────────────────────────────────────
    schemas.insert(
        "JournalEvent".into(),
        json!({
            "type": "object",
            "required": ["runId", "seq", "revision", "eventKind", "createdAt"],
            "properties": {
                "runId":       { "$ref": "#/components/schemas/RunId" },
                "seq":         { "type": "integer", "minimum": 0 },
                "revision":    { "type": "integer", "minimum": 0 },
                "eventKind":   { "type": "string" },
                "nodeId":      { "type": ["string", "null"] },
                "payloadJson":  { "type": "string" },
                "createdAt":   { "type": "string", "format": "date-time" }
            }
        }),
    );
    schemas.insert(
        "JournalPage".into(),
        json!({
            "type": "object",
            "required": ["runId", "afterSeq", "limit", "events", "nextAfterSeq"],
            "properties": {
                "runId":        { "$ref": "#/components/schemas/RunId" },
                "afterSeq":     { "type": "integer", "minimum": 0 },
                "limit":        { "type": "integer", "minimum": 1, "maximum": 1000 },
                "events":       { "type": "array", "items": { "$ref": "#/components/schemas/JournalEvent" } },
                "nextAfterSeq": { "type": "integer", "minimum": 0 }
            }
        }),
    );
    schemas.insert(
        "NodeStateRow".into(),
        json!({
            "type": "object",
            "required": ["runId", "nodeId", "phase", "attempt", "lastReducerRevision", "lastEventSeq", "lastUpdatedAt"],
            "properties": {
                "runId":              { "$ref": "#/components/schemas/RunId" },
                "nodeId":             { "type": "string", "pattern": "^wfn_[A-Za-z0-9_-]+$" },
                "phase":              { "type": "string" },
                "attempt":            { "type": "integer", "minimum": 0 },
                "leaseHolder":        { "type": ["string", "null"] },
                "leaseExpiresAt":     { "type": ["string", "null"], "format": "date-time" },
                "lastReducerRevision":{ "type": "integer", "minimum": 0 },
                "lastEventSeq":       { "type": "integer", "minimum": 0 },
                "lastFailureClass":   { "type": ["string", "null"] },
                "lastUpdatedAt":      { "type": "string", "format": "date-time" }
            }
        }),
    );
    schemas.insert(
        "NodeStatesPage".into(),
        json!({
            "type": "object",
            "required": ["runId", "states"],
            "properties": {
                "runId":  { "$ref": "#/components/schemas/RunId" },
                "states": { "type": "array", "items": { "$ref": "#/components/schemas/NodeStateRow" } }
            }
        }),
    );

    // ── SSE events ──────────────────────────────────────────────────────
    schemas.insert(
        "SseEnvelope".into(),
        json!({
            "type": "object",
            "description": "Discriminated union for SSE event payloads.",
            "oneOf": [
                { "$ref": "#/components/schemas/JournalAppendedEvent" },
                { "$ref": "#/components/schemas/PhaseChangedEvent" },
                { "$ref": "#/components/schemas/NodeStateChangedEvent" },
                { "$ref": "#/components/schemas/TerminalEvent" }
            ],
            "discriminator": {
                "propertyName": "kind",
                "mapping": {
                    "journal_appended":   "#/components/schemas/JournalAppendedEvent",
                    "phase_changed":      "#/components/schemas/PhaseChangedEvent",
                    "node_state_changed": "#/components/schemas/NodeStateChangedEvent",
                    "terminal":           "#/components/schemas/TerminalEvent"
                }
            }
        }),
    );
    schemas.insert(
        "JournalAppendedEvent".into(),
        json!({
            "type": "object",
            "required": ["kind", "seq", "revision", "eventKind"],
            "properties": {
                "kind":     { "type": "string", "enum": ["journal_appended"] },
                "seq":      { "type": "integer", "minimum": 0 },
                "revision": { "type": "integer", "minimum": 0 },
                "eventKind":{ "type": "string" },
                "nodeId":   { "type": ["string", "null"] }
            }
        }),
    );
    schemas.insert(
        "PhaseChangedEvent".into(),
        json!({
            "type": "object",
            "required": ["kind", "from", "to", "revision"],
            "properties": {
                "kind":     { "type": "string", "enum": ["phase_changed"] },
                "from":     { "$ref": "#/components/schemas/LifecyclePhase" },
                "to":       { "$ref": "#/components/schemas/LifecyclePhase" },
                "revision": { "type": "integer", "minimum": 0 }
            }
        }),
    );
    schemas.insert(
        "NodeStateChangedEvent".into(),
        json!({
            "type": "object",
            "required": ["kind", "nodeId", "phase", "revision"],
            "properties": {
                "kind":     { "type": "string", "enum": ["node_state_changed"] },
                "nodeId":   { "type": "string" },
                "phase":    { "type": "string" },
                "revision": { "type": "integer", "minimum": 0 }
            }
        }),
    );
    schemas.insert(
        "TerminalEvent".into(),
        json!({
            "type": "object",
            "required": ["kind", "phase", "revision"],
            "properties": {
                "kind":     { "type": "string", "enum": ["terminal"] },
                "phase":    { "$ref": "#/components/schemas/LifecyclePhase" },
                "revision": { "type": "integer", "minimum": 0 },
                "failureClass": { "type": ["string", "null"] }
            }
        }),
    );

    // ── Generic error body ──────────────────────────────────────────────
    schemas.insert(
        "ApiError".into(),
        json!({
            "type": "object",
            "required": ["error", "code"],
            "properties": {
                "error":      { "type": "string" },
                "code":       { "type": "string", "description": "Machine-readable error code." },
                "requestId":  { "type": ["string", "null"], "description": "Echoed from the request, when available." }
            },
            "additionalProperties": false
        }),
    );
}
fn build_paths() -> Map<String, Value> {
    let mut paths = Map::new();

    // ── Workflows ───────────────────────────────────────────────────────
    paths.insert(
        "/workflows".into(),
        json!({
            "post": {
                "tags": ["workflows"],
                "summary": "Create workflow definition",
                "operationId": "createWorkflow",
                "requestBody": {
                    "required": true,
                    "content": {
                        "application/json": {
                            "schema": { "$ref": "#/components/schemas/CreateWorkflowRequest" }
                        }
                    }
                },
                "responses": {
                    "201": {
                        "description": "Workflow created.",
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/WorkflowRowDto" }
                            }
                        }
                    },
                    "400": error_response(),
                    "409": error_response(),
                    "423": error_response()
                }
            }
        }),
    );

    paths.insert(
        "/workflows/{workflow_id}".into(),
        json!({
            "get": {
                "tags": ["workflows"],
                "summary": "Fetch the latest published version of a workflow",
                "operationId": "getWorkflow",
                "parameters": [path_param("workflow_id", "WorkflowId")],
                "responses": {
                    "200": json_response_ref("WorkflowRow"),
                    "404": error_response()
                }
            }
        }),
    );

    paths.insert(
        "/workflows/{workflow_id}/versions".into(),
        json!({
            "get": {
                "tags": ["workflows"],
                "summary": "List all versions of a workflow (newest first)",
                "operationId": "listWorkflowVersions",
                "parameters": [path_param("workflow_id", "WorkflowId")],
                "responses": {
                    "200": {
                        "description": "Version list.",
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "array",
                                    "items": { "$ref": "#/components/schemas/WorkflowVersionRow" }
                                }
                            }
                        }
                    },
                    "404": error_response()
                }
            }
        }),
    );

    for (suffix, operation_id, summary) in [
        (
            "validate",
            "validateWorkflowVersion",
            "Validate a workflow version",
        ),
        (
            "publish",
            "publishWorkflowVersion",
            "Publish a validated workflow version",
        ),
    ] {
        paths.insert(format!("/workflows/{{workflow_id}}/versions/{{version_id}}/{suffix}"), json!({
            "post": { "tags": ["workflows"], "summary": summary, "operationId": operation_id,
                "parameters": [path_param("workflow_id", "WorkflowId"), path_param("version_id", "WorkflowVersionId")],
                "responses": { "200": {"description": "Workflow version result.", "content": {"application/json": {"schema": {"type": "object"}}}}, "404": error_response(), "423": locked_response() }
            }
        }));
    }

    paths.insert("/workflow-inbox".into(), json!({
        "get": { "tags": ["runs"], "summary": "Open workflow review inbox", "operationId": "listWorkflowInbox",
          "responses": { "200": {"description": "Open attention items.", "content": {"application/json": {"schema": {"type": "object"}}}}, "423": locked_response() } }
    }));
    paths.insert("/workflow-inbox/{id}/decision".into(), json!({
        "post": { "tags": ["runs"], "summary": "Decide a workflow approval inbox item", "operationId": "decideWorkflowInboxItem",
          "parameters": [path_param("id", "WorkflowId")], "responses": { "200": {"description": "Decision recorded.", "content": {"application/json": {"schema": {"type": "object"}}}}, "400": error_response(), "404": error_response(), "409": error_response() } }
    }));
    paths.insert("/workflow-imports".into(), json!({
        "get": {"tags":["workflows"],"summary":"List disabled workflow import quarantine records","operationId":"listWorkflowImports","responses":{"200":{"description":"Quarantine records."},"423":locked_response()}},
        "post": {"tags":["workflows"],"summary":"Submit signed workflow package into disabled quarantine","operationId":"submitWorkflowImport","responses":{"202":{"description":"Persisted disabled quarantine record."},"400":error_response(),"423":locked_response()}}
    }));
    paths.insert("/workflow-imports/{id}/review".into(), json!({
        "post": {"tags":["workflows"],"summary":"Record import review without activation","operationId":"reviewWorkflowImport","parameters":[path_param("id", "WorkflowId")],"responses":{"200":{"description":"Review receipt; import remains disabled."},"404":error_response(),"423":locked_response()}}
    }));
    paths.insert("/workflow-optimizations".into(), json!({
        "get": {"tags":["workflows"],"summary":"List optimization promotion receipts","operationId":"listWorkflowOptimizations","responses":{"200":{"description":"Promotion receipts."},"423":locked_response()}},
        "post": {"tags":["workflows"],"summary":"Create immutable workflow optimization proposal","operationId":"createWorkflowOptimization","requestBody":{"required":true,"content":{"application/json":{"schema":{"type":"object","required":["workflowId","baseVersionId","candidateDefinition","changeKind","diffSummary","proposedBy","benchmarkId","evidence"]}}}},"responses":{"201":{"description":"Proposal and evaluation receipt persisted."},"400":error_response(),"409":error_response(),"423":locked_response()}}
    }));
    for (suffix, operation_id, summary) in [
        (
            "promote",
            "promoteWorkflowOptimization",
            "Review & promote optimization",
        ),
        (
            "monitor",
            "monitorWorkflowOptimization",
            "Record optimization monitoring",
        ),
        (
            "rollback",
            "rollbackWorkflowOptimization",
            "Roll back future optimization admission",
        ),
    ] {
        paths.insert(format!("/workflow-optimizations/{{id}}/{suffix}"), json!({"post":{"tags":["workflows"],"summary":summary,"operationId":operation_id,"parameters":[path_param("id", "WorkflowId")],"responses":{"200":{"description":"Optimization receipt updated."},"404":error_response(),"423":locked_response()}}}));
    }
    paths.insert("/workflow-interop/mcp/tasks".into(), json!({
        "get":{"tags":["runs"],"summary":"List MCP Task projections","operationId":"listWorkflowMcpTasks","responses":{"200":{"description":"Task projections."},"423":locked_response()}},
        "post":{"tags":["runs"],"summary":"Create MCP Task via workflow admission","operationId":"createWorkflowMcpTask","requestBody":{"required":true,"content":{"application/json":{"schema":{"type":"object","required":["workflowId"]}}}},"responses":{"201":{"description":"Task admitted."},"400":error_response(),"423":locked_response()}}
    }));
    paths.insert("/workflow-interop/mcp/tasks/{id}".into(), json!({
        "get":{"tags":["runs"],"summary":"Get MCP Task projection","operationId":"getWorkflowMcpTask","parameters":[path_param("id", "RunId")],"responses":{"200":{"description":"Task projection."},"404":error_response(),"423":locked_response()}},
        "patch":{"tags":["runs"],"summary":"Update MCP Task lifecycle","operationId":"updateWorkflowMcpTask","parameters":[path_param("id", "RunId")],"responses":{"200":{"description":"Task updated."},"400":error_response(),"404":error_response(),"423":locked_response()}},
        "delete":{"tags":["runs"],"summary":"Cancel MCP Task","operationId":"cancelWorkflowMcpTask","parameters":[path_param("id", "RunId")],"responses":{"200":{"description":"Task cancelled."},"404":error_response(),"423":locked_response()}}
    }));

    paths.insert(
        "/workflows/{workflow_id}/runs".into(),
        json!({
            "post": {
                "tags": ["workflows"],
                "summary": "Start a new run against a pinned version (feature-gated)",
                "operationId": "startRun",
                "parameters": [path_param("workflow_id", "WorkflowId")],
                "requestBody": {
                    "required": true,
                    "content": {
                        "application/json": {
                            "schema": { "$ref": "#/components/schemas/StartRunRequest" }
                        }
                    }
                },
                "responses": {
                    "201": json_response_ref("RunRowDto"),
                    "400": error_response(),
                    "404": error_response(),
                    "423": locked_response(),
                    "503": error_response()
                }
            }
        }),
    );

    for (path, operation_id, summary) in [
        (
            "/workflows/{workflow_id}/triggers",
            "listWorkflowTriggers",
            "List workflow triggers",
        ),
        (
            "/runs/{run_id}/attempts",
            "listWorkflowAttempts",
            "List run attempts",
        ),
        (
            "/runs/{run_id}/evidence",
            "listWorkflowEvidence",
            "List run evidence",
        ),
        (
            "/runs/{run_id}/effects",
            "listWorkflowEffects",
            "List run effects",
        ),
        ("/approvals/{id}", "getWorkflowApproval", "Get approval"),
        ("/effects/{id}", "getWorkflowEffect", "Get effect"),
        ("/attempts/{id}", "getWorkflowAttempt", "Get attempt"),
    ] {
        let parameter = if path.contains("workflow_id") {
            path_param("workflow_id", "WorkflowId")
        } else if path.contains("{id}") {
            path_param("id", "RunId")
        } else {
            path_param("run_id", "RunId")
        };
        paths.insert(path.into(), json!({"get": {"tags": ["runs"], "summary": summary, "operationId": operation_id, "parameters": [parameter], "responses": {"200": {"description": "Projection rows.", "content": {"application/json": {"schema": {"type": "object"}}}}, "404": error_response(), "423": locked_response()}}}));
    }

    // ── Runs ────────────────────────────────────────────────────────────
    paths.insert(
        "/runs/{run_id}".into(),
        json!({
            "get": {
                "tags": ["runs"],
                "summary": "Current state of a run",
                "operationId": "getRun",
                "parameters": [path_param("run_id", "RunId")],
                "responses": {
                    "200": json_response_ref("RunRow"),
                    "404": error_response()
                }
            }
        }),
    );

    paths.insert(
        "/runs/{run_id}/journal".into(),
        json!({
            "get": {
                "tags": ["runs"],
                "summary": "Paginated journal events (after a cursor seq)",
                "operationId": "getRunJournal",
                "parameters": [
                    path_param("run_id", "RunId"),
                    {
                        "name": "afterSeq",
                        "in": "query",
                        "required": false,
                        "schema": { "type": "integer", "minimum": 0, "default": 0 }
                    },
                    {
                        "name": "limit",
                        "in": "query",
                        "required": false,
                        "schema": { "type": "integer", "minimum": 1, "maximum": 1000, "default": 200 }
                    }
                ],
                "responses": {
                    "200": json_response_ref("JournalPage"),
                    "404": error_response()
                }
            }
        }),
    );

    paths.insert(
        "/runs/{run_id}/node-states".into(),
        json!({
            "get": {
                "tags": ["runs"],
                "summary": "Per-node lifecycle projection for a run",
                "operationId": "getRunNodeStates",
                "parameters": [path_param("run_id", "RunId")],
                "responses": {
                    "200": json_response_ref("NodeStatesPage"),
                    "404": error_response()
                }
            }
        }),
    );

    paths.insert(
        "/runs/{run_id}/cancel".into(),
        json!({
            "post": {
                "tags": ["runs"],
                "summary": "Cancel a run via `RunSupervisor::release_run(..., terminal=None)`",
                "operationId": "cancelRun",
                "parameters": [path_param("run_id", "RunId")],
                "responses": {
                    "200": json_response_ref("RunRow"),
                    "404": error_response(),
                    "423": locked_response(),
                    "503": error_response()
                }
            }
        }),
    );

    paths.insert(
        "/runs/{run_id}/approve".into(),
        json!({
            "post": {
                "tags": ["runs"],
                "summary": "Approve a pending approval node",
                "operationId": "approveRunNode",
                "parameters": [path_param("run_id", "RunId")],
                "requestBody": {
                    "required": true,
                    "content": {
                        "application/json": {
                            "schema": { "$ref": "#/components/schemas/ApprovalRequest" }
                        }
                    }
                },
                "responses": {
                    "200": json_response_ref("RunRow"),
                    "400": error_response(),
                    "404": error_response(),
                    "423": locked_response(),
                    "503": error_response()
                }
            }
        }),
    );

    paths.insert(
        "/runs/{run_id}/pause".into(),
        json!({
            "post": {
                "tags": ["runs"],
                "summary": "Pause an active run",
                "operationId": "pauseRun",
                "parameters": [path_param("run_id", "RunId")],
                "responses": {
                    "200": json_response_ref("RunRow"),
                    "404": error_response(),
                    "423": locked_response(),
                    "503": error_response()
                }
            }
        }),
    );

    paths.insert(
        "/runs/{run_id}/resume".into(),
        json!({
            "post": {
                "tags": ["runs"],
                "summary": "Resume a suspended run",
                "operationId": "resumeRun",
                "parameters": [path_param("run_id", "RunId")],
                "responses": {
                    "200": json_response_ref("RunRow"),
                    "404": error_response(),
                    "423": locked_response(),
                    "503": error_response()
                }
            }
        }),
    );

    paths.insert(
        "/runs/{run_id}/events".into(),
        json!({
            "get": {
                "tags": ["runs"],
                "summary": "Server-sent events stream for a run (15s heartbeat)",
                "operationId": "streamRunEvents",
                "parameters": [path_param("run_id", "RunId")],
                "responses": {
                    "200": {
                        "description": "SSE stream. `data:` lines are JSON envelopes from `SseEnvelope`; an empty heartbeat is sent every 15s.",
                        "content": {
                            "text/event-stream": {
                                "schema": { "$ref": "#/components/schemas/SseEnvelope" }
                            }
                        }
                    },
                    "404": error_response(),
                    "423": locked_response()
                }
            }
        }),
    );

    paths
}

fn path_param(name: &str, schema_ref: &str) -> Value {
    json!({
        "name": name,
        "in": "path",
        "required": true,
        "schema": { "$ref": format!("#/components/schemas/{schema_ref}") }
    })
}

fn json_response_ref(schema_name: &str) -> Value {
    json!({
        "description": "OK",
        "content": {
            "application/json": {
                "schema": { "$ref": format!("#/components/schemas/{schema_name}") }
            }
        }
    })
}

fn error_response() -> Value {
    json!({
        "description": "Error",
        "content": {
            "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
            }
        }
    })
}

fn locked_response() -> Value {
    json!({
        "description": "Workflow feature gate disabled — see `features.workflows_enabled` in config.",
        "content": {
            "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
            }
        }
    })
}
