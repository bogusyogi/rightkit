// Operator-facing endpoints for managing configured hooks (tools rule 58):
//
// - `GET  /hooks/list`    — the event taxonomy and every configured hook, with its id
//                           (`<Event>#<n>`), origin and `revoked` marker.
// - `POST /hooks/test`    — dry-run the command hooks of an event. Returns the synthesized
//                           Proceed / Deny / Error outcome without publishing on the bus.
// - `POST /hooks/revoke`  — record / clear a hook in the daemon-wide revocation registry.
//                           The positional id is resolved, against the request's `workspace`
//                           (default: the daemon's cwd), to the hook's stable identity
//                           (`hook_revocation_key`), and that is what is stored, so later
//                           reordering of the hook file cannot move the revocation (audit M4).
//                           The shared registry is also consulted by `spawn_hook_runner*`
//                           so subsequent events skip revoked hooks.
//
// Composed onto `hook_routes()` via `include!` from `hooks.rs`.

// `HookRunnerConfig`, `daemon_wide_bus`, and the axum/serde/serde_json/
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
    /// The hook's own tool-name matcher, when it has one.
    matcher: Option<String>,
    /// Where the hook is configured: `config` (the `[hooks]` key), `project` (the project hook
    /// file) or `env-json` (`<PREFIX>_HOOKS_JSON`).
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
    /// Every event a hook may be configured for (`rightkit_hooks::agent_hooks::chain::HOOK_EVENT_NAMES`).
    events: Vec<String>,
    hooks: Vec<HookListItem>,
}

async fn list_hooks_handler() -> Json<HookListResponse> {
    let registry = shared_registry();
    let hooks = collect_list_items(&daemon_wide_bus(), &registry, &daemon_cwd_scope());
    let configured = hooks
        .iter()
        .filter(|item| item.configured && !item.revoked)
        .count();
    let revoked = hooks.iter().filter(|item| item.revoked).count();
    Json(HookListResponse {
        total: hooks.len(),
        configured,
        revoked,
        events: rightkit_hooks::agent_hooks::chain::HOOK_EVENT_NAMES
            .iter()
            .map(|name| name.to_string())
            .collect(),
        hooks,
    })
}

fn daemon_cwd_scope() -> String {
    workspace_scope(&std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")))
}

fn collect_list_items(bus: &HookBus, registry: &HookRegistry, scope: &str) -> Vec<HookListItem> {
    bus.inventory()
        .unwrap_or_default()
        .into_iter()
        .map(|hook| HookListItem {
            revoked: registry.is_revoked(&hook_revocation_key(&hook, scope)),
            id: hook.id,
            event: hook.event,
            kind: hook.spec.kind.as_str().to_string(),
            matcher: hook.spec.matcher,
            source: hook.origin.to_string(),
            configured: true,
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
    // Handler tests use local managed child semantics; daemon production stays
    // bound to native confinement adapter.
    let adapter = rightkit_agent_coding_tools::shell::NativeCommandSandboxAdapter;
    let hooks = daemon_wide_bus().hooks_for(&req.event).unwrap_or_default();
    // M4: revocations are stable keys, so revoked hooks are filtered here and reported by
    // their positional id, rather than letting the dry run compare positional ids.
    let (revoked_hooks, hooks) = split_revoked(hooks, &shared_registry(), &context.workspace_root);
    let revoked_candidates: Vec<String> = revoked_hooks
        .into_iter()
        .filter(|hook| hook.event == req.event)
        .filter(|hook| hook.spec.kind == rightkit_hooks::agent_hooks::HookKind::Command)
        .filter(|hook| hook.spec.matches(matcher))
        .map(|hook| hook.id)
        .collect();
    let mut report: HookDryRunReport = rightkit_hooks::agent_hooks::dry_run_hook_with_adapter(
        &req.event, matcher, None, &hooks, &adapter, &context,
    )
    .await;
    if !revoked_candidates.is_empty() {
        report.no_match = false;
        report.revoked.extend(revoked_candidates);
    }
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

/// `hooks` split into (revoked, live) by each hook's stable key in `workspace`.
fn split_revoked(
    hooks: Vec<IdentifiedHook>,
    registry: &HookRegistry,
    workspace: &str,
) -> (Vec<IdentifiedHook>, Vec<IdentifiedHook>) {
    let scope = workspace_scope(Path::new(workspace));
    hooks
        .into_iter()
        .partition(|hook| registry.is_revoked(&hook_revocation_key(hook, &scope)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HookRevokeRequest {
    /// A positional id from `/hooks/list` (`PostToolUse#1`), or a stable key (`hook:...`)
    /// from a revoke response's snapshot.
    id: String,
    /// The workspace whose hook list the positional id refers to. Default: the daemon's cwd,
    /// the view `/hooks/list` shows.
    #[serde(default)]
    workspace: Option<String>,
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
    let workspace = req
        .workspace
        .as_deref()
        .filter(|path| !path.trim().is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        });
    let bus = HookBus::new(1);
    configure_session_hooks(&bus, &workspace);
    let key = resolve_revocation_key(&req.id, &bus, &workspace_scope(&workspace));
    Json(apply_revocation(&registry, req.id, key, req.restore))
}

/// The stable key `id` names in `bus`'s hook list: a `hook:` key as given, or the key of the
/// hook at that position now. `None` when no such hook exists.
fn resolve_revocation_key(id: &str, bus: &HookBus, scope: &str) -> Option<String> {
    if id.starts_with(HOOK_KEY_PREFIX) {
        return Some(id.to_string());
    }
    bus.inventory()
        .unwrap_or_default()
        .iter()
        .find(|hook| hook.id == id)
        .map(|hook| hook_revocation_key(hook, scope))
}

fn apply_revocation(
    registry: &HookRegistry,
    id: String,
    key: Option<String>,
    restore: bool,
) -> HookRevokeResponse {
    let Some(key) = key else {
        // An id that names no hook is never stored: a stale positional id must not
        // later match whatever hook moves into that slot.
        return HookRevokeResponse {
            id,
            applied: false,
            revoked: false,
            snapshot: registry.snapshot(),
        };
    };
    let applied = if restore {
        registry.unrevoke(&key)
    } else {
        registry.revoke(&key)
    };
    HookRevokeResponse {
        id,
        applied,
        revoked: registry.is_revoked(&key),
        snapshot: registry.snapshot(),
    }
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
            "required": ["total", "configured", "revoked", "events", "hooks"],
            "properties": {
                "total": {"type": "integer", "minimum": 0},
                "configured": {"type": "integer", "minimum": 0},
                "revoked": {"type": "integer", "minimum": 0},
                "events": {"type": "array", "items": {"type": "string"}},
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
                "workspace": {"type": ["string", "null"]},
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
                "workspace": {"type": ["string", "null"]},
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
