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

/// Test-only: replace the daemon-wide registry with `reg`. No-op in non-test
/// builds. Used by tests that need the shared static to reflect a particular
/// revocation state regardless of test execution order.
#[cfg(test)]
pub(crate) fn replace_shared_registry(reg: Arc<HookRegistry>) {
    REGISTRY
        .get_or_init(|| std::sync::Mutex::new(Arc::new(HookRegistry::default())))
        .lock()
        .expect("REGISTRY mutex poisoned")
        .clone_from(&reg);
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
    // Handler tests use local managed child semantics; daemon production stays
    // bound to native confinement adapter.
    #[cfg(test)]
    let adapter = rightkit_agent_tools::coding_tools::process_launch::LocalProcessLaunchAdapter;
    #[cfg(not(test))]
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

/// Serializes tests that touch the process-global hook state (the shared
/// `REGISTRY` static and `RIGHTKIT_HOOK_*` env vars). Without this the
/// hooks tests race under default-parallel `cargo test` and fail; the
/// single-threaded DoD gate always passed. Poison-tolerant so one failing
/// test does not cascade.
#[cfg(test)]
pub(crate) fn hooks_global_state_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
#[allow(clippy::await_holding_lock)]
mod hooks_management_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    async fn list_request() -> Request<Body> {
        Request::builder()
            .uri("/hooks/list")
            .body(Body::empty())
            .unwrap()
    }

    async fn post_json(uri: &str, body: Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    /// Build a router with a specific registry installed into the daemon-wide
    /// static. Swappable so tests can verify per-registry behaviour in any
    /// order.
    fn app_with_registry(registry: Arc<HookRegistry>) -> Router {
        replace_shared_registry(registry);
        hook_management_routes().with_state(crate::api::DaemonState::new(""))
    }

    #[tokio::test]
    async fn list_handlers_returns_all_entries_redacted() {
        let _guard = hooks_global_state_test_lock();
        // Ensure no env vars leak into the test.
        for key in [
            "RIGHTKIT_HOOK_PRE_TOOL_USE",
            "RIGHTKIT_HOOK_POST_TOOL_USE",
            "RIGHTKIT_HOOK_USER_PROMPT_SUBMIT",
            "RIGHTKIT_HOOK_NOTIFICATION",
            "RIGHTKIT_HOOK_SUBAGENT_STOP",
            "RIGHTKIT_HOOK_STOP_BLOCKING",
            "RIGHTKIT_HOOK_PRE_COMMIT",
        ] {
            std::env::remove_var(key);
        }
        let registry = Arc::new(HookRegistry::default());
        let app = app_with_registry(registry);
        let resp = app.oneshot(list_request().await).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        // 7 command + 6 URL + 6 MCP hook slots enumerated by hook_status_from_config.
        assert_eq!(json["total"], 19);
        assert_eq!(json["configured"], 0);
        assert_eq!(json["revoked"], 0);
        let hooks = json["hooks"].as_array().unwrap();
        assert!(hooks.iter().all(|h| h["matcher"].is_null()));
        assert!(hooks
            .iter()
            .all(|h| h["source"].as_str().unwrap().starts_with("RIGHTKIT_HOOK_")));
        assert!(hooks.iter().all(|h| h["id"] == h["source"]));
    }

    #[tokio::test]
    async fn list_handler_marks_revoked_entries() {
        let _guard = hooks_global_state_test_lock();
        std::env::remove_var("RIGHTKIT_HOOK_NOTIFICATION");
        std::env::set_var("RIGHTKIT_HOOK_NOTIFICATION", "echo ran");
        let registry = Arc::new(HookRegistry::default());
        registry.revoke("RIGHTKIT_HOOK_NOTIFICATION");
        let app = app_with_registry(registry);
        let resp = app.oneshot(list_request().await).await.unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        let hooks = json["hooks"].as_array().unwrap();
        let notification = hooks
            .iter()
            .find(|h| h["id"] == "RIGHTKIT_HOOK_NOTIFICATION")
            .unwrap();
        assert_eq!(notification["configured"], true);
        assert_eq!(notification["revoked"], true);
        // The status aggregate drops revoked entries from `configured`.
        assert!(json["revoked"].as_u64().unwrap() >= 1);
        std::env::remove_var("RIGHTKIT_HOOK_NOTIFICATION");
    }

    #[tokio::test]
    async fn test_handler_runs_proceed_outcome() {
        let _guard = hooks_global_state_test_lock();
        std::env::set_var("RIGHTKIT_HOOK_NOTIFICATION", "exit 0");
        let registry = Arc::new(HookRegistry::default());
        let app = app_with_registry(registry);
        let resp = app
            .oneshot(post_json("/hooks/test", serde_json::json!({"event": "Notification"})).await)
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["ran"], true);
        assert_eq!(json["outcome"], "Proceed");
        assert_eq!(
            json["invoked"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect::<Vec<_>>(),
            vec!["RIGHTKIT_HOOK_NOTIFICATION".to_string()]
        );
        std::env::remove_var("RIGHTKIT_HOOK_NOTIFICATION");
    }

    #[tokio::test]
    async fn test_handler_runs_deny_outcome() {
        let _guard = hooks_global_state_test_lock();
        // dry_run_hook runs the command via the platform shell (`cmd /C` on
        // Windows, `sh -c` elsewhere); `;` is not a cmd separator, so use the
        // platform-correct chaining operator.
        let deny_cmd = if cfg!(windows) {
            "echo blocked& exit 2"
        } else {
            "echo blocked; exit 2"
        };
        std::env::set_var("RIGHTKIT_HOOK_NOTIFICATION", deny_cmd);
        let registry = Arc::new(HookRegistry::default());
        let app = app_with_registry(registry);
        let resp = app
            .oneshot(post_json("/hooks/test", serde_json::json!({"event": "Notification"})).await)
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["ran"], true);
        assert_eq!(json["outcome"], "Deny");
        assert_eq!(json["message"], "blocked");
        std::env::remove_var("RIGHTKIT_HOOK_NOTIFICATION");
    }

    #[tokio::test]
    async fn test_handler_reports_revoked_when_revoked() {
        let _guard = hooks_global_state_test_lock();
        std::env::set_var("RIGHTKIT_HOOK_NOTIFICATION", "exit 0");
        let registry = Arc::new(HookRegistry::default());
        registry.revoke("RIGHTKIT_HOOK_NOTIFICATION");
        let app = app_with_registry(registry);
        let resp = app
            .oneshot(post_json("/hooks/test", serde_json::json!({"event": "Notification"})).await)
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["ran"], false);
        assert_eq!(json["outcome"], "Proceed");
        assert_eq!(
            json["revoked"],
            serde_json::json!(["RIGHTKIT_HOOK_NOTIFICATION"])
        );
        std::env::remove_var("RIGHTKIT_HOOK_NOTIFICATION");
    }

    #[tokio::test]
    async fn test_handler_reports_no_match_for_unknown_event() {
        let _guard = hooks_global_state_test_lock();
        let registry = Arc::new(HookRegistry::default());
        let app = app_with_registry(registry);
        let resp = app
            .oneshot(post_json("/hooks/test", serde_json::json!({"event": "ALienEvent"})).await)
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["ran"], false);
        assert_eq!(json["noMatch"], true);
        assert_eq!(json["outcome"], "Proceed");
        assert_eq!(json["invoked"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn revoke_handler_records_id_and_restores() {
        let _guard = hooks_global_state_test_lock();
        let registry = Arc::new(HookRegistry::default());
        let app = app_with_registry(registry.clone());
        let resp = app
            .oneshot(
                post_json(
                    "/hooks/revoke",
                    serde_json::json!({"id": "RIGHTKIT_HOOK_NOTIFICATION"}),
                )
                .await,
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["applied"], true);
        assert_eq!(json["revoked"], true);
        assert_eq!(
            json["snapshot"],
            serde_json::json!(["RIGHTKIT_HOOK_NOTIFICATION"])
        );
        assert!(registry.is_revoked("RIGHTKIT_HOOK_NOTIFICATION"));

        // Restore
        let app2 = app_with_registry(registry.clone());
        let resp = app2
            .oneshot(
                post_json(
                    "/hooks/revoke",
                    serde_json::json!({
                        "id": "RIGHTKIT_HOOK_NOTIFICATION",
                        "restore": true
                    }),
                )
                .await,
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["applied"], true);
        assert_eq!(json["revoked"], false);
        assert_eq!(json["snapshot"], serde_json::json!([]));
        assert!(!registry.is_revoked("RIGHTKIT_HOOK_NOTIFICATION"));
    }

    #[tokio::test]
    async fn list_includes_revoked_then_cleared_when_revoke_restored() {
        let _guard = hooks_global_state_test_lock();
        std::env::remove_var("RIGHTKIT_HOOK_PRE_TOOL_USE");
        std::env::set_var("RIGHTKIT_HOOK_PRE_TOOL_USE", "exit 0");
        let registry = Arc::new(HookRegistry::default());
        registry.revoke("RIGHTKIT_HOOK_PRE_TOOL_USE");
        let app = app_with_registry(registry.clone());
        let resp = app.oneshot(list_request().await).await.unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        let pre = json["hooks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["id"] == "RIGHTKIT_HOOK_PRE_TOOL_USE")
            .unwrap();
        assert_eq!(pre["revoked"], true);

        registry.unrevoke("RIGHTKIT_HOOK_PRE_TOOL_USE");
        let app2 = app_with_registry(registry.clone());
        let resp = app2.oneshot(list_request().await).await.unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        let pre = json["hooks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["id"] == "RIGHTKIT_HOOK_PRE_TOOL_USE")
            .unwrap();
        assert_eq!(pre["revoked"], false);
        assert_eq!(pre["configured"], true);
        std::env::remove_var("RIGHTKIT_HOOK_PRE_TOOL_USE");
    }

    #[test]
    fn collect_list_items_marks_revoked_independently_of_configured() {
        let _guard = hooks_global_state_test_lock();
        // `configured` reflects the env-set truth; `revoked` is an orthogonal
        // suppression flag. A configured-but-revoked hook reports both true so
        // operators can tell "not configured" apart from "configured but
        // revoked". The configured *tally* excludes revoked entries — asserted
        // via the list handler in `list_handler_marks_revoked_entries`.
        std::env::remove_var("RIGHTKIT_HOOK_PRE_TOOL_USE");
        std::env::set_var("RIGHTKIT_HOOK_PRE_TOOL_USE", "exit 0");
        let registry = HookRegistry::default();
        registry.revoke("RIGHTKIT_HOOK_PRE_TOOL_USE");
        let config = HookRunnerConfig::from_env();
        let items = collect_list_items(&config, &registry);
        let pre = items
            .iter()
            .find(|i| i.id == "RIGHTKIT_HOOK_PRE_TOOL_USE")
            .unwrap();
        assert!(pre.configured);
        assert!(pre.revoked);
        std::env::remove_var("RIGHTKIT_HOOK_PRE_TOOL_USE");
    }

    #[tokio::test]
    async fn spawn_hook_runner_does_not_fire_revoked_posttool_use_mcp() {
        let _guard = hooks_global_state_test_lock();
        use rightkit_mcp::agent_mcp::{
            McpError, McpManager, McpResourceContent, McpResourceDef, McpToolDef, McpToolResult,
            McpTransport,
        };
        use std::sync::Arc;
        use tokio::sync::mpsc;

        struct RecordingMcp {
            tx: mpsc::Sender<String>,
        }
        #[async_trait::async_trait]
        impl McpTransport for RecordingMcp {
            async fn list_tools(&self) -> Result<Vec<McpToolDef>, McpError> {
                Ok(vec![McpToolDef {
                    name: "record".into(),
                    description: "record".into(),
                    annotations: rightkit_mcp::agent_mcp::McpToolAnnotations::default(),
                    input_schema: serde_json::json!({"type": "object"}),
                }])
            }
            async fn call_tool(
                &self,
                _name: &str,
                _args: Value,
                _session: Option<&str>,
            ) -> Result<McpToolResult, McpError> {
                let _ = self.tx.send("fired".to_string()).await;
                Ok(McpToolResult {
                    content: "ok".into(),
                    is_error: false,
                })
            }
            async fn list_resources(&self) -> Result<Vec<McpResourceDef>, McpError> {
                Ok(vec![])
            }
            async fn read_resource(&self, _uri: &str) -> Result<McpResourceContent, McpError> {
                Err(McpError::ResourceNotFound("unused".into()))
            }
        }

        let (tx, mut rx) = mpsc::channel::<String>(1);
        let mut manager = McpManager::new();
        manager.register("hooks", Arc::new(RecordingMcp { tx }));

        let registry = HookRegistry::new();
        registry.revoke("RIGHTKIT_HOOK_POST_TOOL_USE_MCP");
        // Sanity-check the registry before plumbing.
        assert!(registry.is_revoked("RIGHTKIT_HOOK_POST_TOOL_USE_MCP"));

        let bus = rightkit_hooks::agent_hooks::HookBus::new(16);
        let config = HookRunnerConfig {
            post_tool_use_mcp_tool: Some("mcp__hooks__record".to_string()),
            mcp: Some(Arc::new(manager)),
            revoked: Some(Arc::new(registry)),
            ..Default::default()
        };
        super::spawn_hook_runner_with_config(&bus, config);

        bus.publish(HookEvent::PostToolUse {
            session_id: "s".into(),
            tool: "read_file".into(),
            is_error: false,
        });

        // After 200ms the runner must NOT have invoked the recording MCP hook.
        let res = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await;
        assert!(
            res.is_err(),
            "revoked PostToolUse MCP hook fired anyway: {res:?}"
        );
    }
}
