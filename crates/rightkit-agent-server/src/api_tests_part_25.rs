#[tokio::test]
async fn tasks_route_marks_live_workspace_shell_records_running() {
    let root = unique_temp_dir("tasks-live-shell");
    let state = DaemonState::new("");
    let live = state.create_session(
        "live-shells",
        None,
        &root.to_string_lossy(),
        "Live shells",
        None,
    );
    let command = if cfg!(windows) {
        "Start-Sleep -Seconds 30".to_string()
    } else {
        "sleep 30".to_string()
    };
    let _ = rightkit_agent_tools::coding_tools::process_resource_authority::install_process_resource_profile(
        rightkit_agent_tools::coding_tools::process_resource_authority::ResourceProfile::Expanded,
    );
    let shell_id = rightkit_agent_tools::coding_tools::background_shell::spawn_background(
        command.clone(),
        &root,
        &live.background_shells,
        None,
    )
    .expect("background admission");
    write_task_fixture_with_shell(&root, &shell_id, &command);
    let app = build_router(state);
    let uri = format!(
        "/tasks?workspaceRoot={}",
        encode_query_value(&root.to_string_lossy())
    );

    let resp = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["shellId"], shell_id);
    assert_eq!(v[0]["processStatus"], "running");
    assert_eq!(v[0]["processAttachment"], "attached");
    assert_eq!(v[0]["canPollOutput"], true);
    assert_eq!(v[0]["canStop"], true);
    assert!(v[0]["processStatusReason"].is_null());
    assert!(v[0]["lifecycleStatusReason"].is_null());

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn submit_user_text_records_live_transcript_and_event() {
    let state = state_with_session("scheduled");
    let live = state.get_session("scheduled").unwrap();
    let before = live.conduit.events_after(0).len();

    let seq = state
        .submit_text_with_trust(
            "scheduled",
            "[Scheduled prompt]\nrun the report",
            ContentTrust::User,
            "daemon.submitUserText",
        )
        .await
        .expect("submit");

    assert!(seq > 0);
    let transcript = lock_recover(&live.transcript, "session.transcript");
    assert!(matches!(
        transcript.last(),
        Some(TranscriptMessage::User { content, .. }) if content.contains("run the report")
    ));
    drop(transcript);
    let events = live.conduit.events_after(0);
    assert!(events.len() > before);
    assert!(events.iter().any(|event| matches!(
        event,
        ProtocolEvent::UserMessageSubmitted { text, .. } if text.contains("run the report")
    )));
}

/// The delivery outcome must distinguish a full queue from a closed one:
/// collapsing both to a bare bool is what let a full queue be mistaken for
/// the benign "no live engine" case and silently drop user content.
#[test]
fn engine_text_delivery_reports_full_and_closed_channels() {
    use super::api_state::{deliver_engine_text, EngineTextRefusal};
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    assert_eq!(deliver_engine_text(&tx, "first".to_string()), Ok(()));
    assert_eq!(
        deliver_engine_text(&tx, "full".to_string()),
        Err(EngineTextRefusal::QueueFull)
    );
    assert_eq!(rx.try_recv().unwrap(), "first");
    drop(rx);
    assert_eq!(
        deliver_engine_text(&tx, "closed".to_string()),
        Err(EngineTextRefusal::QueueClosed)
    );
}

#[tokio::test]
async fn resume_rehydrates_and_revives_dormant_session() {
    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    // Seed a dormant session + a user turn and an assistant reply directly.
    {
        let mut s = storage.lock().await;
        s.create_session(&rightkit_agent_engine_storage::SessionRow {
            id: "old".into(),
            title: "Prior".into(),
            workspace_root: "/tmp".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            parent_session_id: None,
            role: None,
            status: "dormant".into(),
            config_json: "{}".into(),
            tags: String::new(),
            recovery_marker: None,
        })
        .unwrap();
        let user = ProtocolEvent::UserMessageSubmitted {
            session_id: "old".into(),
            seq: 1,
            created_at: "t".into(),
            text: "remember this".into(),
            nonce: None,
            trust: None,
            attachments: Vec::new(),
        };
        let assistant = ProtocolEvent::AssistantMessageCompleted {
            session_id: "old".into(),
            seq: 2,
            created_at: "t".into(),
            message: TranscriptMessage::Assistant {
                id: "a1".into(),
                created_at: "t".into(),
                content: "I will".into(),
                reasoning_content: None,
                tool_calls: vec![],
                pinned: false,
            },
        };
        s.append_event(
            "old",
            "userMessageSubmitted",
            &serde_json::to_string(&user).unwrap(),
        )
        .unwrap();
        s.append_event(
            "old",
            "assistantMessageCompleted",
            &serde_json::to_string(&assistant).unwrap(),
        )
        .unwrap();
    }

    let state = DaemonState::new("").with_storage(storage.clone());
    // Not live until resumed.
    assert!(state.get_session("old").is_none());

    let live = state.resume_session("old").await.expect("resume");
    let t = lock_recover(&live.transcript, "session.transcript");
    assert_eq!(t.len(), 2, "rehydrated user + assistant turns");
    match &t[0] {
        TranscriptMessage::User { content, .. } => assert_eq!(content, "remember this"),
        _ => panic!("expected user first"),
    }
    // Original created_at is preserved (resume must not reset it).
    drop(t);
    let row = {
        let s = storage.lock().await;
        s.get_session("old").unwrap().unwrap()
    };
    assert_eq!(row.created_at, "2026-01-01T00:00:00Z");
    assert_eq!(row.status, "running");
}

#[tokio::test]
async fn resume_restores_and_shares_exhausted_lineage_budget() {
    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    {
        let mut store = storage.lock().await;
        for (id, parent, ceiling) in [
            ("budget-root", None, 0.03),
            ("budget-child", Some("budget-root"), 0.01),
        ] {
            store
                .create_session(&rightkit_agent_engine_storage::SessionRow {
                    id: id.into(),
                    title: id.into(),
                    workspace_root: "/tmp".into(),
                    created_at: "2026-01-01T00:00:00Z".into(),
                    updated_at: "2026-01-01T00:00:00Z".into(),
                    parent_session_id: parent.map(str::to_string),
                    role: parent.map(|_| "worker".to_string()),
                    status: "dormant".into(),
                    config_json: serde_json::json!({
                        "budgetCeilingUsd": ceiling
                    })
                    .to_string(),
                    tags: String::new(),
                    recovery_marker: None,
                })
                .unwrap();
        }
        for (id, cost_usd) in [("budget-root", 0.01), ("budget-child", 0.02)] {
            let usage = ProtocolEvent::UsageUpdated {
                session_id: id.into(),
                seq: 1,
                created_at: "2026-01-01T00:00:01Z".into(),
                prompt_tokens: 1,
                completion_tokens: 1,
                cost_usd,
                context_tokens: 2,
                context_percent: 0.0,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
                cost_known: true,
            };
            store
                .append_event(id, "usageUpdated", &serde_json::to_string(&usage).unwrap())
                .unwrap();
        }
    }

    let restarted = DaemonState::new("").with_storage(storage);
    let child = restarted
        .resume_session("budget-child")
        .await
        .expect("child resumes first");
    assert_eq!(child.lineage_budget.ceiling_microusd(), Some(30_000));
    assert_eq!(child.lineage_budget.spent_microusd(), 30_000);
    assert!(child.lineage_budget.reserve_remaining().is_err());

    let root = restarted
        .resume_session("budget-root")
        .await
        .expect("root resumes second");
    assert!(Arc::ptr_eq(&root.lineage_budget, &child.lineage_budget));
}

#[tokio::test]
async fn resume_rehydrates_interrupted_journal_marker() {
    let root = unique_temp_dir("resume-journal");
    let journal_dir = root.join("journals");
    std::fs::create_dir_all(&journal_dir).unwrap();
    std::fs::write(
        journal_dir.join("old.journal"),
        serde_json::json!({
            "ts": "2026-06-06T00:00:00Z",
            "kind": "run_start",
            "session_id": "old",
            "turn_id": "turn-1",
            "user_message_preview": "run expensive migration"
        })
        .to_string()
            + "\n",
    )
    .unwrap();

    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    {
        let mut s = storage.lock().await;
        s.create_session(&rightkit_agent_engine_storage::SessionRow {
            id: "old".into(),
            title: "Prior".into(),
            workspace_root: "/tmp".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            parent_session_id: None,
            role: None,
            status: "dormant".into(),
            config_json: "{}".into(),
            tags: String::new(),
            recovery_marker: None,
        })
        .unwrap();
        let user = ProtocolEvent::UserMessageSubmitted {
            session_id: "old".into(),
            seq: 1,
            created_at: "t".into(),
            text: "run expensive migration".into(),
            nonce: None,
            trust: None,
            attachments: Vec::new(),
        };
        s.append_event(
            "old",
            "userMessageSubmitted",
            &serde_json::to_string(&user).unwrap(),
        )
        .unwrap();
    }

    let state = DaemonState::new("")
        .with_storage(storage)
        .with_journal_dir(journal_dir);
    let live = state.resume_session("old").await.expect("resume");
    let transcript = lock_recover(&live.transcript, "session.transcript");

    assert!(transcript.iter().any(|message| matches!(
        message,
        TranscriptMessage::System { content, pinned, .. }
            if *pinned
                && content.contains("interrupted")
                && content.contains("turn-1")
                && content.contains("run expensive migration")
    )));

    let _ = std::fs::remove_dir_all(root);
}

/// A leftover journal with a `run_start` but no matching `run_end` (i.e.
/// an interrupted turn) causes `reconcile_persistent_marker` to stamp
/// a serialized `RecoveryMarker` onto the session row. The UI gets the
/// recoverable badge without having to read the FS at list time.
#[tokio::test]
async fn reconcile_writes_persistent_marker_for_leftover_journal() {
    let root = unique_temp_dir("reconcile-leftover");
    let journal_dir = root.join("journals");
    std::fs::create_dir_all(&journal_dir).unwrap();
    // Old crashed turn (no run_end) — the source of interrupted_runs().
    std::fs::write(
            journal_dir.join("old.journal"),
            "{\"ts\":\"2026-07-02T00:00:00Z\",\"kind\":\"run_start\",\"session_id\":\"old\",\"turn_id\":\"turn-7\",\"user_message_preview\":\"do the migration\"}\n",
        )
        .unwrap();

    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    {
        let s = storage.lock().await;
        s.create_session(&rightkit_agent_engine_storage::SessionRow {
            id: "old".into(),
            title: "Prior".into(),
            workspace_root: "/tmp".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            parent_session_id: None,
            role: None,
            status: "dormant".into(),
            config_json: "{}".into(),
            tags: String::new(),
            recovery_marker: None,
        })
        .unwrap();
    }
    let state = DaemonState::new("")
        .with_storage(storage.clone())
        .with_journal_dir(journal_dir.clone());

    // Daemon startup housekeeping uses persist_recovery_markers; that
    // calls reconcile_persistent_marker per session.
    crate::journal::persist_recovery_markers(
        state.storage.as_ref().expect("storage"),
        &journal_dir,
    )
    .await;
    let got = storage.lock().await.get_session("old").unwrap().unwrap();
    let json = got
        .recovery_marker
        .as_deref()
        .expect("recovery_marker stamped on row");
    let parsed: serde_json::Value = serde_json::from_str(json).unwrap();
    assert_eq!(parsed["turnId"], "turn-7");
    assert_eq!(parsed["startedAt"], "2026-07-02T00:00:00Z");
    assert_eq!(parsed["promptPreview"], "do the migration");
    assert!(!got.status.is_empty());

    let _ = std::fs::remove_dir_all(root);
}

/// When the journal is deleted (clean completion), reconcile clears
/// `recovery_marker` on the row so a stale badge doesn't survive a
/// restart — UI sees `recoverable: null` after the daemon comes back.
#[tokio::test]
async fn reconcile_clears_persistent_marker_when_journal_gone() {
    let root = unique_temp_dir("reconcile-clean");
    let journal_dir = root.join("journals");
    std::fs::create_dir_all(&journal_dir).unwrap();

    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    {
        let s = storage.lock().await;
        s.create_session(&rightkit_agent_engine_storage::SessionRow {
            id: "clean".into(),
            title: "Clean".into(),
            workspace_root: "/tmp".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            parent_session_id: None,
            role: None,
            status: "dormant".into(),
            config_json: "{}".into(),
            tags: String::new(),
            // Pre-existing stale marker — simulates a prior lifecycle
            // that observed a crash and now, on this startup, the
            // journal is gone (clean close in a later restart). The
            // reconcile must clear the row, not leave it stale.
            recovery_marker: Some(r#"{"turnId":"t","startedAt":"old"}"#.into()),
        })
        .unwrap();
    }

    let state = DaemonState::new("")
        .with_storage(storage.clone())
        .with_journal_dir(journal_dir.clone());
    // Run startup housekeeping: no journal files in the dir → all rows
    // reconcile to `recovery_marker = None`.
    crate::journal::persist_recovery_markers(
        state.storage.as_ref().expect("storage"),
        &journal_dir,
    )
    .await;

    let got = storage.lock().await.get_session("clean").unwrap().unwrap();
    assert!(
        got.recovery_marker.is_none(),
        "reconcile must clear stale marker when no journal exists"
    );

    let _ = std::fs::remove_dir_all(root);
}

/// `ExecutionJournal::close()` removes the journal file; on the next
/// reconcile pass the persistent marker is cleared. This is the
/// mid-life "clean completion" path: an open then immediately
/// completed turn should not badge the session as recoverable.
#[tokio::test]
async fn reconcile_after_close_clears_marker() {
    let root = unique_temp_dir("reconcile-close");
    let journal_dir = root.join("journals");
    std::fs::create_dir_all(&journal_dir).unwrap();

    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    {
        let s = storage.lock().await;
        s.create_session(&rightkit_agent_engine_storage::SessionRow {
            id: "ok".into(),
            title: "OK".into(),
            workspace_root: "/tmp".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            parent_session_id: None,
            role: None,
            status: "running".into(),
            config_json: "{}".into(),
            tags: String::new(),
            recovery_marker: None,
        })
        .unwrap();
    }
    let state = DaemonState::new("")
        .with_storage(storage.clone())
        .with_journal_dir(journal_dir.clone());

    // Open + append run_start + append run_end + close, mirroring the
    // engine caller. Then reconcile must NOT stamp a marker (clean
    // completion produced a paired start/end).
    let mut j = crate::journal::ExecutionJournal::open(&journal_dir, "ok").await;
    let turn_id = "turn-clean";
    j.append(crate::journal::entry(
        "run_start",
        "ok",
        turn_id,
        serde_json::json!({ "user_message_preview": "test" }),
    ))
    .await;
    j.append(crate::journal::entry(
        "run_end",
        "ok",
        turn_id,
        serde_json::json!({ "success": true }),
    ))
    .await;
    j.close().await;

    crate::journal::persist_recovery_markers(
        state.storage.as_ref().expect("storage"),
        &journal_dir,
    )
    .await;

    let got = storage.lock().await.get_session("ok").unwrap().unwrap();
    assert!(
        got.recovery_marker.is_none(),
        "clean completion must not leave a recoverable marker"
    );
    // Sanity: the file really is gone — `close()` deleted it.
    assert!(!journal_dir.join("ok.journal").exists());

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn health_ok() {
    let app = build_router(DaemonState::new("").with_runtime_info(54321, "/tmp/cr.db"));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["service"], "coderight-daemon");
    assert_eq!(v["port"], 54321);
    assert_eq!(v["databasePath"], "/tmp/cr.db");
}

#[tokio::test]
async fn cors_preflight_allows_tauri_webview_origin() {
    let app = build_router(DaemonState::new(""));
    let resp = app
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/sessions")
                .header("origin", "http://tauri.localhost")
                .header("access-control-request-method", "GET")
                .header("access-control-request-headers", "authorization")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    // The daemon now reflects the matched allow-listed origin rather than "*".
    assert_eq!(
        resp.headers().get("access-control-allow-origin").unwrap(),
        "http://tauri.localhost"
    );
}

#[tokio::test]
async fn cors_preflight_rejects_unknown_origin() {
    let app = build_router(DaemonState::new(""));
    let resp = app
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/sessions")
                .header("origin", "http://evil.example.com")
                .header("access-control-request-method", "GET")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // No allow-origin header is echoed for a non-allow-listed origin.
    assert!(resp.headers().get("access-control-allow-origin").is_none());
}

#[tokio::test]
async fn api_auth_accepts_canonical_bearer_header() {
    let app = build_router(DaemonState::new("secret-token"));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/sessions")
                .header("authorization", "Bearer secret-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn list_sessions_empty() {
    let app = build_router(DaemonState::new(""));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/sessions")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn list_sessions_hides_child_workers_unless_explicitly_requested() {
    let state = DaemonState::new("");
    state.create_session("parent", None, "/tmp", "Parent", None);
    state.create_session("child", Some("auditor"), "/tmp", "Auditor", Some("parent"));
    let app = build_router(state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/sessions")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let rows: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["id"], "parent");

    let response = app
        .oneshot(
            Request::builder()
                .uri("/sessions?includeSubagents=true")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let rows: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 2);
}

/// EXT-001 call-site regression.
///
/// An earlier version of this test only composed the same two calls session
/// creation makes; the third adversarial pass showed it still passed with the
/// production call site deleted, so it proved the functions worked rather
/// than that anything invoked them. This drives REAL session creation and
/// asserts the recorded unload, so removing the call site fails here.
///
/// Note the unload lives inside the engine-spawn block, so the state must
/// carry an engine config or the code under test is never reached at all.
#[cfg(test)]
mod ext001_session_plugin_unload_wiring {
    use crate::api::DaemonState;
    use crate::api::EngineConfig;

    fn write_plugin(root: &std::path::Path, id: &str, tools: &[&str]) {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("plugin.py"), "").unwrap();
        std::fs::write(
            dir.join("plugin.json"),
            serde_json::json!({
                "id": id,
                "name": id,
                "version": "1.0.0",
                "description": "fixture",
                "entry": "plugin.py",
                "provides_tools": tools,
                "provides_commands": [],
                "enabled": true
            })
            .to_string(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn session_creation_unloads_plugins_disabled_for_that_session() {
        let dir = tempfile::TempDir::new().unwrap();
        let plugins_root = dir.path().join("plugins");
        write_plugin(&plugins_root, "alpha", &["alpha_tool"]);
        write_plugin(&plugins_root, "beta", &["beta_tool"]);

        let mut state = DaemonState::new("").with_engine(EngineConfig {
            api_key: "test-key".into(),
            model_id: "openrouter/test-model".into(),
            base_url: "http://127.0.0.1:9".into(),
            fallback_models: Vec::new(),
        });
        state.plugin_dir = plugins_root.clone();

        // Disable one plugin for the session that is about to be created,
        // through the same type the plugin HTTP routes write to.
        crate::api::lock_recover(
            &state.session_plugin_activations,
            "state.session_plugin_activations",
        )
        .disable_for_session("alpha", "ext001-callsite");

        state.create_session(
            "ext001-callsite",
            None,
            &dir.path().to_string_lossy(),
            "unload on open",
            None,
        );

        // The session's tool registry is built inside the spawned engine task,
        // so wait for the unload to be recorded rather than assuming it
        // happened synchronously.
        let mut recorded = None;
        for _ in 0..200 {
            recorded = crate::api::lock_recover(
                &state.session_plugin_unloads,
                "state.session_plugin_unloads",
            )
            .get("ext001-callsite")
            .cloned();
            if recorded.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let (disabled, removed) = recorded.expect(
            "session creation must unload session-disabled plugin scopes; \
             no record means the call site never ran",
        );
        assert_eq!(disabled, vec!["alpha".to_string()]);
        assert_eq!(
            removed, 1,
            "the disabled plugin's tool must actually be removed from this \
             session's registry"
        );
    }
}

/// EFF-006 call-site regression: the mutation-journal sweep must actually run
/// when a session opens a workspace.
///
/// The third adversarial pass showed a "wiring test" that composed the same
/// calls the production path makes still passed with the production call site
/// removed — it proved the functions worked, not that anything invoked them.
/// This drives real session creation and asserts the recorded outcome, so
/// deleting the call site fails here.
#[cfg(test)]
mod eff006_journal_recovery_call_site {
    use crate::api::DaemonState;

    #[tokio::test]
    async fn session_creation_reconciles_an_unsettled_mutation_journal() {
        let workspace = tempfile::TempDir::new().unwrap();
        let root = workspace.path();

        // A batch that committed one mutation and died before settling.
        let applied = root.join("applied.txt");
        let untouched = root.join("untouched.txt");
        std::fs::write(&applied, "before\n").unwrap();
        std::fs::write(&untouched, "before\n").unwrap();
        let applied_revision =
            rightkit_agent_tools::coding_tools::file_service::observe_revision_for_tests(&applied)
                .unwrap();
        let untouched_revision =
            rightkit_agent_tools::coding_tools::file_service::observe_revision_for_tests(
                &untouched,
            )
            .unwrap();

        let journal_dir = root.join(".coderight").join("mutation-journal");
        std::fs::create_dir_all(&journal_dir).unwrap();
        std::fs::write(
            journal_dir.join("batch-callsite.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema": 1,
                "journalId": "batch-callsite",
                "phase": "prepared",
                "intents": [
                    {"index": 0, "intent": rightkit_agent_tools::coding_tools::file_service::MutationIntent::replace(&applied, applied_revision)},
                    {"index": 1, "intent": rightkit_agent_tools::coding_tools::file_service::MutationIntent::replace(&untouched, untouched_revision)},
                ],
                "receipt": serde_json::Value::Null,
            }))
            .unwrap(),
        )
        .unwrap();
        // The crash happened after the first commit landed.
        std::fs::write(&applied, "after\n").unwrap();

        let state = DaemonState::new("");
        state.create_session(
            "eff006-callsite",
            None,
            &root.to_string_lossy(),
            "reconcile on open",
            None,
        );

        // The sweep runs off the request path, so wait for it briefly rather
        // than assuming it completed synchronously.
        let mut reports = Vec::new();
        for _ in 0..100 {
            if let Some(found) = crate::api::lock_recover(
                &state.mutation_journal_reports,
                "state.mutation_journal_reports",
            )
            .get("eff006-callsite")
            .cloned()
            {
                reports = found;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        assert_eq!(
            reports.len(),
            1,
            "session creation must reconcile the unsettled journal; \
             an empty result means the call site never ran"
        );
        let report = &reports[0];
        assert_eq!(report.journal_id, "batch-callsite");
        assert!(
            report.observed_partial_application(),
            "the sweep must surface that the batch was partially applied"
        );
    }
}
