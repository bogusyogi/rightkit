//! Repro + regression coverage for audit B-1 (client-presence reaper
//! `blocking_lock` hazard).
//!
//! `any_session_running` used to call `storage.blocking_lock()` directly
//! inside the async client-presence reaper loop with no `spawn_blocking`
//! wrapper at all — the single call site glm flagged as "the odd one out"
//! against the 37 other call sites that correctly go through
//! `tokio::task::spawn_blocking`. Calling `blocking_lock()` synchronously
//! from a plain async fn panics unconditionally (it is not even guarded by
//! `block_in_place`, so a current-thread runtime is not even required to
//! trigger it — any async task context does).

#[cfg(test)]
mod client_presence_reaper_tests {
    use crate::api::SharedStorage;
    use crate::main_runtime::{any_session_running, DaemonState};

    fn unique_temp_dir(label: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir()
            .join("coderight-test-tmp")
            .join(format!("coderight-{label}-{nanos}"))
    }

    /// Before the fix, this call (from a plain, non-multi-thread async test)
    /// panicked because `any_session_running` called `blocking_lock()`
    /// synchronously inside an `async fn` with no `spawn_blocking` wrapper.
    #[tokio::test]
    async fn any_session_running_does_not_panic_under_current_thread_runtime() {
        let dir = unique_temp_dir("client-presence-reaper");
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("presence.db");

        let storage =
            rightkit_agent_engine_storage::Storage::open(&db_path).expect("open fresh db");
        let shared = SharedStorage::new(storage);
        let state = DaemonState::new("test-token").with_storage(shared.clone());

        let live = state.create_session(
            "presence-session",
            None,
            dir.to_str().unwrap(),
            "Presence session",
            None,
        );
        assert_eq!(live.id, "presence-session");

        // Wait for the fire-and-forget initial persist (B-1 fix in
        // api_state_impl_04.rs) to land before flipping status.
        for _ in 0..50 {
            let exists = {
                let s = shared.lock().await;
                s.get_session("presence-session").ok().flatten().is_some()
            };
            if exists {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        {
            let s = shared.lock().await;
            s.update_session_status("presence-session", "running", "2026-07-04T00:00:00Z")
                .expect("status update should succeed");
        }

        // A live turn handle, rather than a potentially stale database row,
        // is the authoritative in-flight signal.
        let turn_guard = live.live_turn_handle.begin();
        let running = any_session_running(&state).await;
        assert!(running, "an active live turn must be detected as in-flight");
        drop(turn_guard);

        // A crash-stranded `running` projection must not pin the daemon. The
        // next probe repairs it to idle after the live guard is gone.
        assert!(!any_session_running(&state).await);
        let repaired_status = {
            let s = shared.lock().await;
            s.get_session("presence-session")
                .expect("read repaired session")
                .expect("persisted session")
                .status
        };
        assert_eq!(repaired_status, "idle");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn any_session_running_false_when_no_sessions_running() {
        let dir = unique_temp_dir("client-presence-reaper-idle");
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("presence.db");

        let storage =
            rightkit_agent_engine_storage::Storage::open(&db_path).expect("open fresh db");
        let shared = SharedStorage::new(storage);
        let state = DaemonState::new("test-token").with_storage(shared.clone());

        let live = state.create_session(
            "idle-session",
            None,
            dir.to_str().unwrap(),
            "Idle session",
            None,
        );
        assert_eq!(live.id, "idle-session");

        for _ in 0..50 {
            let exists = {
                let s = shared.lock().await;
                s.get_session("idle-session").ok().flatten().is_some()
            };
            if exists {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let running = any_session_running(&state).await;
        assert!(
            !running,
            "a freshly created ('idle') session must not read as in-flight"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn any_session_running_false_with_no_storage_and_no_live_sessions() {
        let state = DaemonState::new("test-token");
        assert!(!any_session_running(&state).await);
    }

    #[tokio::test]
    async fn active_turn_is_authoritative_without_storage() {
        let dir = unique_temp_dir("client-presence-no-storage");
        std::fs::create_dir_all(&dir).unwrap();
        let state = DaemonState::new("test-token");
        let live = state.create_session(
            "active-no-storage",
            None,
            dir.to_str().unwrap(),
            "Active session",
            None,
        );

        assert!(!any_session_running(&state).await);
        let turn_guard = live.live_turn_handle.begin();
        assert!(any_session_running(&state).await);
        drop(turn_guard);
        assert!(!any_session_running(&state).await);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Lane-5 keep-alive: with `keep_daemon_resident` set, the reaper must
    /// skip its self-exit decision even after every client has detached.
    /// Without the flag and no live session, the owner's exit rule reports
    /// "shut down" once the linger window has elapsed.
    #[test]
    fn exit_rule_reports_shutdown_after_the_last_client_detaches_and_the_linger_elapses() {
        let state = DaemonState::new("test-token");
        let start = std::time::Instant::now();
        let grant = state
            .presence
            .attach("cli", 1, None, start)
            .expect("attach");
        assert!(state.presence.detach(&grant.client_id, start));
        let linger = std::time::Duration::from_secs(state.presence.config().linger_seconds);

        assert!(!state.presence.should_shut_down(start, false));
        assert!(state.presence.should_shut_down(start + linger, false));
        assert!(!state.presence.should_shut_down(start + linger, true));
    }

    #[tokio::test]
    async fn scheduler_helper_returns_true_when_unpaused_schedule_has_future_occurrence() {
        let dir = unique_temp_dir("keep-resident-sched");
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("presence.db");
        let storage =
            rightkit_agent_engine_storage::Storage::open(&db_path).expect("open fresh db");
        let shared = SharedStorage::new(storage);
        let mut state = DaemonState::new("test-token").with_storage(shared.clone());
        state.licensed_features = std::sync::Arc::new(std::sync::Mutex::new({
            let mut s = std::collections::HashSet::new();
            s.insert("schedules".to_string());
            s
        }));

        // Persist a schedule file in the live workspace's `.coderight/schedules.json`.
        // The helper uses `scheduled_workspace_roots` which walks session
        // workspaces; we have no live sessions yet, so write directly into
        // the workspace list via a session we create.
        let workspace = dir.join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(workspace.join(".coderight")).unwrap();
        let schedule_json = serde_json::json!({
            "future": {
                "id": "future",
                "cron_expr": "* * * * *",
                "time_zone": "UTC",
                "paused": false,
                "prompt": "ping",
                "mode": "agent",
            }
        })
        .to_string();
        std::fs::write(
            workspace.join(".coderight").join("schedules.json"),
            schedule_json,
        )
        .unwrap();

        let live = state.create_session(
            "sched-session",
            None,
            workspace.to_str().unwrap(),
            "Sched session",
            None,
        );
        assert_eq!(live.id, "sched-session");

        // Now the helper should find the future occurrence.
        assert!(crate::scheduler::has_enabled_schedules_with_future_occurrence(&state));

        // A paused or no-occurrence schedule returns false.
        let paused_json = serde_json::json!({
            "future": {
                "id": "future",
                "cron_expr": "* * * * *",
                "time_zone": "UTC",
                "paused": true,
                "prompt": "ping",
                "mode": "agent",
            }
        })
        .to_string();
        std::fs::write(
            workspace.join(".coderight").join("schedules.json"),
            paused_json,
        )
        .unwrap();
        assert!(!crate::scheduler::has_enabled_schedules_with_future_occurrence(&state));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
