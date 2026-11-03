/// Probe an existing `~/.coderight/daemon.port` daemon (if any) and refuse to
/// start a second one on the same database. Different db path, no port file,
/// unreachable daemon, or an unhealthy response all proceed — see
/// `api_clients::startup_guard_decision` for the pure decision logic.
async fn run_startup_guard(db_path: &str) -> Result<(), String> {
    let Some(state_dir) = rightkit_app_settings::agent_config::state_root() else {
        return Ok(());
    };
    let port_file = state_dir.join("daemon.port");
    let Ok(content) = std::fs::read_to_string(&port_file) else {
        return Ok(());
    };
    let Some(info) = LoopbackRecord::parse(&content) else {
        return Ok(());
    };
    let url = format!("http://127.0.0.1:{}", info.port);
    let client = match rightkit_http::http_client::daemon_client_probe(
        &url,
        std::time::Duration::from_millis(500),
        std::time::Duration::from_millis(500),
    ) {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };
    let health = match client
        .send_any(&rightkit_http::Request::get(format!("{url}/health")))
        .await
    {
        Ok(resp) if rightkit_http::http_client::is_success(resp.status) => {
            resp.json::<serde_json::Value>().ok()
        }
        _ => None,
    };
    crate::api_clients::startup_guard_decision(health.as_ref(), db_path, &url)
}

/// Remove `<state root>/daemon.port` on graceful shutdown, but only while it still holds the record
/// this process published: a successor's record is never deleted by a daemon late to exit.
fn remove_daemon_port_file() {
    if let (Some(state_dir), Some(record)) = (
        rightkit_app_settings::agent_config::state_root(),
        DAEMON_RECORD.get(),
    ) {
        record.remove_if_ours(&state_dir.join("daemon.port"));
    }
}

/// Background sweep for the client-presence lifecycle (U-F). Every
/// heartbeat period, reaps expired client leases, then asks the owner's
/// `Presence::should_shut_down` (which tracks how long the count has been
/// zero) with a conservative in-flight-run signal (any live session whose
/// persisted status is `"running"`).
fn spawn_client_presence_reaper(state: DaemonState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(
            state.presence.config().heartbeat_seconds,
        ));
        loop {
            interval.tick().await;
            // R12/CR-38: one daemon wakeup. This loop fires on the heartbeat
            // interval whether or not anything needs reaping, so it is exactly
            // the idle cost the S0 baseline is meant to expose -- a daemon that
            // wakes constantly with no clients attached is the thing the
            // wakeups-per-second column exists to catch.
            rightkit_logs::agent_telemetry::record_wakeup();
            crate::api_clients::reap_expired_clients(&state);
            let any_running = any_session_running(&state).await;
            // Honor the keep-daemon-resident setting OR any enabled schedule
            // with a future cron occurrence: headless schedules must keep
            // firing while the desktop app is closed.
            //
            // The settings guard MUST drop before the schedule probe: the
            // probe walks sessions/storage and scans cron windows, and
            // holding a sync mutex across it starves every request handler
            // that needs settings (std mutexes are not fair). The probe
            // itself only runs when shutdown is otherwise imminent.
            let keep_toggle =
                { lock_recover(&state.settings, "state.settings").keep_daemon_resident };
            let would_shut_down = state
                .presence
                .should_shut_down(std::time::Instant::now(), any_running);
            let keep_resident = keep_toggle
                || (would_shut_down
                    && crate::scheduler::has_enabled_schedules_with_future_occurrence(&state));
            if !keep_resident && would_shut_down {
                eprintln!("[serve] last client detached and linger window elapsed; shutting down");
                let _ = state.shutdown_tx.send(true);
                return;
            }
            // Re-evaluate next tick — both the toggle and the schedule list
            // are dynamic; we cannot pin forever based on a stale snapshot.
        }
    });
}

/// Authoritative in-flight signal: true if any live session currently owns an
/// engine-turn handle. Persistent `running` rows are a durable projection, not
/// authority: a crash can strand one forever, so inactive rows are reconciled
/// back to `idle` while this probe runs.
///
/// Runs the storage read via `spawn_blocking` (never `blocking_lock()` called
/// directly from an async task, and never `block_in_place` — both panic on a
/// current-thread Tokio runtime; `spawn_blocking` works on every flavor).
async fn any_session_running(state: &DaemonState) -> bool {
    let live_handles = state
        .list_sessions()
        .into_iter()
        .map(|session| (session.id.clone(), session.live_turn_handle.clone()))
        .collect::<std::collections::HashMap<_, _>>();
    let Some(storage) = state.storage.clone() else {
        return live_handles.values().any(|handle| handle.is_active());
    };
    tokio::task::spawn_blocking(move || {
        let store = storage.blocking_lock();
        if let Ok(rows) = store.list_sessions() {
            for row in rows.into_iter().filter(|row| row.status == "running") {
                let active = live_handles
                    .get(&row.id)
                    .is_some_and(|handle| handle.is_active());
                if !active {
                    // Recheck immediately before repair so a turn that began
                    // after the snapshot is not overwritten back to idle.
                    let still_inactive = live_handles
                        .get(&row.id)
                        .is_none_or(|handle| !handle.is_active());
                    if still_inactive {
                        if let Err(error) = store.update_session_status(&row.id, "idle", &now_iso())
                        {
                            tracing::warn!(
                                session_id = %row.id,
                                error = %error,
                                "failed to reconcile orphaned running session status"
                            );
                        }
                    }
                }
            }
        }
        live_handles.values().any(|handle| handle.is_active())
    })
    .await
    .unwrap_or(false)
}
