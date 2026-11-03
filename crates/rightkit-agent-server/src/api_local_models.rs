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
            "/local-models/chat-smoke",
            post(chat_smoke_local_model_endpoint),
        )
        .route("/local-models/add-provider", post(add_local_model_provider))
}

async fn get_local_model_hardware(
) -> Result<Json<rightkit_models::agent_local_models::HardwareReport>, StatusCode> {
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
        rightkit_models::agent_local_models::probe_hardware_with_adapter(
            adapter.as_ref(),
            &context,
        )
        .await,
    ))
}

async fn get_local_model_catalog(
) -> Result<Json<rightkit_models::agent_local_models::LocalModelCatalog>, StatusCode> {
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
    Ok(Json(rightkit_models::agent_local_models::model_catalog(
        rightkit_models::agent_local_models::probe_hardware_with_adapter(
            adapter.as_ref(),
            &context,
        )
        .await,
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
    suggestion: rightkit_models::agent_local_models::ProviderRouteSuggestion,
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
    let suggestion = rightkit_models::agent_local_models::provider_route_suggestion(
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
    let route = (
        suggestion.provider_id.clone(),
        rightkit_app_settings::agent_config::ProviderRouteConfig {
            display_name: suggestion.display_name.clone(),
            base_url: suggestion.base_url.clone(),
            model: suggestion.model.clone(),
            api_key_required: suggestion.api_key_required,
            api_key_env: None,
            approved_private,
        },
    );
    let set_default = body.set_default;
    let default_model = suggestion.model.clone();
    let apply = move |config: &mut rightkit_app_settings::agent_config::HostConfig| {
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
                rightkit_app_settings::agent_config::ConfigLoader::new()
                    .update_user_from(|_| load_user_config_only(), apply)
            })
            .await
            .unwrap_or_else(|e| {
                Err(rightkit_app_settings::agent_config::ConfigError::Io(
                    format!("config writer stopped: {e}"),
                ))
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
                let result = rightkit_app_settings::agent_config::ConfigLoader::new()
                    .update_workspace_from(
                        &target,
                        || {
                            load_workspace_config_only(&target).map_err(|err| {
                                read_failed.set(true);
                                err
                            })
                        },
                        apply,
                    );
                (result.map(|_| ()), read_failed.get())
            })
            .await
            .unwrap_or_else(|e| {
                (
                    Err(rightkit_app_settings::agent_config::ConfigError::Io(
                        format!("config writer stopped: {e}"),
                    )),
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
        .unwrap_or_else(|| "<RightKit state directory>/config.toml".to_string())
}

fn user_config_path() -> Option<std::path::PathBuf> {
    rightkit_app_settings::agent_config::state_root().map(|root| root.join("config.toml"))
}

fn load_user_config_only() -> Result<
    rightkit_app_settings::agent_config::HostConfig,
    rightkit_app_settings::agent_config::ConfigError,
> {
    let Some(path) = user_config_path() else {
        return Ok(rightkit_app_settings::agent_config::HostConfig::default());
    };
    match std::fs::read_to_string(&path) {
        Ok(content) => rightkit_app_settings::agent_config::ConfigLoader::new().load_str(&content),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(rightkit_app_settings::agent_config::HostConfig::default())
        }
        Err(err) => Err(rightkit_app_settings::agent_config::ConfigError::Io(
            err.to_string(),
        )),
    }
}

async fn probe_openai_models_endpoint(base_url: &str) -> ProbeEndpointResponse {
    let trimmed = base_url.trim().trim_end_matches('/');
    let models_url = format!("{trimmed}/models");
    let outcome = match rightkit_http::http_client::endpoint_client(
        &models_url,
        std::time::Duration::from_secs(5),
    ) {
        Ok(client) => {
            rightkit_http::http_client::send_any(
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
                Ok(json) if rightkit_http::http_client::is_success(status) => {
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
                "content": "Reply with exactly: coderight local model ok"
            }
        ],
        "max_tokens": 16,
        "temperature": 0,
        "stream": false
    });
    let outcome = match rightkit_http::http_client::endpoint_client(
        &chat_url,
        std::time::Duration::from_secs(10),
    ) {
        Ok(client) => match rightkit_http::Request::post(chat_url.clone()).json(&body) {
            Ok(request) => rightkit_http::http_client::send_any(&client, &request).await,
            Err(error) => Err(error.to_string()),
        },
        Err(error) => Err(error),
    };
    match outcome {
        Ok(resp) => {
            let status = resp.status;
            match resp.json::<serde_json::Value>() {
                Ok(json) if rightkit_http::http_client::is_success(status) => {
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
    workspace.join(".coderight").join("config.toml")
}

fn load_workspace_config_only(
    workspace: &std::path::Path,
) -> Result<
    rightkit_app_settings::agent_config::HostConfig,
    rightkit_app_settings::agent_config::ConfigError,
> {
    let path = workspace_config_path(workspace);
    match std::fs::read_to_string(&path) {
        Ok(content) => rightkit_app_settings::agent_config::ConfigLoader::new().load_str(&content),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(rightkit_app_settings::agent_config::HostConfig::default())
        }
        Err(err) => Err(rightkit_app_settings::agent_config::ConfigError::Io(
            err.to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tower::ServiceExt;

    fn temp_path(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("coderight-{label}-{nanos}"))
    }

    fn query_encode(raw: &str) -> String {
        raw.bytes()
            .flat_map(|byte| match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                    vec![byte as char]
                }
                b' ' => vec!['%', '2', '0'],
                other => format!("%{other:02X}").chars().collect(),
            })
            .collect()
    }

    #[tokio::test]
    async fn local_model_hardware_route_returns_cpu_threads() {
        let app = crate::api::build_router(DaemonState::new(""));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/local-models/hardware")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["cpuThreads"].as_u64().unwrap_or(0) >= 1);
    }

    #[test]
    fn local_models_openapi_components_cover_local_model_routes() {
        let components = local_models_openapi_components();
        assert!(components.contains_key("LocalModelHardwareReport"));
        assert!(components.contains_key("LocalModelCatalog"));
        assert!(components.contains_key("ProbeLocalModelEndpointRequest"));
        assert!(components.contains_key("ProbeLocalModelEndpointResponse"));
        assert!(components.contains_key("ChatSmokeLocalModelRequest"));
        assert!(components.contains_key("ChatSmokeLocalModelResponse"));
        assert!(components.contains_key("AddLocalModelProviderRequest"));
        assert!(components.contains_key("AddLocalModelProviderResponse"));
        assert_eq!(
            probe_local_model_request_schema()["$ref"],
            "#/components/schemas/ProbeLocalModelEndpointRequest"
        );
        assert_eq!(
            chat_smoke_local_model_response_schema()["$ref"],
            "#/components/schemas/ChatSmokeLocalModelResponse"
        );
        assert_eq!(
            add_local_model_provider_response_schema()["$ref"],
            "#/components/schemas/AddLocalModelProviderResponse"
        );
        assert_eq!(
            components["ProbeLocalModelEndpointResponse"]["properties"]["models"]["items"]["type"],
            "string"
        );
    }

    #[tokio::test]
    async fn local_model_catalog_route_returns_recommendations() {
        let app = crate::api::build_router(DaemonState::new(""));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/local-models/catalog")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(!json["recommendations"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn local_model_add_provider_returns_route_suggestion() {
        let workspace = temp_path("local-model-provider");
        std::fs::create_dir_all(&workspace).unwrap();
        let app = crate::api::build_router(DaemonState::new(""));
        let uri = format!(
            "/local-models/add-provider?workspaceRoot={}",
            query_encode(&workspace.display().to_string())
        );
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"providerId":"ollama-local","baseUrl":"http://127.0.0.1:11434/v1","model":"qwen2.5-coder:7b","setDefault":true}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["apiKeyRequired"], false);
        assert_eq!(json["providerId"], "ollama-local");
        // Persisted, not live: the daemon resolves its engine provider once at
        // boot, so a newly written route cannot be active in this process. The
        // old `applied: true` here asserted a success the runtime never
        // delivered — a caller that trusted it saw an inert route with no error.
        assert_eq!(json["applied"], false);
        assert_eq!(json["requiresRestart"], true);
        assert_eq!(json["activeInRunningDaemon"], false);
        assert_eq!(json["scope"], "workspace");
        assert_eq!(json["setDefault"], true);

        let config = std::fs::read_to_string(workspace.join(".coderight").join("config.toml"))
            .expect("workspace config");
        assert!(config.contains("[model]"), "{config}");
        assert!(
            config.contains("default-model = \"qwen2.5-coder:7b\""),
            "{config}"
        );
        assert!(
            config.contains("[provider-routes.ollama-local]"),
            "{config}"
        );
        assert!(
            config.contains("base-url = \"http://127.0.0.1:11434/v1\""),
            "{config}"
        );
        assert_eq!(json["approvedPrivate"], false, "loopback is never private");
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[tokio::test]
    async fn local_model_add_provider_rejects_metadata_endpoint_without_approval() {
        let workspace = temp_path("local-model-metadata-denied");
        std::fs::create_dir_all(&workspace).unwrap();
        let app = crate::api::build_router(DaemonState::new(""));
        let uri = format!(
            "/local-models/add-provider?workspaceRoot={}",
            query_encode(&workspace.display().to_string())
        );
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"providerId":"metadata-probe","baseUrl":"http://169.254.169.254/v1","model":"x"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(!workspace.join(".coderight").join("config.toml").exists());
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[tokio::test]
    async fn local_model_add_provider_allows_private_target_with_explicit_approval() {
        let workspace = temp_path("local-model-private-approved");
        std::fs::create_dir_all(&workspace).unwrap();
        let app = crate::api::build_router(DaemonState::new(""));
        let uri = format!(
            "/local-models/add-provider?workspaceRoot={}",
            query_encode(&workspace.display().to_string())
        );
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"providerId":"lan-model","baseUrl":"http://192.168.1.50:8080/v1","model":"x","approvePrivate":true}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["approvedPrivate"], true);
        let config = std::fs::read_to_string(workspace.join(".coderight").join("config.toml"))
            .expect("workspace config");
        assert!(config.contains("approved-private = true"), "{config}");
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[tokio::test]
    async fn local_model_mutations_reject_lower_trust_sources() {
        let workspace = temp_path("local-model-untrusted");
        std::fs::create_dir_all(&workspace).unwrap();
        let app = crate::api::build_router(DaemonState::new(""));
        let workspace_query = query_encode(&workspace.display().to_string());
        let cases = [
            (
                "/local-models/probe".to_string(),
                serde_json::json!({"baseUrl": "http://127.0.0.1:9/v1"}),
            ),
            (
                "/local-models/chat-smoke".to_string(),
                serde_json::json!({
                    "baseUrl": "http://127.0.0.1:9/v1",
                    "model": "lower-trust-model"
                }),
            ),
            (
                format!("/local-models/add-provider?workspaceRoot={workspace_query}"),
                serde_json::json!({
                    "providerId": "lower-trust-local",
                    "baseUrl": "http://127.0.0.1:11434/v1",
                    "model": "lower-trust-model",
                    "setDefault": true
                }),
            ),
        ];

        for (uri, body) in cases {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(uri.clone())
                        .header("content-type", "application/json")
                        .header("x-coderight-source-trust", "toolUntrusted")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{uri}");
        }

        assert!(!workspace.join(".coderight").join("config.toml").exists());
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[tokio::test]
    async fn local_model_probe_reads_openai_models_endpoint() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/v1/models")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"object":"list","data":[{"id":"qwen2.5-coder:7b"},{"id":"local-coder"}]}"#,
            )
            .create_async()
            .await;

        let app = crate::api::build_router(DaemonState::new(""));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/local-models/probe")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"baseUrl":"{}/v1"}}"#,
                        server.url()
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["ok"], true);
        assert_eq!(json["modelCount"], 2);
        assert_eq!(json["models"][0], "qwen2.5-coder:7b");
    }

    #[tokio::test]
    async fn local_model_chat_smoke_posts_openai_chat_completion() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/v1/chat/completions")
            .match_header("content-type", "application/json")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "model": "qwen2.5-coder:7b",
                "stream": false,
                "messages": [
                    {
                        "role": "user",
                        "content": "Reply with exactly: coderight local model ok"
                    }
                ]
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":"chatcmpl-local","choices":[{"message":{"role":"assistant","content":"coderight local model ok"}}]}"#,
            )
            .create_async()
            .await;

        let app = crate::api::build_router(DaemonState::new(""));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/local-models/chat-smoke")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"baseUrl":"{}/v1","model":"qwen2.5-coder:7b"}}"#,
                        server.url()
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["ok"], true);
        assert_eq!(json["model"], "qwen2.5-coder:7b");
        assert_eq!(json["responsePreview"], "coderight local model ok");
    }
}
