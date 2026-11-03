//! Generic route assembly plus the navigation/control/telemetry handlers.
//! Annotation handlers live in `annotations.rs`; this module wires them into
//! the same router so the mounted path list is unchanged.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use rightkit_agent_tools::coding_tools::browser::BrowserNetworkDecisionV1;
use rightkit_agent_tools::coding_tools::browser_fetch::wrap_untrusted_web_text;
use serde_json::{json, Value};
use std::sync::Arc;

use super::annotations::{annotation_handler, annotation_send_handler, list_annotations_handler};
use super::policy::workspace_ssrf_check;
use super::services::{BrowserSession, BrowserWorkspaceServices, BrowserWorkspaceState};
use super::types::{
    capability_denied_response as types_capability_denied_response,
    host_policy_approval_required_response, host_policy_blocked_response, internal_error,
    missing_field, named_shared_profile_unsupported_response, now_millis, paused_response,
    png_dimensions, workspace_error, BrowserActionActor, BrowserActionEntry, CapabilityVerdict,
    CloseRequest, ConsoleRequest, HostPolicyDecision, HostPolicyRequest, NavigateRequest,
    OpenWorkspaceRequest, ProfileConsentRequest, ReloadRequest, ScreenshotEnvelope,
    ScreenshotRequest, ViewportInfo, ViewportRequest, ZoomRequest, WORKSPACE_USER_OWNER,
};

/// Re-exported so `annotations.rs` can reuse the exact same error body
/// without duplicating the `workspace_error` call.
pub(crate) fn capability_denied_response(verdict: &CapabilityVerdict) -> (StatusCode, Json<Value>) {
    types_capability_denied_response(verdict)
}

/// Bundle the fragment carries through axum's `State<>` extractor. Two
/// fields: the trait object (Agent 4's impl) and the fragment's annotation
/// store. Routes never reach for a central `DaemonState`.
pub(crate) struct RoutesState<S: BrowserWorkspaceServices> {
    pub(crate) services: Arc<S>,
    pub(crate) state: BrowserWorkspaceState,
}

impl<S: BrowserWorkspaceServices> Clone for RoutesState<S> {
    fn clone(&self) -> Self {
        Self {
            services: Arc::clone(&self.services),
            state: self.state.clone(),
        }
    }
}

/// Routes the central router can mount. The standalone fragment does NOT
/// register itself; Agent 4 wires
/// `Router::new().merge(browser_workspace_routes(services))` into the
/// central `build_router`.
pub fn browser_workspace_routes<S: BrowserWorkspaceServices>(
    services: Arc<S>,
    state: BrowserWorkspaceState,
) -> Router {
    Router::new()
        // session list + ephemeral default
        .route(
            "/sessions/{id}/browser/workspace",
            get(get_workspace_handler),
        )
        .route(
            "/sessions/{id}/browser/workspace/open",
            post(open_session_handler),
        )
        .route(
            "/sessions/{id}/browser/workspace/close",
            post(close_session_handler),
        )
        // navigation
        .route(
            "/sessions/{id}/browser/workspace/navigate",
            post(navigate_handler),
        )
        .route(
            "/sessions/{id}/browser/workspace/reload",
            post(reload_handler),
        )
        .route("/sessions/{id}/browser/workspace/back", post(back_handler))
        .route(
            "/sessions/{id}/browser/workspace/forward",
            post(forward_handler),
        )
        .route(
            "/sessions/{id}/browser/workspace/viewport",
            post(viewport_handler).get(get_viewport_handler),
        )
        .route("/sessions/{id}/browser/workspace/zoom", post(zoom_handler))
        .route(
            "/sessions/{id}/browser/workspace/screenshot",
            post(screenshot_handler),
        )
        // live page signals
        .route(
            "/sessions/{id}/browser/workspace/console",
            get(console_get_handler).post(console_handler),
        )
        .route(
            "/sessions/{id}/browser/workspace/network",
            get(network_handler),
        )
        .route(
            "/sessions/{id}/browser/workspace/errors",
            get(errors_get_handler).post(errors_handler),
        )
        .route(
            "/sessions/{id}/browser/workspace/dev-servers",
            get(dev_servers_handler),
        )
        // operator-vs-agent state
        .route(
            "/sessions/{id}/browser/workspace/action-log",
            get(action_log_handler),
        )
        .route(
            "/sessions/{id}/browser/workspace/pause",
            post(pause_handler),
        )
        .route(
            "/sessions/{id}/browser/workspace/resume",
            post(resume_handler),
        )
        .route(
            "/sessions/{id}/browser/workspace/take-control",
            post(take_control_handler),
        )
        .route(
            "/sessions/{id}/browser/workspace/release-control",
            post(release_control_handler),
        )
        // structured annotations + the send seam Agent 4 consumes
        .route(
            "/sessions/{id}/browser/workspace/annotations",
            post(annotation_handler).get(list_annotations_handler),
        )
        .route(
            "/sessions/{id}/browser/workspace/annotations/{ann_id}/send",
            post(annotation_send_handler),
        )
        // host policy — additive after the SSRF guard
        .route(
            "/sessions/{id}/browser/workspace/host-policy",
            get(host_policy_get_handler).post(host_policy_set_handler),
        )
        // profile consent — ephemeral default, named shared opt-in
        .route(
            "/sessions/{id}/browser/workspace/profile",
            get(profile_get_handler).post(profile_consent_handler),
        )
        .with_state(RoutesState { services, state })
}

// =============================================================================
// Handlers
// =============================================================================

async fn get_workspace_handler<S: BrowserWorkspaceServices>(
    State(state): State<RoutesState<S>>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let verdict = state.services.capability_gate(&session_id);
    if !verdict.allowed {
        return Err(capability_denied_response(&verdict));
    }
    let paused = state.services.is_paused(&session_id);
    let profile_kind = state.services.profile_kind(&session_id);
    let profile_label = state.services.profile_label(&session_id);
    let action_log_len = state.services.action_log_len(&session_id);
    let viewport = state.services.viewport(&session_id);
    let zoom = state.services.zoom(&session_id);

    let store = state
        .services
        .browser_store(&session_id)
        .ok_or_else(|| super::types::session_not_found(&session_id))?;

    let sessions: Vec<Arc<BrowserSession>> = store.lock().await.values().cloned().collect();
    let mut summaries = Vec::with_capacity(sessions.len());
    for browser in sessions {
        let console_error_count = browser
            .console_lines()
            .await
            .iter()
            .filter(|line| line.level == "error" || line.level == "exception")
            .count();
        summaries.push(json!({
            "sessionId": browser.id,
            "ownerSessionId": browser.owner_session_id(),
            "mode": browser.mode(),
            "profile": browser.profile(),
            "controlLease": browser.control_lease(),
            "url": browser.url().await,
            "title": browser.title().await,
            "consoleErrorCount": console_error_count,
            "zoom": zoom,
        }));
    }

    Ok(Json(json!({
        "paused": paused,
        "profileKind": match profile_kind {
            super::types::BrowserProfileKind::Ephemeral => "ephemeral",
            super::types::BrowserProfileKind::NamedShared => "named_shared",
        },
        "profileLabel": profile_label,
        "viewport": viewport,
        "actionLogLen": action_log_len,
        "sessions": summaries,
    })))
}

async fn open_session_handler<S: BrowserWorkspaceServices>(
    State(state): State<RoutesState<S>>,
    Path(session_id): Path<String>,
    Json(req): Json<OpenWorkspaceRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let verdict = state.services.capability_gate(&session_id);
    if !verdict.allowed {
        return Err(capability_denied_response(&verdict));
    }
    let decision = state.services.host_policy_decision(&session_id, &req.url);
    let navigation_trust = state.services.navigation_trust(&session_id);
    workspace_ssrf_check(
        &req.url,
        &navigation_trust,
        decision == Some(HostPolicyDecision::Allow),
    )
    .await?;
    if let Ok(parsed) = url::Url::parse(&req.url) {
        if let Some(host) = parsed.host_str() {
            match decision {
                Some(HostPolicyDecision::Block) => {
                    return Err(host_policy_blocked_response(host));
                }
                Some(HostPolicyDecision::Ask) => {
                    return Err(host_policy_approval_required_response(host));
                }
                Some(HostPolicyDecision::Allow) | None => {}
            }
        }
    }

    if req.name.is_some() {
        return Err(named_shared_profile_unsupported_response());
    }

    let browser_session_id = state
        .services
        .open_browser_session(&session_id, &req.url)
        .await
        .map_err(|e| internal_error(&format!("Browser launch failed: {e}")))?;

    state.services.push_action(
        &session_id,
        BrowserActionEntry {
            at_ms: now_millis(),
            actor: BrowserActionActor::Workspace,
            kind: "open".into(),
            detail: format!("opened {} as {browser_session_id}", req.url),
        },
    );

    Ok(Json(json!({
        "sessionId": browser_session_id,
        "url": req.url,
    })))
}

async fn close_session_handler<S: BrowserWorkspaceServices>(
    State(state): State<RoutesState<S>>,
    Path(session_id): Path<String>,
    Json(req): Json<Option<CloseRequest>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let verdict = state.services.capability_gate(&session_id);
    if !verdict.allowed {
        return Err(capability_denied_response(&verdict));
    }
    let browser_session_id = req
        .and_then(|c| c.session_id)
        .ok_or_else(|| missing_field("sessionId"))?;
    state
        .services
        .close_browser_session(&session_id, &browser_session_id)
        .await
        .map_err(|e| internal_error(&e))?;
    state.services.push_action(
        &session_id,
        BrowserActionEntry {
            at_ms: now_millis(),
            actor: BrowserActionActor::Workspace,
            kind: "close".into(),
            detail: browser_session_id.clone(),
        },
    );
    Ok(Json(json!({"closed": browser_session_id})))
}

async fn navigate_handler<S: BrowserWorkspaceServices>(
    State(state): State<RoutesState<S>>,
    Path(session_id): Path<String>,
    Json(req): Json<NavigateRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let verdict = state.services.capability_gate(&session_id);
    if !verdict.allowed {
        return Err(capability_denied_response(&verdict));
    }
    if state.services.is_paused(&session_id) {
        return Err(paused_response());
    }
    let decision = state.services.host_policy_decision(&session_id, &req.url);
    let navigation_trust = state.services.navigation_trust(&session_id);
    workspace_ssrf_check(
        &req.url,
        &navigation_trust,
        decision == Some(HostPolicyDecision::Allow),
    )
    .await?;
    if let Some(host) = url::Url::parse(&req.url)
        .ok()
        .and_then(|u| u.host_str().map(|s| s.to_ascii_lowercase()))
    {
        match decision {
            Some(HostPolicyDecision::Block) => {
                return Err(host_policy_blocked_response(&host));
            }
            Some(HostPolicyDecision::Ask) => {
                return Err(host_policy_approval_required_response(&host));
            }
            Some(HostPolicyDecision::Allow) | None => {}
        }
    }

    let browser_session_id = req.browser_session_id.clone();
    let observation = state
        .services
        .navigate_browser_session_with_observation(&session_id, &browser_session_id, &req.url)
        .await
        .map_err(|e| internal_error(&e))?;
    state.services.push_action(
        &session_id,
        BrowserActionEntry {
            at_ms: now_millis(),
            actor: BrowserActionActor::Agent,
            kind: "navigate".into(),
            detail: req.url.clone(),
        },
    );
    Ok(Json(
        json!({"navigated": req.url, "observation": observation}),
    ))
}

async fn reload_handler<S: BrowserWorkspaceServices>(
    State(state): State<RoutesState<S>>,
    Path(session_id): Path<String>,
    Json(req): Json<Option<ReloadRequest>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let verdict = state.services.capability_gate(&session_id);
    if !verdict.allowed {
        return Err(capability_denied_response(&verdict));
    }
    if state.services.is_paused(&session_id) {
        return Err(paused_response());
    }
    let browser_session_id = req
        .and_then(|r| r.session_id)
        .ok_or_else(|| missing_field("sessionId"))?;
    let observation = state
        .services
        .reload_browser_session_with_observation(&session_id, &browser_session_id)
        .await
        .map_err(|error| internal_error(&error))?;
    state.services.push_action(
        &session_id,
        BrowserActionEntry {
            at_ms: now_millis(),
            actor: BrowserActionActor::Agent,
            kind: "reload".into(),
            detail: browser_session_id.clone(),
        },
    );
    Ok(Json(
        json!({"reloaded": browser_session_id, "observation": observation}),
    ))
}

async fn back_handler<S: BrowserWorkspaceServices>(
    State(state): State<RoutesState<S>>,
    Path(session_id): Path<String>,
    Json(req): Json<Option<ReloadRequest>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    navigate_history_handler(state, session_id, req, false).await
}

async fn forward_handler<S: BrowserWorkspaceServices>(
    State(state): State<RoutesState<S>>,
    Path(session_id): Path<String>,
    Json(req): Json<Option<ReloadRequest>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    navigate_history_handler(state, session_id, req, true).await
}

async fn navigate_history_handler<S: BrowserWorkspaceServices>(
    state: RoutesState<S>,
    session_id: String,
    req: Option<ReloadRequest>,
    forward: bool,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let verdict = state.services.capability_gate(&session_id);
    if !verdict.allowed {
        return Err(capability_denied_response(&verdict));
    }
    if state.services.is_paused(&session_id) {
        return Err(paused_response());
    }
    let browser_session_id = req
        .and_then(|request| request.session_id)
        .ok_or_else(|| missing_field("sessionId"))?;
    let observation = state
        .services
        .navigate_browser_history_with_observation(&session_id, &browser_session_id, forward)
        .await
        .map_err(|error| internal_error(&error))?;
    let kind = if forward { "forward" } else { "back" };
    state.services.push_action(
        &session_id,
        BrowserActionEntry {
            at_ms: now_millis(),
            actor: BrowserActionActor::Human,
            kind: kind.to_string(),
            detail: browser_session_id.clone(),
        },
    );
    Ok(Json(json!({
        "direction": kind,
        "browserSessionId": browser_session_id,
        "observation": observation,
    })))
}

async fn get_viewport_handler<S: BrowserWorkspaceServices>(
    State(state): State<RoutesState<S>>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let verdict = state.services.capability_gate(&session_id);
    if !verdict.allowed {
        return Err(capability_denied_response(&verdict));
    }
    Ok(Json(
        json!({"viewport": state.services.viewport(&session_id)}),
    ))
}

async fn viewport_handler<S: BrowserWorkspaceServices>(
    State(state): State<RoutesState<S>>,
    Path(session_id): Path<String>,
    Json(req): Json<ViewportRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let verdict = state.services.capability_gate(&session_id);
    if !verdict.allowed {
        return Err(capability_denied_response(&verdict));
    }
    if req.width == 0 || req.height == 0 {
        return Err(workspace_error(
            StatusCode::BAD_REQUEST,
            "ssrf_refused",
            "Viewport width/height must be positive.".into(),
        ));
    }
    if req.device_scale_factor <= 0.0 {
        return Err(workspace_error(
            StatusCode::BAD_REQUEST,
            "ssrf_refused",
            "deviceScaleFactor must be > 0.".into(),
        ));
    }
    let info = ViewportInfo {
        width: req.width,
        height: req.height,
        device_scale_factor: req.device_scale_factor,
    };
    state
        .services
        .apply_viewport(&session_id, &info)
        .await
        .map_err(|error| internal_error(&error))?;
    state.services.set_viewport(&session_id, info.clone());
    state.services.push_action(
        &session_id,
        BrowserActionEntry {
            at_ms: now_millis(),
            actor: BrowserActionActor::Workspace,
            kind: "viewport".into(),
            detail: format!(
                "{}x{} @{}",
                info.width, info.height, info.device_scale_factor
            ),
        },
    );
    Ok(Json(json!({"viewport": info})))
}

async fn zoom_handler<S: BrowserWorkspaceServices>(
    State(state): State<RoutesState<S>>,
    Path(session_id): Path<String>,
    Json(req): Json<ZoomRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let verdict = state.services.capability_gate(&session_id);
    if !verdict.allowed {
        return Err(capability_denied_response(&verdict));
    }
    if !(0.25..=5.0).contains(&req.zoom) {
        return Err(workspace_error(
            StatusCode::BAD_REQUEST,
            "ssrf_refused",
            "zoom must be between 0.25 and 5.0.".into(),
        ));
    }
    state
        .services
        .apply_zoom(&session_id, req.zoom)
        .await
        .map_err(|error| internal_error(&error))?;
    state.services.set_zoom(&session_id, req.zoom);
    state.services.push_action(
        &session_id,
        BrowserActionEntry {
            at_ms: now_millis(),
            actor: BrowserActionActor::Workspace,
            kind: "zoom".into(),
            detail: format!("{:.2}", req.zoom),
        },
    );
    Ok(Json(json!({"zoom": req.zoom})))
}

async fn screenshot_handler<S: BrowserWorkspaceServices>(
    State(state): State<RoutesState<S>>,
    Path(session_id): Path<String>,
    Json(req): Json<Option<ScreenshotRequest>>,
) -> Result<Json<ScreenshotEnvelope>, (StatusCode, Json<Value>)> {
    let verdict = state.services.capability_gate(&session_id);
    if !verdict.allowed {
        return Err(capability_denied_response(&verdict));
    }
    let browser_session_id = req
        .and_then(|r| r.session_id)
        .ok_or_else(|| missing_field("sessionId"))?;
    let bytes = state
        .services
        .screenshot_browser_session(&session_id, &browser_session_id)
        .await
        .map_err(|error| internal_error(&error))?;
    let url = state
        .services
        .browser_session_url(&session_id, &browser_session_id)
        .await
        .map_err(|error| internal_error(&error))?;
    let (width, height) = png_dimensions(&bytes).unwrap_or((0, 0));
    let encoded = rightkit_crypto::base64_encode(&bytes);
    let wrapped = wrap_untrusted_web_text("browser_workspace.screenshot", &encoded);
    state.services.push_action(
        &session_id,
        BrowserActionEntry {
            at_ms: now_millis(),
            actor: BrowserActionActor::Workspace,
            kind: "screenshot".into(),
            detail: format!("{} bytes", bytes.len()),
        },
    );
    Ok(Json(ScreenshotEnvelope {
        session_id: browser_session_id,
        url,
        captured_at_ms: now_millis(),
        mime: "image/png",
        width,
        height,
        bytes: bytes.len(),
        data_base64: encoded,
        untrusted_wrapped: wrapped,
    }))
}

async fn console_handler<S: BrowserWorkspaceServices>(
    State(state): State<RoutesState<S>>,
    Path(session_id): Path<String>,
    Json(req): Json<Option<ConsoleRequest>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    console_response(&state, &session_id, req).await
}

async fn console_get_handler<S: BrowserWorkspaceServices>(
    State(state): State<RoutesState<S>>,
    Path(session_id): Path<String>,
    Query(req): Query<ConsoleRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    console_response(&state, &session_id, Some(req)).await
}

async fn console_response<S: BrowserWorkspaceServices>(
    state: &RoutesState<S>,
    session_id: &str,
    req: Option<ConsoleRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let verdict = state.services.capability_gate(session_id);
    if !verdict.allowed {
        return Err(capability_denied_response(&verdict));
    }
    let browser_session_id = req
        .and_then(|r| r.session_id)
        .ok_or_else(|| missing_field("sessionId"))?;
    let lines = state
        .services
        .console_lines(session_id, &browser_session_id)
        .await
        .map_err(|error| internal_error(&error))?;
    let trimmed: Vec<_> = lines
        .iter()
        .rev()
        .map(|l| {
            format!(
                "[{}] {}",
                l.level,
                wrap_untrusted_web_text("browser_workspace.console", &l.text)
            )
        })
        .collect();
    Ok(Json(json!({
        "sessionId": browser_session_id,
        "lines": trimmed,
    })))
}

async fn network_handler<S: BrowserWorkspaceServices>(
    State(state): State<RoutesState<S>>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let verdict = state.services.capability_gate(&session_id);
    if !verdict.allowed {
        return Err(capability_denied_response(&verdict));
    }
    // CDP-level network interception requires
    // Network.setRequestIdInterceptor, which the existing browser tool
    // surface does not yet wire. The standalone fragment exposes an empty
    // envelope so the operator UI can render the empty state honestly.
    Ok(Json(json!({
        "sessionId": session_id,
        "available": false,
        "note": "Network capture is unavailable because the live browser tool does not retain CDP network events.",
        "entries": [],
    })))
}

async fn errors_handler<S: BrowserWorkspaceServices>(
    State(state): State<RoutesState<S>>,
    Path(session_id): Path<String>,
    Json(req): Json<Option<ConsoleRequest>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    errors_response(&state, &session_id, req).await
}

async fn errors_get_handler<S: BrowserWorkspaceServices>(
    State(state): State<RoutesState<S>>,
    Path(session_id): Path<String>,
    Query(req): Query<ConsoleRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    errors_response(&state, &session_id, Some(req)).await
}

async fn errors_response<S: BrowserWorkspaceServices>(
    state: &RoutesState<S>,
    session_id: &str,
    req: Option<ConsoleRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let verdict = state.services.capability_gate(session_id);
    if !verdict.allowed {
        return Err(capability_denied_response(&verdict));
    }
    let browser_session_id = req
        .and_then(|r| r.session_id)
        .ok_or_else(|| missing_field("sessionId"))?;
    let lines = state
        .services
        .console_lines(session_id, &browser_session_id)
        .await
        .map_err(|error| internal_error(&error))?;
    let errors: Vec<_> = lines
        .iter()
        .filter(|l| l.level == "error" || l.level == "exception")
        .rev()
        .map(|l| {
            json!({
                "level": l.level,
                "text": wrap_untrusted_web_text("browser_workspace.errors", &l.text),
            })
        })
        .collect();
    Ok(Json(json!({
        "sessionId": browser_session_id,
        "errors": errors,
    })))
}

async fn dev_servers_handler<S: BrowserWorkspaceServices>(
    State(state): State<RoutesState<S>>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let verdict = state.services.capability_gate(&session_id);
    if !verdict.allowed {
        return Err(capability_denied_response(&verdict));
    }
    let candidates: Vec<u16> = vec![3000, 5173, 8000, 8080, 4200];
    let mut discovered: Vec<Value> = Vec::new();
    for port in candidates {
        let url = format!("http://127.0.0.1:{port}/");
        if probe_loopback(&url).await {
            discovered.push(json!({"port": port, "url": url}));
        }
    }
    Ok(Json(json!({
        "sessionId": session_id,
        "servers": discovered,
    })))
}

pub(crate) async fn probe_loopback(url: &str) -> bool {
    // Redirects are not followed (`daemon_client_probe`), so a 3xx is the answer; only
    // the status line is read.
    match rightkit_http::http_client::daemon_client_probe(
        url,
        std::time::Duration::from_millis(400),
        std::time::Duration::from_millis(400),
    ) {
        Ok(client) => client
            .send_any(&rightkit_http::Request::get(url).headers_only())
            .await
            .map(|r| (200..400).contains(&r.status))
            .unwrap_or(false),
        Err(_) => false,
    }
}

async fn action_log_handler<S: BrowserWorkspaceServices>(
    State(state): State<RoutesState<S>>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let verdict = state.services.capability_gate(&session_id);
    if !verdict.allowed {
        return Err(capability_denied_response(&verdict));
    }
    let actions = state.services.action_log(&session_id);
    Ok(Json(json!({
        "sessionId": session_id,
        "actionLogLen": actions.len(),
        "actions": actions,
    })))
}

async fn pause_handler<S: BrowserWorkspaceServices>(
    State(state): State<RoutesState<S>>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let verdict = state.services.capability_gate(&session_id);
    if !verdict.allowed {
        return Err(capability_denied_response(&verdict));
    }
    state.services.set_paused(&session_id, true).await;
    state.services.push_action(
        &session_id,
        BrowserActionEntry {
            at_ms: now_millis(),
            actor: BrowserActionActor::Human,
            kind: "pause".into(),
            detail: "agent paused".into(),
        },
    );
    Ok(Json(json!({"paused": true})))
}

async fn resume_handler<S: BrowserWorkspaceServices>(
    State(state): State<RoutesState<S>>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let verdict = state.services.capability_gate(&session_id);
    if !verdict.allowed {
        return Err(capability_denied_response(&verdict));
    }
    state.services.set_paused(&session_id, false).await;
    state.services.push_action(
        &session_id,
        BrowserActionEntry {
            at_ms: now_millis(),
            actor: BrowserActionActor::Human,
            kind: "resume".into(),
            detail: "agent resumed".into(),
        },
    );
    Ok(Json(json!({"paused": false})))
}

async fn take_control_handler<S: BrowserWorkspaceServices>(
    State(state): State<RoutesState<S>>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let verdict = state.services.capability_gate(&session_id);
    if !verdict.allowed {
        return Err(capability_denied_response(&verdict));
    }
    let transition_lock = state.state.control_transition_lock();
    let _transition = transition_lock.lock().await;
    let owner = WORKSPACE_USER_OWNER.to_string();
    let mut generations = Vec::new();
    if let Some(store) = state.services.browser_store(&session_id) {
        let browsers: Vec<_> = store.lock().await.values().cloned().collect();
        for browser in &browsers {
            if browser
                .control_lease()
                .owner
                .as_deref()
                .is_some_and(|current| current != owner)
            {
                return Err(internal_error(
                    "browser control lease is already owned by another actor",
                ));
            }
        }
        let mut acquired: Vec<Arc<BrowserSession>> = Vec::new();
        for browser in &browsers {
            let already_owned = browser.control_lease().owner.as_deref() == Some(owner.as_str());
            match browser.take_human_control(owner.clone()).await {
                Ok(lease) => generations.push(json!({
                    "browserSessionId": browser.id,
                    "generation": lease.generation,
                })),
                Err(error) => {
                    for acquired_browser in acquired.into_iter().rev() {
                        let _ = acquired_browser.release_human_control(&owner).await;
                    }
                    return Err(internal_error(&error));
                }
            }
            if !already_owned {
                acquired.push(browser.clone());
            }
        }
    }
    state.services.set_paused(&session_id, true).await;
    state.services.push_action(
        &session_id,
        BrowserActionEntry {
            at_ms: now_millis(),
            actor: BrowserActionActor::Human,
            kind: "take_control".into(),
            detail: "operator took control".into(),
        },
    );
    Ok(Json(json!({
        "paused": true,
        "control": "human",
        "leases": generations,
    })))
}

async fn release_control_handler<S: BrowserWorkspaceServices>(
    State(state): State<RoutesState<S>>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let verdict = state.services.capability_gate(&session_id);
    if !verdict.allowed {
        return Err(capability_denied_response(&verdict));
    }
    let transition_lock = state.state.control_transition_lock();
    let _transition = transition_lock.lock().await;
    let owner = WORKSPACE_USER_OWNER.to_string();
    let mut generations = Vec::new();
    if let Some(store) = state.services.browser_store(&session_id) {
        let browsers: Vec<_> = store.lock().await.values().cloned().collect();
        for browser in &browsers {
            if browser.control_lease().owner.as_deref() != Some(owner.as_str()) {
                return Err(internal_error(
                    "browser control lease is owned by another actor",
                ));
            }
        }
        let mut released: Vec<Arc<BrowserSession>> = Vec::new();
        for browser in &browsers {
            match browser.release_human_control(&owner).await {
                Ok(lease) => generations.push(json!({
                    "browserSessionId": browser.id,
                    "generation": lease.generation,
                })),
                Err(error) => {
                    for released_browser in released.into_iter().rev() {
                        let _ = released_browser.take_human_control(owner.clone()).await;
                    }
                    return Err(internal_error(&error));
                }
            }
            released.push(browser.clone());
        }
    }
    state.services.set_paused(&session_id, false).await;
    state.services.push_action(
        &session_id,
        BrowserActionEntry {
            at_ms: now_millis(),
            actor: BrowserActionActor::Human,
            kind: "release_control".into(),
            detail: "operator released control".into(),
        },
    );
    Ok(Json(json!({
        "paused": false,
        "control": "agent",
        "leases": generations,
    })))
}

async fn host_policy_get_handler<S: BrowserWorkspaceServices>(
    State(state): State<RoutesState<S>>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let verdict = state.services.capability_gate(&session_id);
    if !verdict.allowed {
        return Err(capability_denied_response(&verdict));
    }
    let policy: Vec<Value> = state
        .services
        .host_policy_snapshot(&session_id)
        .into_iter()
        .map(|(host, decision)| json!({"host": host, "decision": decision}))
        .collect();
    let navigation_trust = state.services.navigation_trust(&session_id);
    Ok(Json(json!({
        "sessionId": session_id,
        "policy": policy,
        "trustedPrivateNetworkDomains": navigation_trust.trusted_domains,
        "trustedCidrs": navigation_trust.trusted_cidrs,
    })))
}

async fn host_policy_set_handler<S: BrowserWorkspaceServices>(
    State(state): State<RoutesState<S>>,
    Path(session_id): Path<String>,
    Json(req): Json<HostPolicyRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let verdict = state.services.capability_gate(&session_id);
    if !verdict.allowed {
        return Err(capability_denied_response(&verdict));
    }
    let host = req.host.trim().to_ascii_lowercase();
    if host.is_empty() {
        return Err(missing_field("host"));
    }
    if host.contains('/') || host.contains(' ') {
        return Err(workspace_error(
            StatusCode::BAD_REQUEST,
            "ssrf_refused",
            "host must be a bare hostname.".into(),
        ));
    }
    if req.trusted_private_network == Some(true) && req.decision != HostPolicyDecision::Allow {
        return Err(workspace_error(
            StatusCode::BAD_REQUEST,
            "ssrf_refused",
            "A private-network domain exception requires an allow decision.".into(),
        ));
    }
    let mut navigation_trust = state.services.navigation_trust(&session_id);
    if let Some(trust_domain) = req.trusted_private_network {
        navigation_trust
            .trusted_domains
            .retain(|domain| domain != &host);
        if trust_domain {
            navigation_trust.trusted_domains.push(host.clone());
        }
    }
    if let Some(trusted_cidrs) = req.trusted_cidrs.clone() {
        navigation_trust.trusted_cidrs = trusted_cidrs;
    }
    navigation_trust.validate().map_err(|error| {
        workspace_error(
            StatusCode::BAD_REQUEST,
            "ssrf_refused",
            format!("Invalid trusted navigation exception: {error}"),
        )
    })?;
    state
        .services
        .set_host_policy(&session_id, host.clone(), req.decision);
    state
        .services
        .set_navigation_trust(&session_id, navigation_trust.clone());
    if let Some(store) = state.services.browser_store(&session_id) {
        let browsers: Vec<_> = store.lock().await.values().cloned().collect();
        for browser in browsers {
            browser
                .set_navigation_trust(navigation_trust.clone())
                .map_err(|error| {
                    internal_error(&format!(
                        "Could not apply trusted navigation exception: {error}"
                    ))
                })?;
            browser.set_host_policy(
                host.clone(),
                match req.decision {
                    HostPolicyDecision::Allow => BrowserNetworkDecisionV1::Allow,
                    HostPolicyDecision::Ask => BrowserNetworkDecisionV1::Ask,
                    HostPolicyDecision::Block => BrowserNetworkDecisionV1::Block,
                },
            );
        }
    }
    state.services.push_action(
        &session_id,
        BrowserActionEntry {
            at_ms: now_millis(),
            actor: BrowserActionActor::Human,
            kind: "host_policy".into(),
            detail: format!("{host} -> {:?}", req.decision),
        },
    );
    Ok(Json(json!({
        "host": host,
        "decision": req.decision,
        "trustedPrivateNetwork": req.trusted_private_network,
        "trustedCidrs": navigation_trust.trusted_cidrs,
    })))
}

async fn profile_get_handler<S: BrowserWorkspaceServices>(
    State(state): State<RoutesState<S>>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let verdict = state.services.capability_gate(&session_id);
    if !verdict.allowed {
        return Err(capability_denied_response(&verdict));
    }
    Ok(Json(json!({
        "kind": state.services.profile_kind(&session_id),
        "label": state.services.profile_label(&session_id),
    })))
}

async fn profile_consent_handler<S: BrowserWorkspaceServices>(
    State(state): State<RoutesState<S>>,
    Path(session_id): Path<String>,
    Json(req): Json<ProfileConsentRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let verdict = state.services.capability_gate(&session_id);
    if !verdict.allowed {
        return Err(capability_denied_response(&verdict));
    }
    match req.kind {
        super::types::BrowserProfileKind::NamedShared => {
            if req.label.as_deref().map(str::trim).unwrap_or("").is_empty() {
                return Err(missing_field("label"));
            }
            return Err(named_shared_profile_unsupported_response());
        }
        super::types::BrowserProfileKind::Ephemeral => {}
    }
    state
        .services
        .set_profile(&session_id, req.kind, req.label.clone());
    state.services.push_action(
        &session_id,
        BrowserActionEntry {
            at_ms: now_millis(),
            actor: BrowserActionActor::Human,
            kind: "profile".into(),
            detail: format!("{:?}", req.kind),
        },
    );
    Ok(Json(json!({
        "kind": req.kind,
        "label": req.label,
    })))
}
