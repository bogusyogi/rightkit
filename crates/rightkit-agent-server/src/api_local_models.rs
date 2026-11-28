use axum::{
    extract::Query,
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::api::{DaemonState, WorkspaceQuery};
use crate::api_permissions::enforce_trusted_source_authority;

pub(crate) fn local_model_routes() -> Router<DaemonState> {
    Router::new()
        .route("/local-models/hardware", get(get_local_model_hardware))
        .route("/local-models/catalog", get(get_local_model_catalog))
        .route("/local-models/probe", post(probe_local_model_endpoint))
        .route(
            "/local-models/discover",
            post(discover_local_model_endpoints),
        )
        .route(
            "/local-models/chat-smoke",
            post(chat_smoke_local_model_endpoint),
        )
        .route("/local-models/add-provider", post(add_local_model_provider))
}

async fn get_local_model_hardware(
) -> Result<Json<rightkit_agent_local_models::HardwareReport>, StatusCode> {
    let adapter = crate::main_runtime::governed_child_process_launcher()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let context = rightkit_sandbox::execution::ProcessLaunchContext {
        workspace_root: std::env::current_dir()
            .unwrap_or_default()
            .display()
            .to_string(),
        sandbox_profile: Some("local-model-probe".to_string()),
        sandbox_required: true,
        network_required: false,
    };
    Ok(Json(
        rightkit_agent_local_models::probe_hardware_with_adapter(adapter.as_ref(), &context).await,
    ))
}

async fn get_local_model_catalog(
) -> Result<Json<rightkit_agent_local_models::LocalModelCatalog>, StatusCode> {
    let adapter = crate::main_runtime::governed_child_process_launcher()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let context = rightkit_sandbox::execution::ProcessLaunchContext {
        workspace_root: std::env::current_dir()
            .unwrap_or_default()
            .display()
            .to_string(),
        sandbox_profile: Some("local-model-probe".to_string()),
        sandbox_required: true,
        network_required: false,
    };
    Ok(Json(rightkit_agent_local_models::model_catalog(
        rightkit_agent_local_models::probe_hardware_with_adapter(adapter.as_ref(), &context).await,
    )))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProbeEndpointRequest {
    base_url: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProbeEndpointResponse {
    ok: bool,
    base_url: String,
    models_url: String,
    model_count: usize,
    models: Vec<String>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatSmokeRequest {
    base_url: String,
    model: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatSmokeResponse {
    ok: bool,
    base_url: String,
    chat_url: String,
    model: String,
    response_preview: Option<String>,
    error: Option<String>,
}

async fn probe_local_model_endpoint(
    headers: HeaderMap,
    Json(body): Json<ProbeEndpointRequest>,
) -> Result<Json<ProbeEndpointResponse>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    Ok(Json(probe_openai_models_endpoint(&body.base_url).await))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct DiscoverEndpointsRequest {
    /// Base URLs to probe; empty (or no body) probes the usual local-server addresses.
    candidates: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiscoveredEndpoint {
    base_url: String,
    ok: bool,
    models: Vec<String>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiscoverEndpointsResponse {
    endpoints: Vec<DiscoveredEndpoint>,
}

/// Most candidates one discovery request probes.
const DISCOVER_MAX_CANDIDATES: usize = 16;
/// How long one candidate may take; all candidates are probed at once.
const DISCOVER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// The candidate list of a discovery request: the caller's, trimmed, without blanks or repeats and
/// at most [`DISCOVER_MAX_CANDIDATES`]; the catalog's local-server addresses when it names none.
fn discovery_candidates(requested: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut list: Vec<String> = requested
        .into_iter()
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty() && seen.insert(c.clone()))
        .take(DISCOVER_MAX_CANDIDATES)
        .collect();
    if list.is_empty() {
        list = rightkit_agent_local_models::LOCAL_SERVER_ENDPOINTS
            .iter()
            .map(|url| url.to_string())
            .collect();
    }
    list
}

async fn discover_local_model_endpoints(
    headers: HeaderMap,
    body: Option<Json<DiscoverEndpointsRequest>>,
) -> Result<Json<DiscoverEndpointsResponse>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let requested = body.map(|Json(body)| body.candidates).unwrap_or_default();
    let probes = discovery_candidates(requested)
        .into_iter()
        .map(|candidate| async move {
            let probed = probe_openai_models_endpoint_within(&candidate, DISCOVER_TIMEOUT).await;
            DiscoveredEndpoint {
                base_url: candidate,
                ok: probed.ok,
                models: probed.models,
                error: probed.error,
            }
        });
    Ok(Json(DiscoverEndpointsResponse {
        endpoints: futures_util::future::join_all(probes).await,
    }))
}

async fn chat_smoke_local_model_endpoint(
    headers: HeaderMap,
    Json(body): Json<ChatSmokeRequest>,
) -> Result<Json<ChatSmokeResponse>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    Ok(Json(
        chat_smoke_openai_endpoint(&body.base_url, &body.model).await,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddProviderRequest {
    provider_id: String,
    base_url: String,
    model: String,
    #[serde(default)]
    set_default: bool,
    /// Explicit caller approval for registering a base_url that resolves to a
    /// private/reserved network target. Loopback (127.0.0.1, localhost) never
    /// needs this — it is the primary local-model use case and stays a single
    /// no-prompt registration. Ignored (has no effect) when the target isn't
    /// private, so a client can always send it defensively.
    #[serde(default)]
    approve_private: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AddProviderResponse {
    #[serde(flatten)]
    suggestion: rightkit_agent_local_models::ProviderRouteSuggestion,
    /// Live in the running daemon. Always false: the engine provider is
    /// resolved once at boot, so a freshly written route needs a restart.
    applied: bool,
    requires_restart: bool,
    active_in_running_daemon: bool,
    scope: ProviderRouteScope,
    set_default: bool,
    config_path: String,
    approved_private: bool,
}

/// Which config file a route was written to. `User` is the one the daemon
/// reads at boot; `Workspace` routes apply only to tooling that merges the
/// workspace config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum ProviderRouteScope {
    User,
    Workspace,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProviderRouteError {
    error: String,
}

pub(crate) fn local_models_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    schemas.insert(
        "LocalModelHardwareReport".to_string(),
        json!({"type": "object"}),
    );
    schemas.insert("LocalModelCatalog".to_string(), json!({"type": "object"}));
    schemas.insert(
        "ProbeLocalModelEndpointRequest".to_string(),
        json!({
            "type": "object",
            "required": ["baseUrl"],
            "properties": {"baseUrl": {"type": "string"}},
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ProbeLocalModelEndpointResponse".to_string(),
        json!({
            "type": "object",
            "required": ["ok", "baseUrl", "modelsUrl", "modelCount", "models"],
            "properties": {
                "ok": {"type": "boolean"},
                "baseUrl": {"type": "string"},
                "modelsUrl": {"type": "string"},
                "modelCount": {"type": "integer", "minimum": 0},
                "models": {"type": "array", "items": {"type": "string"}},
                "error": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "DiscoverLocalModelEndpointsRequest".to_string(),
        json!({
            "type": "object",
            "properties": {"candidates": {"type": "array", "items": {"type": "string"}, "maxItems": 16}},
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "DiscoverLocalModelEndpointsResponse".to_string(),
        json!({
            "type": "object",
            "required": ["endpoints"],
            "properties": {"endpoints": {"type": "array", "items": {
                "type": "object",
                "required": ["baseUrl", "ok", "models"],
                "properties": {
                    "baseUrl": {"type": "string"},
                    "ok": {"type": "boolean"},
                    "models": {"type": "array", "items": {"type": "string"}},
                    "error": {"type": ["string", "null"]}
                },
                "additionalProperties": false
            }}},
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ChatSmokeLocalModelRequest".to_string(),
        json!({
            "type": "object",
            "required": ["baseUrl", "model"],
            "properties": {
                "baseUrl": {"type": "string"},
                "model": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ChatSmokeLocalModelResponse".to_string(),
        json!({
            "type": "object",
            "required": ["ok", "baseUrl", "chatUrl", "model"],
            "properties": {
                "ok": {"type": "boolean"},
                "baseUrl": {"type": "string"},
                "chatUrl": {"type": "string"},
                "model": {"type": "string"},
                "responsePreview": {"type": ["string", "null"]},
                "error": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "AddLocalModelProviderRequest".to_string(),
        json!({
            "type": "object",
            "required": ["providerId", "baseUrl", "model"],
            "properties": {
                "providerId": {"type": "string"},
                "baseUrl": {"type": "string"},
                "model": {"type": "string"},
                "setDefault": {"type": "boolean"},
                "approvePrivate": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "AddLocalModelProviderResponse".to_string(),
        json!({
            "type": "object",
            "required": ["providerId", "displayName", "baseUrl", "model", "apiKeyRequired", "note", "applied", "requiresRestart", "activeInRunningDaemon", "scope", "setDefault", "configPath", "approvedPrivate"],
            "properties": {
                "providerId": {"type": "string"},
                "displayName": {"type": "string"},
                "baseUrl": {"type": "string"},
                "model": {"type": "string"},
                "apiKeyRequired": {"type": "boolean"},
                "note": {"type": "string"},
                "applied": {"type": "boolean"},
                "requiresRestart": {"type": "boolean"},
                "activeInRunningDaemon": {"type": "boolean"},
                "scope": {"type": "string", "enum": ["user", "workspace"]},
                "setDefault": {"type": "boolean"},
                "configPath": {"type": "string"},
                "approvedPrivate": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas
}

pub(crate) fn local_model_hardware_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/LocalModelHardwareReport"})
}

pub(crate) fn local_model_catalog_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/LocalModelCatalog"})
}

pub(crate) fn probe_local_model_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/ProbeLocalModelEndpointRequest"})
}

pub(crate) fn probe_local_model_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ProbeLocalModelEndpointResponse"})
}

pub(crate) fn discover_local_models_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/DiscoverLocalModelEndpointsRequest"})
}

pub(crate) fn discover_local_models_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/DiscoverLocalModelEndpointsResponse"})
}

pub(crate) fn chat_smoke_local_model_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/ChatSmokeLocalModelRequest"})
}

pub(crate) fn chat_smoke_local_model_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ChatSmokeLocalModelResponse"})
}

pub(crate) fn add_local_model_provider_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/AddLocalModelProviderRequest"})
}

pub(crate) fn add_local_model_provider_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/AddLocalModelProviderResponse"})
}

async fn add_local_model_provider(
    Query(q): Query<WorkspaceQuery>,
    headers: HeaderMap,
    Json(body): Json<AddProviderRequest>,
) -> Result<Json<AddProviderResponse>, (StatusCode, Json<ProviderRouteError>)> {
    enforce_trusted_source_authority(&headers).map_err(|status| {
        route_error(
            status,
            "untrusted source for provider registration".to_string(),
        )
    })?;
    validate_provider_route_id(&body.provider_id).map_err(|err| {
        route_error(
            StatusCode::BAD_REQUEST,
            format!("invalid providerId '{}': {err}", body.provider_id),
        )
    })?;
    let approved_private = validated_provider_base_url(&body.base_url, body.approve_private)
        .await
        .map_err(|()| {
            route_error(
                StatusCode::BAD_REQUEST,
                format!(
                    "rejected baseUrl '{}': must be a reachable http(s) URL; private/reserved targets need approvePrivate=true",
                    body.base_url
                ),
            )
        })?;
    let suggestion = rightkit_agent_local_models::provider_route_suggestion(
        body.provider_id,
        body.base_url,
        body.model,
    );

    // Scope matters and used to be invisible: the running daemon resolves its
    // engine provider ONCE at boot from the user-global merged config, so a
    // route written only into a workspace config never reaches the engine —
    // the caller would see success and a permanently inert route. With no
    // explicit workspaceRoot we therefore target the user config the engine
    // actually reads, and we always report which file was written.
    let scope = if q.workspace_root.trim().is_empty() {
        ProviderRouteScope::User
    } else {
        ProviderRouteScope::Workspace
    };
    let workspace = workspace_path(&q);

    // Audit 2026-11-05 platform H1: a saved provider key must never be redirected to a
    // caller-chosen host by registering a route under the key's id.
    let refusal = {
        let provider_id = suggestion.provider_id.clone();
        let base_url = suggestion.base_url.clone();
        let target = workspace.clone();
        tokio::task::spawn_blocking(move || {
            let has_saved_key = rightkit_llm::agent_providers::load_api_key(&provider_id)
                .is_some_and(|key| !key.trim().is_empty());
            let layer = match scope {
                ProviderRouteScope::User => load_user_config_only(),
                ProviderRouteScope::Workspace => load_workspace_config_only(&target),
            };
            let existing = layer
                .ok()
                .and_then(|config| config.provider_routes.get(&provider_id).cloned())
                .map(|route| route.base_url);
            provider_key_redirect_refusal(
                &provider_id,
                &base_url,
                has_saved_key,
                existing.as_deref(),
            )
        })
        .await
        .unwrap_or_else(|e| Some(format!("provider key check stopped: {e}")))
    };
    if let Some(message) = refusal {
        return Err(route_error(StatusCode::CONFLICT, message));
    }

    let route = (
        suggestion.provider_id.clone(),
        rightkit_agent_config::ProviderRouteConfig {
            display_name: suggestion.display_name.clone(),
            base_url: suggestion.base_url.clone(),
            model: suggestion.model.clone(),
            api_key_required: suggestion.api_key_required,
            api_key_env: None,
            approved_private,
            ..Default::default()
        },
    );
    let set_default = body.set_default;
    let default_model = suggestion.model.clone();
    let apply = move |config: &mut rightkit_agent_config::HostConfig| {
        config.provider_routes.insert(route.0, route.1);
        if set_default {
            config.model.default_model = default_model;
        }
    };

    // The config lock wait can last five seconds, so the read-modify-write runs on a blocking thread.
    let config_path = match scope {
        ProviderRouteScope::User => {
            // Read-modify-write of the user file under the config lock, starting from the user
            // layer alone, so a concurrent save elsewhere is merged in rather than overwritten.
            tokio::task::spawn_blocking(move || {
                rightkit_agent_config::ConfigLoader::new()
                    .update_user_from(|_| load_user_config_only(), apply)
            })
            .await
            .unwrap_or_else(|e| {
                Err(rightkit_agent_config::ConfigError::Io(format!(
                    "config writer stopped: {e}"
                )))
            })
            .map_err(|err| {
                route_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("could not write user config: {err}"),
                )
            })?;
            user_config_path_display()
        }
        ProviderRouteScope::Workspace => {
            // The workspace file gets its own lock; a malformed existing file is a bad request.
            let target = workspace.clone();
            let (result, read_failed) = tokio::task::spawn_blocking(move || {
                let read_failed = std::cell::Cell::new(false);
                let result = rightkit_agent_config::ConfigLoader::new().update_workspace_from(
                    &target,
                    || {
                        load_workspace_config_only(&target).inspect_err(|_| {
                            read_failed.set(true);
                        })
                    },
                    apply,
                );
                (result.map(|_| ()), read_failed.get())
            })
            .await
            .unwrap_or_else(|e| {
                (
                    Err(rightkit_agent_config::ConfigError::Io(format!(
                        "config writer stopped: {e}"
                    ))),
                    false,
                )
            });
            result.map_err(|err| {
                if read_failed {
                    route_error(
                        StatusCode::BAD_REQUEST,
                        format!("could not read existing config: {err}"),
                    )
                } else {
                    route_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!(
                            "could not write workspace config at {}: {err}",
                            workspace_config_path(&workspace).display()
                        ),
                    )
                }
            })?;
            workspace_config_path(&workspace).display().to_string()
        }
    };

    Ok(Json(AddProviderResponse {
        suggestion,
        approved_private,
        // Persisted, but never live in this process: the engine provider is
        // resolved at daemon boot. Saying `applied: true` here was a false
        // success claim.
        applied: false,
        requires_restart: true,
        scope,
        active_in_running_daemon: false,
        set_default: body.set_default,
        config_path,
    }))
}

fn route_error(status: StatusCode, message: String) -> (StatusCode, Json<ProviderRouteError>) {
    (status, Json(ProviderRouteError { error: message }))
}

fn user_config_path_display() -> String {
    user_config_path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| {
            format!(
                "<{} state directory>/config.toml",
                rightkit_agent_config::app_name()
            )
        })
}

fn user_config_path() -> Option<std::path::PathBuf> {
    rightkit_agent_config::state_root().map(|root| root.join("config.toml"))
}

fn load_user_config_only(
) -> Result<rightkit_agent_config::HostConfig, rightkit_agent_config::ConfigError> {
    let Some(path) = user_config_path() else {
        return Ok(rightkit_agent_config::HostConfig::default());
    };
    match std::fs::read_to_string(&path) {
        Ok(content) => rightkit_agent_config::ConfigLoader::new().load_str(&content),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(rightkit_agent_config::HostConfig::default())
        }
        Err(err) => Err(rightkit_agent_config::ConfigError::Io(err.to_string())),
    }
}

async fn probe_openai_models_endpoint(base_url: &str) -> ProbeEndpointResponse {
    probe_openai_models_endpoint_within(base_url, std::time::Duration::from_secs(5)).await
}

async fn probe_openai_models_endpoint_within(
    base_url: &str,
    timeout: std::time::Duration,
) -> ProbeEndpointResponse {
    let trimmed = base_url.trim().trim_end_matches('/');
    let models_url = format!("{trimmed}/models");
    let outcome = match rightkit_agent_daemon::http_client::endpoint_client(&models_url, timeout) {
        Ok(client) => {
            rightkit_agent_daemon::http_client::send_any(
                &client,
                &rightkit_http::Request::get(models_url.clone()),
            )
            .await
        }
        Err(error) => Err(error),
    };
    match outcome {
        Ok(resp) => {
            let status = resp.status;
            match resp.json::<serde_json::Value>() {
                Ok(json)
                    if rightkit_agent_daemon::http_client::is_success(status)
                        && json
                            .get("data")
                            .and_then(serde_json::Value::as_array)
                            .is_none() =>
                {
                    ProbeEndpointResponse {
                        ok: false,
                        base_url: trimmed.to_string(),
                        models_url,
                        model_count: 0,
                        models: Vec::new(),
                        error: Some(format!(
                            "endpoint returned {status} without an OpenAI `data` model list"
                        )),
                    }
                }
                Ok(json) if rightkit_agent_daemon::http_client::is_success(status) => {
                    let models = json
                        .get("data")
                        .and_then(serde_json::Value::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|item| {
                                    item.get("id").and_then(serde_json::Value::as_str)
                                })
                                .map(str::to_string)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    ProbeEndpointResponse {
                        ok: true,
                        base_url: trimmed.to_string(),
                        models_url,
                        model_count: models.len(),
                        models,
                        error: None,
                    }
                }
                Ok(json) => ProbeEndpointResponse {
                    ok: false,
                    base_url: trimmed.to_string(),
                    models_url,
                    model_count: 0,
                    models: Vec::new(),
                    error: Some(format!("endpoint returned {status}: {json}")),
                },
                Err(error) => ProbeEndpointResponse {
                    ok: false,
                    base_url: trimmed.to_string(),
                    models_url,
                    model_count: 0,
                    models: Vec::new(),
                    error: Some(format!(
                        "endpoint returned {status}, but JSON parse failed: {error}"
                    )),
                },
            }
        }
        Err(error) => ProbeEndpointResponse {
            ok: false,
            base_url: trimmed.to_string(),
            models_url,
            model_count: 0,
            models: Vec::new(),
            error: Some(error),
        },
    }
}

async fn chat_smoke_openai_endpoint(base_url: &str, model: &str) -> ChatSmokeResponse {
    let trimmed = base_url.trim().trim_end_matches('/');
    let model = model.trim();
    let chat_url = format!("{trimmed}/chat/completions");
    if trimmed.is_empty() || model.is_empty() {
        return ChatSmokeResponse {
            ok: false,
            base_url: trimmed.to_string(),
            chat_url,
            model: model.to_string(),
            response_preview: None,
            error: Some("baseUrl and model are required".to_string()),
        };
    }
    let body = serde_json::json!({
        "model": model,
        "messages": [
            {
                "role": "user",
                "content": format!("Reply with exactly: {app} local model ok", app = crate::host_identity::app())
            }
        ],
        "max_tokens": 16,
        "temperature": 0,
        "stream": false
    });
    let outcome = match rightkit_agent_daemon::http_client::endpoint_client(
        &chat_url,
        std::time::Duration::from_secs(10),
    ) {
        Ok(client) => match rightkit_http::Request::post(chat_url.clone()).json(&body) {
            Ok(request) => rightkit_agent_daemon::http_client::send_any(&client, &request).await,
            Err(error) => Err(error.to_string()),
        },
        Err(error) => Err(error),
    };
    match outcome {
        Ok(resp) => {
            let status = resp.status;
            match resp.json::<serde_json::Value>() {
                Ok(json) if rightkit_agent_daemon::http_client::is_success(status) => {
                    let preview = json
                        .get("choices")
                        .and_then(serde_json::Value::as_array)
                        .and_then(|choices| choices.first())
                        .and_then(|choice| choice.get("message"))
                        .and_then(|message| message.get("content"))
                        .and_then(serde_json::Value::as_str)
                        .map(|content| content.trim().chars().take(240).collect::<String>());
                    ChatSmokeResponse {
                        ok: preview.is_some(),
                        base_url: trimmed.to_string(),
                        chat_url,
                        model: model.to_string(),
                        response_preview: preview,
                        error: None,
                    }
                }
                Ok(json) => ChatSmokeResponse {
                    ok: false,
                    base_url: trimmed.to_string(),
                    chat_url,
                    model: model.to_string(),
                    response_preview: None,
                    error: Some(format!("endpoint returned {status}: {json}")),
                },
                Err(error) => ChatSmokeResponse {
                    ok: false,
                    base_url: trimmed.to_string(),
                    chat_url,
                    model: model.to_string(),
                    response_preview: None,
                    error: Some(format!(
                        "endpoint returned {status}, but JSON parse failed: {error}"
                    )),
                },
            }
        }
        Err(error) => ChatSmokeResponse {
            ok: false,
            base_url: trimmed.to_string(),
            chat_url,
            model: model.to_string(),
            response_preview: None,
            error: Some(error),
        },
    }
}

/// Validate a provider `base_url` at registration: scheme must be http/https,
/// and a host that resolves to a private/reserved network target (per
/// `rightkit_agent_policy::net_guard`) requires explicit `approve_private` opt-in.
/// Loopback (127.0.0.1, localhost, ::1) is the primary local-model-server use
/// case and is always allowed without a prompt — this gate is about silent
/// acceptance of a non-loopback private target (LAN, CGNAT, link-local/IMDS),
/// not about blocking the local-model feature itself.
///
/// Returns the `approved_private` decision to persist on the stored provider
/// route entry: `true` only when the target was private AND approved.
async fn validated_provider_base_url(base_url: &str, approve_private: bool) -> Result<bool, ()> {
    let trimmed = base_url.trim();
    let url = url::Url::parse(trimmed).map_err(|_| ())?;
    match url.scheme() {
        "http" | "https" => {}
        _ => return Err(()),
    }
    let host = url.host_str().ok_or(())?;
    let port = url.port_or_known_default().unwrap_or(80);
    match rightkit_agent_policy::resolve_then_reject_private_allow_loopback(host, port).await {
        Ok(()) => Ok(false), // loopback or public — not a private target
        Err(rightkit_agent_policy::NetGuardError::PrivateTarget(_)) => {
            if approve_private {
                Ok(true)
            } else {
                Err(())
            }
        }
        // Fail closed on every other guard outcome. A resolution failure, a
        // malformed trusted CIDR, or an empty answer is not evidence that the
        // target is public, so none of them may fall through to `Ok(false)`.
        Err(rightkit_agent_policy::NetGuardError::ResolutionFailed(_, _))
        | Err(rightkit_agent_policy::NetGuardError::NoPort)
        | Err(rightkit_agent_policy::NetGuardError::InvalidTrustedCidr(_))
        | Err(rightkit_agent_policy::NetGuardError::NoResolvedAddresses(_)) => Err(()),
    }
}

/// Audit 2026-11-05 platform H1. Returns why registering `provider_id` -> `base_url` would let a
/// stored key reach a host it was not saved for, or `None` when the write is safe:
/// - a built-in provider id must keep its registry host (its saved key is bound to it at runtime
///   by `saved_key_bound_to_route_host`, so another host would only produce a dead route);
/// - any other id with a saved key may only be rewritten at the origin its existing route in the
///   target layer already uses. A fresh route under a key-holding id is refused: the caller picks
///   a new id, or removes the saved key first and saves it again for the new host.
fn provider_key_redirect_refusal(
    provider_id: &str,
    base_url: &str,
    has_saved_key: bool,
    existing_route_base_url: Option<&str>,
) -> Option<String> {
    // Round 2 M4: the same binding the runtime applies, so the credential ids outside the
    // registry (`anthropic`, `google`, `mistral`, `antigravity`, `tinyfish`) are covered too.
    match crate::main_runtime::saved_key_binding(provider_id) {
        crate::main_runtime::SavedKeyBinding::Origin(origin) => {
            return (!crate::main_runtime::same_url_origin(&origin, base_url)).then(|| {
                format!(
                    "providerId '{provider_id}' is a built-in provider bound to {origin}; register a different providerId for another host"
                )
            });
        }
        crate::main_runtime::SavedKeyBinding::NoRoute => {
            return Some(format!(
                "providerId '{provider_id}' names a vendor credential that is never sent to a provider route; register a different providerId"
            ));
        }
        crate::main_runtime::SavedKeyBinding::Unbound => {}
    }
    if !has_saved_key {
        return None;
    }
    let same_origin = existing_route_base_url
        .is_some_and(|existing| crate::main_runtime::same_url_origin(existing, base_url));
    (!same_origin).then(|| {
        format!(
            "providerId '{provider_id}' has a saved API key that would be sent to a new host; use a different providerId, or remove that key first"
        )
    })
}

#[cfg(test)]
mod provider_key_redirect_tests {
    use super::provider_key_redirect_refusal;

    #[test]
    fn built_in_id_cannot_point_at_another_host() {
        assert!(provider_key_redirect_refusal(
            "openrouter",
            "https://attacker.example/v1",
            true,
            None
        )
        .is_some());
        // Refused even with no key saved yet: the key may be saved later.
        assert!(
            provider_key_redirect_refusal("openai", "http://127.0.0.1:9/v1", false, None).is_some()
        );
        assert!(provider_key_redirect_refusal(
            "openrouter",
            "https://openrouter.ai/api/v1",
            true,
            None
        )
        .is_none());
    }

    #[test]
    fn key_holding_custom_id_keeps_its_origin() {
        assert!(provider_key_redirect_refusal(
            "tinyfish",
            "https://attacker.example/v1",
            true,
            None
        )
        .is_some());
        assert!(provider_key_redirect_refusal(
            "my-lab",
            "https://attacker.example/v1",
            true,
            Some("https://lab.example/v1")
        )
        .is_some());
        assert!(provider_key_redirect_refusal(
            "my-lab",
            "https://lab.example/v2",
            true,
            Some("https://lab.example/v1")
        )
        .is_none());
    }

    /// Round 2 M4: credential ids outside the provider registry are bound too.
    #[test]
    fn non_registry_credential_ids_keep_their_vendor_origin() {
        for id in ["anthropic", "google", "mistral", "antigravity", "tinyfish"] {
            assert!(
                provider_key_redirect_refusal(id, "https://attacker.example/v1", false, None)
                    .is_some(),
                "{id}"
            );
        }
        assert!(provider_key_redirect_refusal(
            "anthropic",
            "https://api.anthropic.com/v1",
            true,
            None
        )
        .is_none());
    }

    #[test]
    fn keyless_custom_id_is_unrestricted() {
        assert!(
            provider_key_redirect_refusal("ollama", "http://127.0.0.1:11434/v1", false, None)
                .is_none()
        );
    }
}

fn validate_provider_route_id(id: &str) -> Result<(), String> {
    let trimmed = id.trim();
    if trimmed.is_empty() || trimmed != id {
        return Err("provider route id must be non-empty and trimmed".to_string());
    }
    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(
            "provider route id may contain only ASCII letters, digits, '.', '-', or '_'"
                .to_string(),
        );
    }
    Ok(())
}

fn workspace_path(q: &WorkspaceQuery) -> std::path::PathBuf {
    (!q.workspace_root.trim().is_empty())
        .then(|| std::path::PathBuf::from(q.workspace_root.trim()))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

fn workspace_config_path(workspace: &std::path::Path) -> std::path::PathBuf {
    // The file `ConfigLoader::update_workspace_from` and `save_workspace` write and
    // `load_files_for_workspace` reads (`<workspace>/{home}/config.toml`).
    workspace
        .join(rightkit_agent_config::state_dir_name())
        .join("config.toml")
}

fn load_workspace_config_only(
    workspace: &std::path::Path,
) -> Result<rightkit_agent_config::HostConfig, rightkit_agent_config::ConfigError> {
    let path = workspace_config_path(workspace);
    match std::fs::read_to_string(&path) {
        Ok(content) => rightkit_agent_config::ConfigLoader::new().load_str(&content),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(rightkit_agent_config::HostConfig::default())
        }
        Err(err) => Err(rightkit_agent_config::ConfigError::Io(err.to_string())),
    }
}

#[cfg(test)]
mod discovery_candidate_tests {
    use super::discovery_candidates;

    #[test]
    fn no_candidates_means_the_catalog_addresses() {
        let list = discovery_candidates(vec![]);
        assert_eq!(list, rightkit_agent_local_models::LOCAL_SERVER_ENDPOINTS);
        assert_eq!(discovery_candidates(vec!["  ".into()]), list);
    }

    #[test]
    fn candidates_are_trimmed_deduplicated_and_capped() {
        let list = discovery_candidates(vec![
            " http://a/v1 ".into(),
            "http://a/v1".into(),
            "http://b/v1".into(),
        ]);
        assert_eq!(list, ["http://a/v1", "http://b/v1"]);
        let many: Vec<String> = (0..40).map(|n| format!("http://h{n}/v1")).collect();
        assert_eq!(
            discovery_candidates(many).len(),
            super::DISCOVER_MAX_CANDIDATES
        );
    }
}

/// The first model an OpenAI-compatible local server lists, with every listed model. `Err` names
/// why the server did not answer (`{app} --oss` / `--local-provider` use it).
pub(crate) async fn local_server_models(
    base_url: &str,
    timeout: std::time::Duration,
) -> Result<Vec<String>, String> {
    let probe = probe_openai_models_endpoint_within(base_url, timeout).await;
    if probe.ok {
        Ok(probe.models)
    } else {
        Err(probe
            .error
            .unwrap_or_else(|| "the server did not answer".to_string()))
    }
}
