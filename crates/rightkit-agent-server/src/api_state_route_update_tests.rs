//! Stage 1A route-update coverage: `update_session_route` must validate
//! before effects, commit live + durable route state as one transaction,
//! refuse while a turn is admitted/active/queued, and refuse qualified
//! (structured `inferenceSelection`) sessions the legacy consumer cannot
//! commit to.
//!
//! Authored under the Stage 1A route assignment; NOT executed by the worker
//! (execution is parent-owned). Run with:
//! `rustright cargo test -p coderight --bin coderight route_update_tests`

#[cfg(test)]
mod route_update_tests {
    use crate::api::api_session_utils::update_session_route;
    use crate::api::api_state::lock_recover;
    use crate::api::{DaemonState, EngineConfig, SharedStorage};

    fn engine_cfg() -> EngineConfig {
        rightkit_agent_tools::coding_tools::process_resource_authority::install_process_resource_profile(
            rightkit_agent_tools::coding_tools::process_resource_authority::ResourceProfile::Expanded,
        )
        .expect("route fixture process authority");
        EngineConfig {
            api_key: "test-key".into(),
            model_id: "engine-default-model".into(),
            base_url: "https://engine.invalid/v1".into(),
            fallback_models: Vec::new(),
        }
    }

    fn unique_temp_dir(label: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir()
            .join("coderight-test-tmp")
            .join(format!("coderight-{label}-{nanos}"))
    }

    async fn stored_config(storage: &SharedStorage, id: &str) -> serde_json::Value {
        let row = {
            let s = storage.lock().await;
            s.get_session(id).expect("session row readable")
        }
        .expect("session row exists");
        serde_json::from_str(&row.config_json).expect("config_json parses")
    }

    /// A committed switch must land the SAME provider/model/effort in live
    /// state and in config_json — the atomic durable rewrite is what resume
    /// consumes.
    #[tokio::test(flavor = "multi_thread")]
    async fn route_update_commits_live_and_durable_state_together() {
        let dir = unique_temp_dir("route-commit");
        std::fs::create_dir_all(&dir).unwrap();
        let storage = SharedStorage::new(
            rightkit_agent_engine_storage::Storage::open(dir.join("s.db")).unwrap(),
        );
        let state = DaemonState::new("")
            .with_storage(storage.clone())
            .with_engine(engine_cfg());
        let live = state.create_session(
            "route-commit-session",
            None,
            dir.to_str().unwrap(),
            "Route commit",
            None,
        );
        // The initial row write is fire-and-forget; wait for it so the
        // durable commit is exercised rather than skipped.
        let mut persisted = false;
        for _ in 0..50 {
            if storage
                .lock()
                .await
                .get_session("route-commit-session")
                .ok()
                .flatten()
                .is_some()
            {
                persisted = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(persisted, "precondition: session row persisted");

        let applied = update_session_route(
            &state,
            "route-commit-session",
            "minimax/minimax-m3",
            None,
            Some("high"),
        )
        .await
        .expect("idle session route update commits");
        assert!(applied, "live session with engine route applies live");

        assert_eq!(
            lock_recover(&live.model, "session.model").as_str(),
            "minimax/minimax-m3"
        );
        assert_eq!(
            lock_recover(&live.provider_id, "session.provider_id").as_deref(),
            Some("minimax"),
            "model-only update persists the provider the resolution consumed"
        );
        assert_eq!(
            lock_recover(&live.effort, "session.effort").as_deref(),
            Some("high")
        );
        let route = live.model_route.as_ref().expect("live route");
        let route = lock_recover(route, "session.model_route");
        assert_eq!(route.model_id, "MiniMax-M3");
        assert_eq!(route.effort.as_deref(), Some("high"));
        drop(route);

        let cfg = stored_config(&storage, "route-commit-session").await;
        assert_eq!(cfg["model"], "minimax/minimax-m3");
        assert_eq!(cfg["providerId"], "minimax");
        assert_eq!(cfg["effort"], "high");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An unresolvable explicit provider must fail before ANY state moves.
    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_explicit_provider_rejects_before_effects() {
        let dir = unique_temp_dir("route-reject");
        std::fs::create_dir_all(&dir).unwrap();
        let storage = SharedStorage::new(
            rightkit_agent_engine_storage::Storage::open(dir.join("s.db")).unwrap(),
        );
        let state = DaemonState::new("")
            .with_storage(storage.clone())
            .with_engine(engine_cfg());
        let live = state.create_session(
            "route-reject-session",
            None,
            dir.to_str().unwrap(),
            "Route reject",
            None,
        );
        let before_model = lock_recover(&live.model, "session.model").clone();

        let result = update_session_route(
            &state,
            "route-reject-session",
            "some-model",
            Some("definitely-not-a-provider"),
            None,
        )
        .await;
        assert_eq!(result, Err(axum::http::StatusCode::BAD_REQUEST));
        assert_eq!(
            lock_recover(&live.model, "session.model").as_str(),
            before_model.as_str(),
            "rejected update must not mutate live model"
        );
        assert!(
            lock_recover(&live.provider_id, "session.provider_id").as_deref()
                != Some("definitely-not-a-provider"),
            "rejected update must not pin a provider"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A turn admitted but not yet consumed (pending_admissions > 0) is busy:
    /// the switch must refuse rather than land between admission and the
    /// first provider request.
    #[tokio::test(flavor = "multi_thread")]
    async fn queued_admission_blocks_route_update_until_consumed() {
        let dir = unique_temp_dir("route-busy-queue");
        std::fs::create_dir_all(&dir).unwrap();
        let storage = SharedStorage::new(
            rightkit_agent_engine_storage::Storage::open(dir.join("s.db")).unwrap(),
        );
        let state = DaemonState::new("")
            .with_storage(storage.clone())
            .with_engine(engine_cfg());
        let live = state.create_session(
            "route-busy-session",
            None,
            dir.to_str().unwrap(),
            "Route busy",
            None,
        );

        // The initial row write is fire-and-forget; wait for it so the durable
        // commit after the queue drains is not racing the row insert.
        let mut persisted = false;
        for _ in 0..50 {
            if storage
                .lock()
                .await
                .get_session("route-busy-session")
                .ok()
                .flatten()
                .is_some()
            {
                persisted = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(persisted, "precondition: session row persisted");

        live.pending_admissions
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let result =
            update_session_route(&state, "route-busy-session", "model-b", None, None).await;
        assert_eq!(
            result,
            Err(axum::http::StatusCode::CONFLICT),
            "a queued admitted turn is busy"
        );

        live.pending_admissions
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        let result =
            update_session_route(&state, "route-busy-session", "model-b", None, None).await;
        assert!(result.is_ok(), "switch commits once the queue drains");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End-to-end regression for the shared-counter fix: a real admission
    /// through `LiveSession::try_admit_route_bound_text` is dequeued and
    /// decremented by the production engine task, so once the turn settles
    /// (provider failure on a closed loopback port resolves deterministically)
    /// the route update must succeed. Before the fix, LiveSession carried a
    /// private counter the engine never decremented, so one real turn left
    /// pending_admissions > 0 forever and every route update returned 409.
    #[tokio::test(flavor = "multi_thread")]
    async fn route_update_permitted_after_admitted_turn_settles() {
        let dir = unique_temp_dir("route-settled");
        std::fs::create_dir_all(&dir).unwrap();
        let mut config = engine_cfg();
        // Closed loopback port: deterministic provider failure without external DNS.
        config.base_url = "http://127.0.0.1:9/v1".into();
        let state = DaemonState::new("").with_engine(config);
        let live = state.create_session(
            "route-settled-session",
            None,
            dir.to_str().unwrap(),
            "Route settled",
            None,
        );

        live.try_admit_route_bound_text("trigger one real engine turn".to_string())
            .await
            .expect("admission lands on the live engine channel");

        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                let pending = live
                    .pending_admissions
                    .load(std::sync::atomic::Ordering::Acquire);
                if pending == 0 && !live.live_turn_handle.is_active() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("engine consumed and settled the admitted turn");

        let result = update_session_route(
            &state,
            "route-settled-session",
            "minimax/minimax-m3",
            None,
            None,
        )
        .await;
        assert_eq!(
            result,
            Ok(true),
            "route update must succeed once the real turn fully settles"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A live in-flight turn is busy regardless of the admission counter.
    #[tokio::test(flavor = "multi_thread")]
    async fn active_turn_blocks_route_update() {
        let dir = unique_temp_dir("route-busy-live");
        std::fs::create_dir_all(&dir).unwrap();
        let state = DaemonState::new("").with_engine(engine_cfg());
        let live = state.create_session(
            "route-live-busy-session",
            None,
            dir.to_str().unwrap(),
            "Route live busy",
            None,
        );
        let _turn_guard = live.live_turn_handle.begin();
        let result =
            update_session_route(&state, "route-live-busy-session", "model-c", None, None).await;
        assert_eq!(result, Err(axum::http::StatusCode::CONFLICT));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Qualified sessions consume `ResolvedInferenceRoute` through
    /// `inference_route_tx`; a legacy model patch cannot be committed to
    /// that consumer, so it must refuse BEFORE touching live.model or the
    /// durable row — a send on the update channel is not an acknowledgement.
    #[tokio::test(flavor = "multi_thread")]
    async fn qualified_session_rejects_legacy_route_patch_before_effects() {
        let dir = unique_temp_dir("route-qualified");
        std::fs::create_dir_all(&dir).unwrap();
        let state = DaemonState::new("").with_engine(engine_cfg());
        let live = state.create_session(
            "route-qualified-session",
            None,
            dir.to_str().unwrap(),
            "Route qualified",
            None,
        );
        *lock_recover(&live.inference_selection, "session.inference_selection") =
            Some(crate::inference_runtime::InferenceSelection {
                schema_version: crate::inference_runtime::INFERENCE_SELECTION_SCHEMA_VERSION,
                authority: crate::inference_runtime::InferenceAuthority::Direct,
                connection_id: "conn-1".into(),
                protocol: crate::inference_runtime::InferenceProtocol::OpenaiCompatible,
                model_id: "qualified-model".into(),
                route_policy_id: None,
            });
        let before = lock_recover(&live.model, "session.model").clone();

        let result =
            update_session_route(&state, "route-qualified-session", "other-model", None, None)
                .await;
        assert_eq!(result, Err(axum::http::StatusCode::BAD_REQUEST));
        assert_eq!(
            lock_recover(&live.model, "session.model").as_str(),
            before.as_str(),
            "qualified session's model must not be mutated by the legacy path"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Invalid effort values are rejected by normalization before any route
    /// resolution, mutation, or persistence.
    #[tokio::test(flavor = "multi_thread")]
    async fn invalid_effort_rejects_before_effects() {
        let dir = unique_temp_dir("route-effort");
        std::fs::create_dir_all(&dir).unwrap();
        let state = DaemonState::new("").with_engine(engine_cfg());
        let live = state.create_session(
            "route-effort-session",
            None,
            dir.to_str().unwrap(),
            "Route effort",
            None,
        );
        let before = lock_recover(&live.model, "session.model").clone();
        let result = update_session_route(
            &state,
            "route-effort-session",
            "model-d",
            None,
            Some("maximum"),
        )
        .await;
        assert_eq!(result, Err(axum::http::StatusCode::BAD_REQUEST));
        assert_eq!(
            lock_recover(&live.model, "session.model").as_str(),
            before.as_str()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The durable row must exist before live fields publish: deleting the
    /// row out from under a live session makes the update fail 500 and must
    /// leave live model/provider/effort/model_route byte-identical — there
    /// is no rollback, nothing was ever published.
    #[tokio::test(flavor = "multi_thread")]
    async fn missing_durable_row_fails_without_touching_live_state() {
        let dir = unique_temp_dir("route-missing-row");
        std::fs::create_dir_all(&dir).unwrap();
        let storage = SharedStorage::new(
            rightkit_agent_engine_storage::Storage::open(dir.join("s.db")).unwrap(),
        );
        let state = DaemonState::new("")
            .with_storage(storage.clone())
            .with_engine(engine_cfg());
        let live = state.create_session(
            "route-missing-session",
            None,
            dir.to_str().unwrap(),
            "Route missing row",
            None,
        );
        // Wait for the fire-and-forget initial row write, then delete it
        // through the same writer authority the commit path uses.
        let mut persisted = false;
        for _ in 0..50 {
            if storage
                .lock()
                .await
                .get_session("route-missing-session")
                .ok()
                .flatten()
                .is_some()
            {
                persisted = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(persisted, "precondition: session row persisted");
        storage
            .lock()
            .await
            .delete_session("route-missing-session")
            .expect("row deletes");

        let before_model = lock_recover(&live.model, "session.model").clone();
        let before_provider = lock_recover(&live.provider_id, "session.provider_id").clone();
        let before_effort = lock_recover(&live.effort, "session.effort").clone();
        let before_route = live
            .model_route
            .as_ref()
            .map(|route| lock_recover(route, "session.model_route").clone());

        let result = update_session_route(
            &state,
            "route-missing-session",
            "minimax/minimax-m3",
            None,
            Some("high"),
        )
        .await;
        assert_eq!(
            result,
            Err(axum::http::StatusCode::INTERNAL_SERVER_ERROR),
            "a vanished durable row cannot claim a committed route"
        );
        assert_eq!(
            lock_recover(&live.model, "session.model").as_str(),
            before_model.as_str(),
            "uncommitted route must never be observable"
        );
        assert_eq!(
            *lock_recover(&live.provider_id, "session.provider_id"),
            before_provider
        );
        assert_eq!(*lock_recover(&live.effort, "session.effort"), before_effort);
        match (live.model_route.as_ref(), before_route) {
            (Some(cell), Some(prev)) => {
                let route = lock_recover(cell, "session.model_route");
                assert_eq!(route.model_id, prev.model_id);
                assert_eq!(route.effort, prev.effort);
            }
            (None, None) => {}
            _ => panic!("live route presence changed on a failed update"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Durable-first ordering: while the storage writer lock is held, the
    /// in-flight update (already past the route gate) must NOT have
    /// published the new route to live state. Releasing storage lets the
    /// commit land and live + durable agree on the same tuple.
    #[tokio::test(flavor = "multi_thread")]
    async fn live_route_publishes_only_after_durable_commit() {
        let dir = unique_temp_dir("route-durable-first");
        std::fs::create_dir_all(&dir).unwrap();
        let storage = SharedStorage::new(
            rightkit_agent_engine_storage::Storage::open(dir.join("s.db")).unwrap(),
        );
        let state = DaemonState::new("")
            .with_storage(storage.clone())
            .with_engine(engine_cfg());
        let live = state.create_session(
            "route-ordering-session",
            None,
            dir.to_str().unwrap(),
            "Route ordering",
            None,
        );
        let mut persisted = false;
        for _ in 0..50 {
            if storage
                .lock()
                .await
                .get_session("route-ordering-session")
                .ok()
                .flatten()
                .is_some()
            {
                persisted = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(persisted, "precondition: session row persisted");

        let before_model = lock_recover(&live.model, "session.model").clone();
        let before_provider = lock_recover(&live.provider_id, "session.provider_id").clone();
        let before_effort = lock_recover(&live.effort, "session.effort").clone();

        // Hold the storage writer: the update's spawn_blocking durable write
        // blocks here, after it has taken route_update_gate.
        let storage_guard = storage.lock().await;

        let update = tokio::spawn({
            let state = state.clone();
            async move {
                update_session_route(
                    &state,
                    "route-ordering-session",
                    "minimax/minimax-m3",
                    None,
                    Some("high"),
                )
                .await
            }
        });

        // Wait until the in-flight update actually holds the route gate —
        // provably past validation and the busy check, parked on the
        // blocked durable write.
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while live.route_update_gate.try_lock().is_ok() {
                // Config resolution may yield to real IO; counting scheduler
                // yields can finish before the spawned update gets CPU time.
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("update reached the route gate");

        // While the durable write is blocked, no reader may observe the
        // uncommitted route.
        assert_eq!(
            lock_recover(&live.model, "session.model").as_str(),
            before_model.as_str(),
            "live model must not publish before the durable commit"
        );
        assert_eq!(
            *lock_recover(&live.provider_id, "session.provider_id"),
            before_provider
        );
        assert_eq!(*lock_recover(&live.effort, "session.effort"), before_effort);
        if let Some(route) = &live.model_route {
            assert_ne!(
                lock_recover(route, "session.model_route").model_id,
                "minimax/minimax-m3",
                "live model_route must not publish before the durable commit"
            );
        }

        drop(storage_guard);
        let result = update.await.expect("update task joins");
        assert_eq!(
            result,
            Ok(true),
            "commit lands once the storage writer is released"
        );

        assert_eq!(
            lock_recover(&live.model, "session.model").as_str(),
            "minimax/minimax-m3"
        );
        assert_eq!(
            lock_recover(&live.provider_id, "session.provider_id").as_deref(),
            Some("minimax")
        );
        assert_eq!(
            lock_recover(&live.effort, "session.effort").as_deref(),
            Some("high")
        );
        let route = live.model_route.as_ref().expect("live route");
        let route = lock_recover(route, "session.model_route");
        assert_eq!(route.model_id, "MiniMax-M3");
        assert_eq!(route.effort.as_deref(), Some("high"));
        drop(route);

        let cfg = stored_config(&storage, "route-ordering-session").await;
        assert_eq!(cfg["model"], "minimax/minimax-m3");
        assert_eq!(cfg["providerId"], "minimax");
        assert_eq!(cfg["effort"], "high");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
