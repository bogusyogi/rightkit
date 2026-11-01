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
    // Only the Tauri webview and local dev servers should reach the daemon.
    // `allow_origin(Any)` let any webpage the user visited talk to the daemon
    // (it binds to 127.0.0.1, but browsers will still send the request).
    // Same-origin requests from the embedded web UI carry no Origin header and
    // are unaffected by this list.
    // The Tauri webview always reaches the daemon via tauri://localhost. The
    // localhost:1422/5173 origins are dev-only (Vite dev server + headless QA)
    // and are now compiled OUT of release binaries entirely (audit A-8 SPEC 2):
    // the previous runtime RIGHTKIT_ALLOW_DEV_ORIGINS env-var check had no
    // effect in release either, so we replaced it with a single cfg gate to
    // make the dev/release split impossible to override at runtime.
    // `mut` is used only by the test-mode extend below; in release the
    // dev origins are compiled out, so the binding is never mutated there.
    #[allow(unused_mut)]
    let mut origins = vec![
        "tauri://localhost",
        "http://tauri.localhost",
        "https://tauri.localhost",
    ];
    #[cfg(any(test, feature = "test-mode"))]
    {
        origins.extend([
            "http://localhost:1422",
            "http://127.0.0.1:1422",
            "http://localhost:1432",
            "http://127.0.0.1:1432",
            "http://localhost:5173",
            "http://127.0.0.1:5173",
        ]);
    }
    let allowed_origins = origins
        .into_iter()
        .filter_map(|o| o.parse().ok())
        .collect::<Vec<_>>();
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(allowed_origins))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::PATCH,
        ])
        .allow_headers(Any);

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
        .merge(crate::api_membrane_background::membrane_background_routes())
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
        // VG-007 peer-team coordination: durable shared task board, mailbox,
        // and mission log across independent sessions/processes — distinct
        // from `coordination_routes()`'s parent-child subagent team
        // definitions (see api_teams.rs module doc).
        .merge(crate::api_fs_watch::router())
        .merge(crate::api_external_observations::routes())
        .merge(crate::api_task_list::task_list_routes())
        .merge(crate::api_teams::peer_team_routes())
        .merge(schedule_routes())
        .merge(runtime_routes())
        .merge(benchmark_routes())
        .merge(crate::api_benchmarks::soak_routes::soak_routes())
        .merge(client_routes());
    if let Some(routes) = workflow_routes {
        management_routes = management_routes.merge(routes);
    }
    let management_routes = management_routes
        .with_state(state)
        .layer(middleware::from_fn(
            rightkit_memory::api_memright::memory_request_context,
        ))
        // Gate the API behind the per-launch token. CORS stays outermost so
        // preflight is handled first.
        .layer(middleware::from_fn_with_state(
            auth_token,
            crate::auth::require_token,
        ));

    management_routes
        .merge(gateway_routes)
        .layer(middleware::from_fn_with_state(
            crate::auth::RateLimiter::new(),
            crate::auth::rate_limit,
        ))
        .layer(cors)
        .fallback(api_not_found)
}

// ── Handlers ──────────────────────────────────────────────────────────────────

async fn takeover_handler(State(state): State<DaemonState>) -> StatusCode {
    let _ = state.shutdown_tx.send(true);
    StatusCode::NO_CONTENT
}

async fn api_not_found() -> StatusCode {
    StatusCode::NOT_FOUND
}

#[cfg(test)]
mod release_origin_tests {
    /// Recreate the same origin list `build_router` would hand to the CORS
    /// layer for the current profile. If you change the origin list above,
    /// update this function — they're coupled by the cfg gate (audit A-8 SPEC 2).
    fn expected_origins() -> Vec<&'static str> {
        let mut origins = vec![
            "tauri://localhost",
            "http://tauri.localhost",
            "https://tauri.localhost",
        ];
        #[cfg(any(test, feature = "test-mode"))]
        {
            origins.extend([
                "http://localhost:1422",
                "http://127.0.0.1:1422",
                "http://localhost:1432",
                "http://127.0.0.1:1432",
                "http://localhost:5173",
                "http://127.0.0.1:5173",
            ]);
        }
        origins
    }

    #[test]
    fn release_profile_has_only_production_origins() {
        // This is the audit-A-8 SPEC 2 invariant: dev origins (Vite +
        // headless QA) MUST NOT appear in a release build. The cfg gate is
        // the only thing preventing them — the old env-var shortcut is gone.
        let origins = expected_origins();
        if cfg!(any(test, feature = "test-mode")) {
            assert!(
                origins.len() >= 7,
                "debug profile should include dev origins"
            );
        } else {
            assert_eq!(
                origins.len(),
                3,
                "release profile must list exactly the 3 production origins"
            );
            assert!(origins.contains(&"tauri://localhost"));
            assert!(origins.contains(&"http://tauri.localhost"));
            assert!(origins.contains(&"https://tauri.localhost"));
            for dev in ["http://localhost:1422", "http://127.0.0.1:5173"] {
                assert!(
                    !origins.contains(&dev),
                    "dev origin {dev} leaked into release CORS list"
                );
            }
        }
    }
}
