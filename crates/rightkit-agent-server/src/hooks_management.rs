// Operator-facing endpoints for managing the env-var-driven hook runner:
//
// - `GET  /hooks/list`    — every env-var key wired to a hook, with
//                           `configured` and `revoked` markers.
// - `POST /hooks/test`    — dry-run matching hooks against a synthetic
//                           event. Returns the synthesized Proceed / Deny /
//                           Error outcome without publishing on the bus.
// - `POST /hooks/revoke`  — record / clear a hook id (env-var name) in the
//                           daemon-wide revocation registry. The shared
//                           registry is also consulted by `spawn_hook_runner*`
//                           so subsequent events skip revoked hooks.
//
// Composed onto `hook_routes()` via `include!` from `hooks.rs`.

// `HookRunnerConfig`, `hook_status_from_config`, and the axum/serde/serde_json/
// `Arc`/`HookBus`/`HookEvent`/`HookRegistry` imports are already in scope from
// `hooks.rs`, which `include!`s this file into its own module. Only the symbols
// new to this file are imported here.
use axum::routing::post;
use rightkit_hooks::agent_hooks::{HookDryRunReport, HookOutcome};
use serde::Deserialize;
use std::sync::OnceLock;

/// Daemon-wide hook revocation registry. Initialised lazily on first access
/// and swappable from tests via [`replace_shared_registry`].
pub(crate) static REGISTRY: OnceLock<std::sync::Mutex<Arc<HookRegistry>>> = OnceLock::new();

/// Return a clone of the daemon-wide registry, initialising on first use.
pub(crate) fn shared_registry() -> Arc<HookRegistry> {
    REGISTRY
        .get_or_init(|| std::sync::Mutex::new(Arc::new(HookRegistry::default())))
        .lock()
        .expect("REGISTRY mutex poisoned")
        .clone()
}

/// Mount the operator-facing endpoints onto an Axum `Router`.
pub(crate) fn hook_management_routes() -> Router<crate::api::DaemonState> {
    Router::new()
        .route("/hooks/list", get(list_hooks_handler))
        .route("/hooks/test", post(test_hooks_handler))
        .route("/hooks/revoke", post(revoke_hook_handler))
}

// ── List ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HookListItem {
    id: String,
    event: String,
    kind: String,
    /// Reserved for future matcher language; the env-var model has no
    /// matcher pattern, so this is currently `null`.
    matcher: Option<String>,
    /// Env-var name backing this hook — surfaced as the canonical id.
    source: String,
    configured: bool,
    revoked: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HookListResponse {
    total: usize,
    configured: usize,
    revoked: usize,
    hooks: Vec<HookListItem>,
}

async fn list_hooks_handler() -> Json<HookListResponse> {
    let config = HookRunnerConfig::from_env();
    let registry = shared_registry();
    let hooks = collect_list_items(&config, &registry);
    let configured = hooks
        .iter()
        .filter(|item| item.configured && !item.revoked)
        .count();
    let revoked = hooks.iter().filter(|item| item.revoked).count();
    Json(HookListResponse {
        total: hooks.len(),
        configured,
        revoked,
        hooks,
    })
}

fn collect_list_items(config: &HookRunnerConfig, registry: &HookRegistry) -> Vec<HookListItem> {
    hook_status_from_config(config)
        .hooks
        .into_iter()
        .map(|entry| {
            let env_var = entry.env_var.to_string();
            let revoked = registry.is_revoked(&env_var);
            HookListItem {
                id: env_var.clone(),
                event: entry.event.to_string(),
                kind: entry.kind.to_string(),
                matcher: None,
                source: env_var,
                configured: entry.configured,
                revoked,
            }
        })
        .collect()
}

// ── Test (dry-run) ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HookTestRequest {
    event: String,
    #[serde(default)]
    matcher: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HookTestResponse {
    event: String,
    matcher: Option<String>,
    ran: bool,
    no_match: bool,
    outcome: &'static str,
    message: Option<String>,
    invoked: Vec<String>,
    revoked: Vec<String>,
}

async fn test_hooks_handler(Json(req): Json<HookTestRequest>) -> Json<HookTestResponse> {
    let matcher = req.matcher.as_deref().filter(|m| !m.is_empty());
    let workspace_root = std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .display()
        .to_string();
    let context = rightkit_sandbox::execution::ProcessLaunchContext {
        workspace_root,
        sandbox_profile: None,
        sandbox_required: false,
        network_required: false,
    };
    let adapter = rightkit_agent_tools::coding_tools::shell::NativeCommandSandboxAdapter;
    let report: HookDryRunReport = rightkit_hooks::agent_hooks::dry_run_hook_with_adapter(
        &req.event,
        matcher,
        Some(&shared_registry()),
        &adapter,
        &context,
    )
    .await;
    let (outcome, message) = match report.outcome {
        HookOutcome::Proceed => ("Proceed", None),
        HookOutcome::Deny(msg) => ("Deny", Some(msg)),
        HookOutcome::Error(msg) => ("Error", Some(msg)),
    };
    Json(HookTestResponse {
        event: req.event,
        matcher: matcher.map(str::to_string),
        ran: report.ran,
        no_match: report.no_match,
        outcome,
        message,
        invoked: report.invoked,
        revoked: report.revoked,
    })
}

// ── Revoke ───────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HookRevokeRequest {
    id: String,
    /// When `true`, remove a prior revocation instead of adding one.
    #[serde(default)]
    restore: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HookRevokeResponse {
    id: String,
    /// `true` if the registry state changed (a new revocation was added, or a
    /// prior one was cleared).
    applied: bool,
    /// `true` if the id is revoked after the operation.
    revoked: bool,
    snapshot: Vec<String>,
}

async fn revoke_hook_handler(Json(req): Json<HookRevokeRequest>) -> Json<HookRevokeResponse> {
    let registry = shared_registry();
    let applied = if req.restore {
        registry.unrevoke(&req.id)
    } else {
        registry.revoke(&req.id)
    };
    let revoked = registry.is_revoked(&req.id);
    Json(HookRevokeResponse {
        id: req.id,
        applied,
        revoked,
        snapshot: registry.snapshot(),
    })
}

// ── OpenAPI components ───────────────────────────────────────────────────────

pub(crate) fn hook_management_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    schemas.insert(
        "HookListItem".to_string(),
        json!({
            "type": "object",
            "required": ["id", "event", "kind", "matcher", "source", "configured", "revoked"],
            "properties": {
                "id": {"type": "string"},
                "event": {"type": "string"},
                "kind": {"type": "string", "enum": ["command", "url", "mcp"]},
                "matcher": {"type": ["string", "null"]},
                "source": {"type": "string"},
                "configured": {"type": "boolean"},
                "revoked": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "HookListResponse".to_string(),
        json!({
            "type": "object",
            "required": ["total", "configured", "revoked", "hooks"],
            "properties": {
                "total": {"type": "integer", "minimum": 0},
                "configured": {"type": "integer", "minimum": 0},
                "revoked": {"type": "integer", "minimum": 0},
                "hooks": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/HookListItem"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "HookTestRequest".to_string(),
        json!({
            "type": "object",
            "required": ["event"],
            "properties": {
                "event": {"type": "string"},
                "matcher": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "HookTestResponse".to_string(),
        json!({
            "type": "object",
            "required": [
                "event",
                "matcher",
                "ran",
                "noMatch",
                "outcome",
                "invoked",
                "revoked"
            ],
            "properties": {
                "event": {"type": "string"},
                "matcher": {"type": ["string", "null"]},
                "ran": {"type": "boolean"},
                "noMatch": {"type": "boolean"},
                "outcome": {"type": "string", "enum": ["Proceed", "Deny", "Error"]},
                "message": {"type": ["string", "null"]},
                "invoked": {"type": "array", "items": {"type": "string"}},
                "revoked": {"type": "array", "items": {"type": "string"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "HookRevokeRequest".to_string(),
        json!({
            "type": "object",
            "required": ["id"],
            "properties": {
                "id": {"type": "string"},
                "restore": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "HookRevokeResponse".to_string(),
        json!({
            "type": "object",
            "required": ["id", "applied", "revoked", "snapshot"],
            "properties": {
                "id": {"type": "string"},
                "applied": {"type": "boolean"},
                "revoked": {"type": "boolean"},
                "snapshot": {"type": "array", "items": {"type": "string"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "HookTestRequest".to_string(),
        json!({
            "type": "object",
            "required": ["event"],
            "properties": {
                "event": {"type": "string"},
                "matcher": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "HookRevokeRequest".to_string(),
        json!({
            "type": "object",
            "required": ["id"],
            "properties": {
                "id": {"type": "string"},
                "restore": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas
}

pub(crate) fn hook_list_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/HookListResponse"})
}
pub(crate) fn hook_test_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/HookTestResponse"})
}
pub(crate) fn hook_revoke_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/HookRevokeResponse"})
}
pub(crate) fn hook_test_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/HookTestRequest"})
}
pub(crate) fn hook_revoke_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/HookRevokeRequest"})
}

// ── Tests ────────────────────────────────────────────────────────────────────
