use super::*;

pub fn build_router(state: DaemonState) -> Router {
    let auth_token = state.token.clone();
    let gateway_routes = crate::api_drivers::driver_capability_routes().with_state(state.clone());
    let browser_workspace_routes =
        crate::api_browser_workspace::daemon_browser_workspace_routes(state.clone());
    // Workflow control plane (WP-09 / ADR-0021): the daemon mounts the
    // workflow REST surface only when the workflow feature gate is on AND a
    // per-router state was constructed at startup (storage available). When
    // the gate is off, the routes are intentionally absent — the
    // `api_not_found` fallback 404s the request — so the desktop UI sees
    // the surface as "not installed". The per-handler 423 in
    // `api_workflows::routes` is the defence-in-depth fallback for the case
    // where the gate is on but state was not built.
    let workflow_routes =
        if state.workflows_feature.is_enabled() && state.workflow_api_state.is_some() {
            Some(crate::api_workflows::build_router())
        } else {
            None
        };
    // CORS, the per-class request-rate limit and the Host and Origin checks of the owner
    // listener run in the loopback host (rightkit_daemon::loopback) before this router.
    // The origin allow-list lives in `PRODUCTION_ORIGINS` and `allowed_origins()` below;
    // loopback_adapter.rs reads them for the host config.
    // The Tauri webview always reaches the daemon via tauri://localhost. The
    // localhost:1422/5173 origins are dev-only (Vite dev server + headless QA)
    // and are compiled OUT of release binaries entirely (audit A-8 SPEC 2).
    // Same-origin requests from the embedded web UI carry no Origin header.

    let mut management_routes = Router::new()
        .merge(health_routes())
        .merge(crate::api_accounts::account_routes())
        .merge(crate::api_adapt::adapt_routes())
        .merge(api_contract_routes())
        .merge(crate::api_run_changes::run_changes_routes())
        .merge(crate::api_workspace::workspace_routes())
        .merge(crate::api_delivery::delivery_routes())
        .merge(crate::api_worktree_pool::worktree_pool_routes())
        .merge(crate::api_drivers::driver_routes())
        .merge(crate::api_environments::environments_routes())
        .merge(rightkit_agent_engine_attention::attention_routes())
        .merge(browser_workspace_routes)
        .merge(sessions_routes())
        .merge(compaction_archive_routes())
        .merge(context_fold_routes())
        .merge(session_handoff_routes())
        .merge(permission_routes())
        .merge(tool_schema_routes())
        .merge(tool_invocation_routes())
        .merge(toolset_routes())
        .merge(trigger_routes())
        .merge(spec_routes())
        .merge(execution_routes())
        .merge(recipe_routes())
        .merge(local_model_routes())
        .merge(crate::hooks::hook_routes())
        .route("/internal/takeover", post(takeover_handler))
        .route("/instructions", get(get_instructions_handler))
        .route(
            "/context/discovery",
            get(super::api_session_context::get_capability_discovery_handler),
        )
        .merge(memory_routes())
        .merge(notification_routes())
        .merge(remote_routes())
        .merge(settings_routes())
        .merge(crate::api_provider_auth::provider_auth_routes())
        .merge(command_routes())
        .merge(mcp_routes())
        .merge(lsp_routes())
        .merge(plugin_routes())
        .merge(skills_routes())
        .merge(coordination_routes())
        .merge(crate::api_fs_watch::router())
        .merge(crate::api_external_observations::routes())
        .merge(crate::api_task_list::task_list_routes())
        .merge(schedule_routes())
        .merge(runtime_routes())
        .merge(benchmark_routes())
        .merge(crate::api_benchmarks::soak_routes::soak_routes())
        .merge(client_routes());
    if let Some(routes) = workflow_routes {
        management_routes = management_routes.merge(routes);
    }
    // Route sets only some hosts offer; each is mounted only when its host option is on, and
    // `api_contract` documents it under the same option. `data/option-gated-routes.json` lists
    // every route of each option, and `api_contract` drops them from the contract from that file.
    // VG-007 peer-team coordination: a durable shared task board, mailbox and mission log across
    // independent sessions and processes, distinct from `coordination_routes()`'s parent-child
    // subagent team definitions (see api_teams.rs module doc).
    if crate::host_options::peer_teams() {
        management_routes = management_routes.merge(crate::api_teams::peer_team_routes());
    }
    // One restricted background review request against the configured provider (a provider call).
    if crate::host_options::membrane_background_review() {
        management_routes =
            management_routes.merge(crate::api_membrane_background::membrane_background_routes());
    }
    if crate::host_options::cloud_workspaces() {
        management_routes = management_routes
            .merge(crate::api_cloud_forge::cloud_forge_routes())
            .merge(crate::api_cloud_workspaces::cloud_workspace_routes());
    }
    if crate::host_options::browser_consent_routes() {
        management_routes =
            management_routes.merge(crate::api_browser_consent::browser_consent_routes());
    }
    if crate::host_options::license_status_route() {
        management_routes =
            management_routes.merge(crate::api_license_status::license_status_routes());
    }
    if crate::host_options::guest_shares() {
        management_routes = management_routes.merge(crate::guest_shares::share_routes());
    }
    let management_routes = management_routes
        .with_state(state)
        .layer(middleware::from_fn(
            rightkit_memory_client::api_memright::memory_request_context,
        ))
        // Gate the API behind the per-launch token (it also resolves guest bearers).
        .layer(middleware::from_fn_with_state(
            auth_token,
            crate::auth::require_token,
        ));

    let routes = management_routes.merge(gateway_routes);
    // The browser client's static files: no data, so no bearer (api_web.rs); none when the
    // host supplied no files.
    let routes = if crate::api_web::web_client_enabled() {
        routes.merge(crate::api_web::web_routes())
    } else {
        routes
    };
    routes.fallback(api_not_found)
}

// ── Handlers ──────────────────────────────────────────────────────────────────

async fn takeover_handler(State(state): State<DaemonState>) -> StatusCode {
    let _ = state.shutdown_tx.send(true);
    StatusCode::NO_CONTENT
}

/// The origins the release daemon accepts: the three Tauri webview origins and nothing else
/// (client API R23).
pub(crate) const PRODUCTION_ORIGINS: [&str; 3] = [
    "tauri://localhost",
    "http://tauri.localhost",
    "https://tauri.localhost",
];

/// The origin list the loopback adapter hands to the loopback host's origin policy (CORS and the
/// Origin check). The release profile compiles dev origins out. These are the webview origins of
/// the Tauri shell, not product names.
pub(crate) fn allowed_origins() -> Vec<&'static str> {
    PRODUCTION_ORIGINS.to_vec()
}

async fn api_not_found() -> StatusCode {
    StatusCode::NOT_FOUND
}
