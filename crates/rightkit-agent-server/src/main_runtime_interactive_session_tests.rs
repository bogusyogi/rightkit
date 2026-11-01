use crate::tui_host::TurnHost as _;
use std::io::Cursor;

#[test]
fn native_harness_repository_uses_committed_switch_before_first_turn() {
    let host = EngineTurnHost::new("launch-session".into(), "legacy".into(), false, false);
    host.seed_initial_model("gpt-6-astra");
    host.turn_model
        .lock()
        .unwrap()
        .request_switch_for_next_turn(
            "openai",
            "gpt-6.1-sol",
            rightkit_agent_orchestration::PrefixChangeReason::UserRequestedSwitch,
        );
    *host.committed_route_state.lock().unwrap() = crate::tui_host::CommittedRoute {
        model: Some("gpt-6.1-sol".into()),
        effort: Some("high".into()),
    };
    *host.committed_provider.lock().unwrap() = Some("openai".into());
    assert_eq!(host.turn_model.lock().unwrap().model_id(), "gpt-6-astra");
    assert_eq!(
        host.next_repository_route(),
        (
            "openai".into(),
            Some("gpt-6.1-sol".into()),
            Some("high".into()),
        )
    );
    *host.pending_route.lock().unwrap() = Some((
        "pending-model".into(),
        "pending-provider".into(),
        Some("low".into()),
    ));
    assert_eq!(
        host.next_repository_route(),
        (
            "pending-provider".into(),
            Some("pending-model".into()),
            Some("low".into()),
        )
    );
}

#[tokio::test]
async fn native_harness_local_approval_uses_bound_repository_session() {
    let host = EngineTurnHost::new("launch-session".into(), "qa-mock".into(), false, false);
    *host.active_session_id.lock().unwrap() = "repository-session".into();
    let state = crate::api::DaemonState::new("");
    let live = state.create_session("repository-session", None, "D:/work", "Approval", None);
    let (tx, rx) = tokio::sync::oneshot::channel();
    live.pending_approvals
        .lock()
        .unwrap()
        .insert("write-1".into(), tx);
    live.pending_approval_contexts.lock().unwrap().insert(
        "write-1".into(),
        rightkit_agent_engine_protocol::PendingApprovalContext {
            request_id: None,
            tool_name: "write_file".into(),
            command: None,
            network: None,
            payload_digest: None,
        },
    );
    let control = crate::main_runtime::RunCancellation::new();
    assert!(control
        .host
        .set((
            crate::main_runtime::SessionExecutionHost::Inline(Box::new(
                crate::main_runtime::InlineHost { state }
            )),
            "repository-session".into(),
        ))
        .is_ok());
    *host.turn_cancellation.lock().unwrap() = Some(control);
    assert!(host
        .resolve_approval(
            "launch-session",
            "write-1",
            rightkit_agent_engine_protocol::ReviewDecision::Approved,
        )
        .await
        .is_err());
    assert!(live
        .pending_approvals
        .lock()
        .unwrap()
        .contains_key("write-1"));
    host.resolve_approval(
        "local",
        "write-1",
        rightkit_agent_engine_protocol::ReviewDecision::Approved,
    )
    .await
    .unwrap();
    assert!(rx.await.unwrap());
}

#[test]
fn vendor_catalog_resolves_launch_picker_and_command_to_native_ids() {
    let mut catalog = Vec::new();
    for (provider, model) in [
        ("claude-code", "sonnet"),
        ("antigravity", "gemini-3.8-flash-high"),
    ] {
        let models = vec![(model.to_string(), model.to_string())];
        append_vendor_routes(&mut catalog, provider, &models);
        append_vendor_routes(&mut catalog, provider, &models);
        let selection = format!("{provider}/{model}");
        let launch = resolve_launch_selection(&catalog, &selection).unwrap();
        let command = resolve_model_selection(&catalog, &selection).unwrap();
        let picker = resolve_picker_route(&catalog, &selection, None).unwrap();
        for (actual_provider, actual_model) in [
            (launch.provider.as_str(), launch.model.as_str()),
            (command.provider.as_str(), command.model.as_str()),
            (picker.provider.as_str(), picker.model.as_str()),
        ] {
            assert_eq!((actual_provider, actual_model), (provider, model));
        }
    }
    assert_eq!(catalog.len(), 2);
    assert!(resolve_launch_selection(&catalog, "claude-code/unknown-model").is_err());
}

#[tokio::test]
async fn context_readiness_retains_completion_before_tui_subscribes() {
    use crate::tui_host::HostReadiness;
    let (tx, rx) = tokio::sync::watch::channel(HostReadiness::StartingContext);
    drop(rx);
    publish_context_readiness(&tx, async { Ok(()) })
        .await
        .unwrap();
    assert_eq!(*tx.subscribe().borrow(), HostReadiness::Ready);

    let failure = "context unavailable".to_string();
    assert!(
        publish_context_readiness(&tx, async { Err(failure.clone()) })
            .await
            .is_err()
    );
    assert_eq!(
        *tx.subscribe().borrow(),
        HostReadiness::Failed { message: failure }
    );
}

#[tokio::test]
async fn inline_tui_rejects_even_a_cached_daemon_feed_but_service_retains_it() {
    let _lock = crate::test_support::process_env_lock();
    struct RestoreHost(Option<std::ffi::OsString>);
    impl Drop for RestoreHost {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => std::env::set_var("RIGHTKIT_RUN_HOST", value),
                None => std::env::remove_var("RIGHTKIT_RUN_HOST"),
            }
        }
    }
    let _restore = RestoreHost(std::env::var_os("RIGHTKIT_RUN_HOST"));
    let host = EngineTurnHost::new("local".into(), "qa-mock".into(), false, false);
    let client = std::sync::Arc::new(crate::main_runtime::DaemonSessionClient::new(
        "http://127.0.0.1:9".into(),
        "unused-test-token".into(),
    ));
    *host.daemon.lock().unwrap() = Some(client.clone());

    // An available/cached daemon must not change normal local host selection.
    for mode in [None, Some("inline"), Some("local")] {
        match mode {
            Some(value) => std::env::set_var("RIGHTKIT_RUN_HOST", value),
            None => std::env::remove_var("RIGHTKIT_RUN_HOST"),
        }
        assert!(matches!(host.daemon_client().await, Err(error)
            if error == "daemon feed unavailable for InlineHost"));
    }
    std::env::set_var("RIGHTKIT_RUN_HOST", "service");
    let service_client = host.daemon_client().await.expect("explicit service feed");
    assert!(std::sync::Arc::ptr_eq(&service_client, &client));
}

#[test]
fn classify_line_blank_reprompts() {
    assert_eq!(classify_line(Some("\n")), ReplLine::Blank);
    assert_eq!(classify_line(Some("   \n")), ReplLine::Blank);
}

#[test]
fn classify_line_quit_variants_exit() {
    assert_eq!(classify_line(Some("/quit\n")), ReplLine::Quit);
    assert_eq!(classify_line(Some("/exit\n")), ReplLine::Quit);
    assert_eq!(classify_line(Some("/q\n")), ReplLine::Quit);
    // Trailing whitespace / no trailing newline still classify correctly.
    assert_eq!(classify_line(Some("/quit")), ReplLine::Quit);
}

#[test]
fn classify_line_eof_on_none() {
    assert_eq!(classify_line(None), ReplLine::Eof);
}

#[test]
fn classify_line_prompt_is_trimmed() {
    assert_eq!(
        classify_line(Some("  hello world  \n")),
        ReplLine::Prompt("hello world".to_string())
    );
}

#[test]
fn classify_line_prompt_mentioning_quit_is_not_swallowed() {
    // A real prompt that happens to contain the word "quit" must NOT be
    // treated as the quit command.
    assert_eq!(
        classify_line(Some("how do I quit vim?\n")),
        ReplLine::Prompt("how do I quit vim?".to_string())
    );
}

#[test]
fn read_line_returns_none_on_eof() {
    let mut reader = Cursor::new(Vec::<u8>::new());
    let result = read_line(&mut reader).unwrap();
    assert_eq!(result, None);
}

#[test]
fn read_line_returns_content() {
    let mut reader = Cursor::new(b"hello\nworld\n".to_vec());
    assert_eq!(read_line(&mut reader).unwrap(), Some("hello\n".to_string()));
    assert_eq!(read_line(&mut reader).unwrap(), Some("world\n".to_string()));
    assert_eq!(read_line(&mut reader).unwrap(), None);
}

#[tokio::test]
async fn run_loop_single_prompt_then_eof_drives_one_turn() {
    let input = Cursor::new(b"say hi\n".to_vec());
    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen_prompt = std::sync::Arc::new(std::sync::Mutex::new(None));
    let call_count_clone = call_count.clone();
    let seen_prompt_clone = seen_prompt.clone();

    let code = run_loop(
        input,
        "interactive-test",
        "openrouter",
        None,
        None,
        None,
        None,
        move |args| {
            call_count_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            *seen_prompt_clone.lock().unwrap() = Some(args.prompt.clone());
            async move { 0 }
        },
    )
    .await;

    assert_eq!(code, 0);
    assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(seen_prompt.lock().unwrap().as_deref(), Some("say hi"));
}

#[tokio::test]
async fn run_loop_quit_exits_without_a_turn() {
    let input = Cursor::new(b"/quit\n".to_vec());
    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let call_count_clone = call_count.clone();

    let code = run_loop(
        input,
        "interactive-test",
        "openrouter",
        None,
        None,
        None,
        None,
        move |_args| {
            call_count_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async move { 0 }
        },
    )
    .await;

    assert_eq!(code, 0);
    assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn run_loop_immediate_eof_exits_without_a_turn() {
    let input = Cursor::new(Vec::<u8>::new());
    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let call_count_clone = call_count.clone();

    let code = run_loop(
        input,
        "interactive-test",
        "openrouter",
        None,
        None,
        None,
        None,
        move |_args| {
            call_count_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async move { 0 }
        },
    )
    .await;

    assert_eq!(code, 0);
    assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn run_loop_blank_lines_reprompt_then_prompt_then_quit() {
    let input = Cursor::new(b"\n\nhello\n/quit\n".to_vec());
    let prompts = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let prompts_clone = prompts.clone();

    let code = run_loop(
        input,
        "interactive-test",
        "openrouter",
        None,
        None,
        None,
        None,
        move |args| {
            prompts_clone.lock().unwrap().push(args.prompt.clone());
            async move { 0 }
        },
    )
    .await;

    assert_eq!(code, 0);
    assert_eq!(*prompts.lock().unwrap(), vec!["hello".to_string()]);
}

#[tokio::test]
async fn run_loop_multiple_prompts_before_eof() {
    let input = Cursor::new(b"first\nsecond\nthird\n".to_vec());
    let prompts = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let prompts_clone = prompts.clone();

    let code = run_loop(
        input,
        "interactive-test",
        "openrouter",
        None,
        None,
        None,
        None,
        move |args| {
            prompts_clone.lock().unwrap().push(args.prompt.clone());
            async move { 0 }
        },
    )
    .await;

    assert_eq!(code, 0);
    assert_eq!(
        *prompts.lock().unwrap(),
        vec![
            "first".to_string(),
            "second".to_string(),
            "third".to_string()
        ]
    );
}

#[tokio::test]
async fn model_switch_command_changes_model_for_next_turn_without_ending_session() {
    let _lock = crate::test_support::process_env_lock();
    struct RestoreModel(Option<std::ffi::OsString>);
    impl Drop for RestoreModel {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => std::env::set_var("RIGHTKIT_MODEL", value),
                None => std::env::remove_var("RIGHTKIT_MODEL"),
            }
        }
    }
    let _restore = RestoreModel(std::env::var_os("RIGHTKIT_MODEL"));
    // The loop now resolves only catalog routes. Bind the fixture's model
    // through the configured authority pin, independent of user credentials.
    std::env::set_var("RIGHTKIT_MODEL", "claude-opus");
    // 13a end-to-end: `/model` between two prompts must change the model
    // used by the *second* turn only, and the loop must keep running (both
    // prompts observed, exit code still clean) rather than treating the
    // switch as session-ending.
    let input = Cursor::new(
        b"first
/model claude-opus
second
/quit
"
        .to_vec(),
    );
    let seen_models = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen_models_clone = seen_models.clone();

    let code = run_loop(
        input,
        "interactive-test",
        "openrouter",
        None,
        None,
        None,
        None,
        move |args| {
            seen_models_clone.lock().unwrap().push(args.model.clone());
            async move { 0 }
        },
    )
    .await;

    assert_eq!(code, 0);
    let seen = seen_models.lock().unwrap();
    assert_eq!(
        seen.len(),
        2,
        "both prompts before /quit must still run as turns"
    );
    assert_eq!(seen[0], None, "first turn predates the /model command");
    assert_eq!(
        seen[1],
        Some("claude-opus".to_string()),
        "second turn must pick up the switch queued by /model"
    );
}

#[test]
fn model_switch_command_is_parsed_and_does_not_itself_submit_a_turn() {
    assert_eq!(
        classify_line(Some(
            "/model claude-opus
"
        )),
        ReplLine::ModelSwitch("claude-opus".to_string())
    );
    // Bare `/model` with no argument is not swallowed — it falls through
    // to a normal prompt so the user sees their mistake rather than a
    // silent no-op.
    assert_eq!(
        classify_line(Some(
            "/model
"
        )),
        ReplLine::Prompt("/model".to_string())
    );
}

#[test]
fn turn_args_uses_session_id_and_resolved_provider() {
    let args = build_turn_args(
        "hello".to_string(),
        "interactive-4242",
        "ollama-local",
        None,
        None,
        None,
        false,
    );
    assert_eq!(args.prompt, "hello");
    assert_eq!(args.session_id.as_deref(), Some("interactive-4242"));
    assert_eq!(args.provider.as_deref(), Some("ollama-local"));
    assert!(args.model.is_none());
    assert!(!args.auto_approve);
    assert!(!args.bypass_approvals);
    assert!(!args.safe_mode);
    assert_eq!(args.input_format, super::super::InputFormat::Text);
    assert_eq!(args.output_format, super::super::OutputFormat::Text);
}

#[test]
fn turn_args_silent_suppresses_streamed_text_projection() {
    // The TUI renders turn events through its own transcript projection;
    // a raw Text projection would write into the live viewport and corrupt
    // the frame (the resize/cancel artifacts seen under ConPTY).
    let args = build_turn_args(
        "hello".to_string(),
        "interactive-4242",
        "ollama-local",
        None,
        None,
        None,
        true,
    );
    assert_eq!(args.output_format, super::super::OutputFormat::Silent);
}

#[test]
fn resolve_default_provider_id_prefers_matching_configured_route() {
    let mut config = rightkit_app_settings::agent_config::RightKitConfig::default();
    config.model.default_model = "qwen2.5-coder:7b".to_string();
    config.provider_routes.insert(
        "ollama-local".to_string(),
        rightkit_app_settings::agent_config::ProviderRouteConfig {
            model: "qwen2.5-coder:7b".to_string(),
            ..Default::default()
        },
    );
    assert_eq!(
        super::super::resolve_default_provider_id(&config),
        "ollama-local"
    );
}

#[test]
fn resolve_default_provider_id_falls_back_to_hosted_default() {
    // No configured route matches and (in the test environment) no hosted
    // key is guaranteed, so resolution must land on the hosted default
    // rather than fail.
    let config = rightkit_app_settings::agent_config::RightKitConfig::default();
    assert_eq!(
        super::super::resolve_default_provider_id(&config),
        "openrouter"
    );
}

// ── 1A/1C repairs: durable snapshot projection & pending-route merge ─────

#[test]
fn session_snapshot_route_restores_full_durable_tuple() {
    let row = serde_json::json!({
        "id": "s-1",
        "workspaceRoot": "D:/repo",
        "providerId": "openai",
        "model": "gpt-6.1-sol",
        "effort": "low",
    });
    assert_eq!(
        session_snapshot_route(&row),
        RestoredSessionRoute {
            workspace_root: Some("D:/repo".to_string()),
            provider_id: Some("openai".to_string()),
            model: Some("gpt-6.1-sol".to_string()),
            effort: Some("low".to_string()),
        }
    );
}

#[test]
fn session_snapshot_route_keeps_missing_fields_absent() {
    // A dormant row emits no `providerId`; an empty-string model is as
    // absent as a missing key — neither may be "restored" as a guess.
    let row = serde_json::json!({
        "workspaceRoot": "D:/repo",
        "model": "",
        "effort": null,
    });
    assert_eq!(
        session_snapshot_route(&row),
        RestoredSessionRoute {
            workspace_root: Some("D:/repo".to_string()),
            provider_id: None,
            model: None,
            effort: None,
        }
    );
}

#[test]
fn session_snapshot_route_accepts_legacy_workspace_key() {
    let row = serde_json::json!({ "workspace": "D:/repo" });
    assert_eq!(
        session_snapshot_route(&row).workspace_root.as_deref(),
        Some("D:/repo")
    );
}

#[test]
fn session_snapshot_route_without_scope_restores_no_root() {
    let row = serde_json::json!({ "id": "s-2", "model": "m" });
    assert_eq!(session_snapshot_route(&row).workspace_root, None);
}

#[test]
fn merge_pending_route_preserves_untouched_fields() {
    // Effort-only update must not blank a pending model/provider.
    assert_eq!(
        merge_pending_route(
            Some(("m1".to_string(), "p1".to_string(), None)),
            None,
            None,
            Some("low".to_string()),
        ),
        ("m1".to_string(), "p1".to_string(), Some("low".to_string()))
    );
    // Model update preserves a pending effort.
    assert_eq!(
        merge_pending_route(
            Some((String::new(), String::new(), Some("high".to_string()))),
            Some("m2".to_string()),
            Some("p2".to_string()),
            None,
        ),
        ("m2".to_string(), "p2".to_string(), Some("high".to_string()))
    );
    // Empty pending behaves like None.
    assert_eq!(
        merge_pending_route(None, None, None, Some("low".to_string())),
        (String::new(), String::new(), Some("low".to_string()))
    );
}

#[tokio::test]
async fn select_effort_without_repo_preserves_empty_model_pending() {
    // `/effort` with no committed model and no repository must merge into
    // the pending tuple — this is the branch that previously overwrote a
    // pending model/provider with empty strings.
    let host = EngineTurnHost::new("s".into(), "qa-mock".into(), false, false);
    *host.selected_repository.lock().unwrap() = None;
    *host.pending_route.lock().unwrap() = Some((String::new(), String::new(), None));
    host.select_effort("low").await.expect("effort pending");
    let (model, provider, effort) = host
        .pending_route
        .lock()
        .unwrap()
        .clone()
        .expect("pending route retained");
    assert_eq!((model.as_str(), provider.as_str()), ("", ""));
    assert_eq!(effort.as_deref(), Some("low"));
}

#[tokio::test]
async fn select_effort_rejects_when_repo_selected_but_no_committed_model() {
    // Selected repository + no committed model is a typed error, not a
    // pending write — the durable session would otherwise accept an effort
    // with no route to apply it to.
    let host = EngineTurnHost::new("s".into(), "qa-mock".into(), false, false);
    *host.selected_repository.lock().unwrap() = Some(
        rightkit_memory::backend::repository_scope::CanonicalRepositoryScope {
            root: "D:/repo".to_string(),
            repository_id: "repo-test".to_string(),
            scope_id: "scope-test".to_string(),
            scope_descriptor: None,
        },
    );
    let error = host
        .select_effort("low")
        .await
        .expect_err("effort without a committed model must fail");
    assert!(error.contains("no committed model"));
    assert!(host.pending_route.lock().unwrap().is_none());
}

// ── switch_session provider/effort restoration semantics ─────────────────

fn catalog_row(provider: &str, model: &str, selectable: bool) -> CatalogRoute {
    CatalogRoute {
        provider: provider.to_string(),
        model: model.to_string(),
        display_name: model.to_string(),
        selectable,
        availability_note: None,
        is_default: false,
        capability: None,
    }
}

#[test]
fn unique_route_provider_resolves_single_selectable_route() {
    let catalog = vec![
        catalog_row("openai", "gpt-6.1-sol", true),
        catalog_row("openrouter", "moonshotai/kimi-k2.6", true),
    ];
    assert_eq!(
        unique_route_provider(&catalog, "gpt-6.1-sol").as_deref(),
        Some("openai")
    );
}

#[test]
fn unique_route_provider_refuses_ambiguous_model() {
    // The same model id served by two providers is not uniquely resolvable —
    // switch_session turns this into a typed refusal rather than pairing the
    // restored model with a guessed (or the previous session's) provider.
    let catalog = vec![
        catalog_row("openai", "shared-model", true),
        catalog_row("azure", "shared-model", true),
    ];
    assert_eq!(unique_route_provider(&catalog, "shared-model"), None);
}

#[test]
fn unique_route_provider_refuses_unknown_or_unavailable_model() {
    let catalog = vec![
        catalog_row("openai", "other-model", true),
        catalog_row("openai", "gone-model", false),
    ];
    assert_eq!(unique_route_provider(&catalog, "missing-model"), None);
    // A typed-unavailable row proves the model exists but is not a
    // selectable authority — it must not resolve a provider.
    assert_eq!(unique_route_provider(&catalog, "gone-model"), None);
}

// ── provider-qualified selection keys (same model under distinct providers) ──

#[test]
fn route_selection_id_qualifies_only_collided_models() {
    let catalog = vec![
        catalog_row("openai", "shared", true),
        catalog_row("azure", "shared", true),
        catalog_row("openai", "solo", true),
    ];
    // The committed row keeps its bare id so the effort picker's
    // committed-model lookup still resolves it.
    assert_eq!(
        route_selection_id(&catalog, &catalog[0], Some(("shared", "openai"))),
        "shared"
    );
    assert_eq!(
        route_selection_id(&catalog, &catalog[1], Some(("shared", "openai"))),
        "azure::shared"
    );
    assert_eq!(
        route_selection_id(&catalog, &catalog[2], Some(("shared", "openai"))),
        "solo"
    );
    // With no committed route BOTH collided rows carry the opaque key.
    assert_eq!(
        route_selection_id(&catalog, &catalog[0], None),
        "openai::shared"
    );
}

#[test]
fn resolve_catalog_route_decodes_provider_qualified_key_to_exact_row() {
    let catalog = vec![
        catalog_row("openai", "shared", true),
        catalog_row("azure", "shared", true),
    ];
    let route = resolve_catalog_route(&catalog, "azure::shared").unwrap();
    assert_eq!(
        (route.provider.as_str(), route.model.as_str()),
        ("azure", "shared")
    );
    // Unknown keys and unknown qualified routes are typed errors.
    assert!(resolve_catalog_route(&catalog, "gemini::shared").is_err());
    assert!(resolve_catalog_route(&catalog, "missing-model").is_err());
}

#[test]
fn resolve_catalog_route_refuses_bare_cross_provider_ambiguity() {
    let catalog = vec![
        catalog_row("openai", "shared", true),
        catalog_row("azure", "shared", true),
        catalog_row("openai", "solo", true),
    ];
    let err = resolve_catalog_route(&catalog, "shared").unwrap_err();
    assert!(err.contains("multiple providers"), "error: {err}");
    // A uniquely-served model still resolves bare.
    let route = resolve_catalog_route(&catalog, "solo").unwrap();
    assert_eq!(route.provider, "openai");
}

#[test]
fn resolve_picker_route_decodes_opaque_key_and_committed_bare_id() {
    let catalog = vec![
        catalog_row("openai", "shared", true),
        catalog_row("azure", "shared", true),
    ];
    assert_eq!(
        resolve_picker_route(&catalog, "azure::shared", None)
            .unwrap()
            .provider,
        "azure"
    );
    // The committed row emits its bare id: re-affirming the current route
    // resolves deterministically to the COMMITTED provider, not a
    // first-match guess.
    assert_eq!(
        resolve_picker_route(&catalog, "shared", Some(("shared", "azure")))
            .unwrap()
            .provider,
        "azure"
    );
    // A bare ambiguous id that does NOT name the committed route stays a
    // refusal — the picker never silently picks a provider.
    assert!(resolve_picker_route(&catalog, "shared", Some(("other", "openai"))).is_err());
    assert!(resolve_picker_route(&catalog, "shared", None).is_err());
}

#[test]
fn resolve_model_selection_preserves_provider_through_number_and_key() {
    let catalog = vec![
        catalog_row("openai", "shared", true),
        catalog_row("azure", "shared", true),
    ];
    // Numeric picker resolution carries the exact row's provider.
    let selection = resolve_model_selection(&catalog, "2").unwrap();
    assert_eq!(selection.provider, "azure");
    assert_eq!(selection.model, "shared");
    // The provider-qualified key resolves the same exact route.
    let selection = resolve_model_selection(&catalog, "azure::shared").unwrap();
    assert_eq!(selection.provider, "azure");
    assert_eq!(selection.model, "shared");
    // The bare ambiguous id is a typed refusal naming the providers.
    let err = resolve_model_selection(&catalog, "shared").unwrap_err();
    assert!(err.contains("multiple providers"), "error: {err}");
    assert!(
        err.contains("openai") && err.contains("azure"),
        "error: {err}"
    );
}

#[test]
fn launch_model_flag_resolves_provider_slash_model_and_bare_ids() {
    let catalog = vec![
        catalog_row("openai", "shared", true),
        catalog_row("azure", "shared", true),
        catalog_row("openrouter", "openai/gpt-x", true),
        catalog_row("openai", "gpt-x", true),
        catalog_row("azure", "offline", false),
    ];
    // `<provider>/<model>` names the exact route, even for an id two providers serve.
    let selection = resolve_launch_selection(&catalog, "azure/shared").unwrap();
    assert_eq!(
        (selection.provider.as_str(), selection.model.as_str()),
        ("azure", "shared")
    );
    // A provider prefix wins over a same-spelled slash-bearing model id.
    let selection = resolve_launch_selection(&catalog, "openai/gpt-x").unwrap();
    assert_eq!(
        (selection.provider.as_str(), selection.model.as_str()),
        ("openai", "gpt-x")
    );
    // `<provider>::<model>` and bare unambiguous ids still work.
    assert_eq!(
        resolve_launch_selection(&catalog, "azure::shared")
            .unwrap()
            .provider,
        "azure"
    );
    assert_eq!(
        resolve_launch_selection(&catalog, "openrouter/openai/gpt-x")
            .unwrap()
            .provider,
        "openrouter"
    );
    // Ambiguity, unavailable routes, unknown ids, numbers and empty input are typed errors.
    assert!(resolve_launch_selection(&catalog, "shared")
        .unwrap_err()
        .contains("multiple providers"));
    assert!(resolve_launch_selection(&catalog, "azure/offline")
        .unwrap_err()
        .contains("unavailable"));
    assert!(resolve_launch_selection(&catalog, "nope/none")
        .unwrap_err()
        .contains("unknown model"));
    assert!(resolve_launch_selection(&catalog, "2").is_err());
    assert!(resolve_launch_selection(&catalog, "  ").is_err());
}

#[test]
fn vendor_warning_follows_committed_route_before_next_turn() {
    use rightkit_agent_policy::PermissionMode;
    let host = EngineTurnHost::new("vendor-warning".into(), "openai".into(), false, false);
    *host.committed_provider.lock().unwrap() = Some("antigravity".into());
    for mode in [
        PermissionMode::Ask,
        PermissionMode::AcceptEdits,
        PermissionMode::AutoSafe,
    ] {
        host.set_permission_mode(mode);
        assert_eq!(
            host.permission_warning(),
            Some("Antigravity uses its own permission rules")
        );
    }
    host.set_permission_mode(PermissionMode::Bypass);
    assert_eq!(host.permission_warning(), None);
}

#[tokio::test]
async fn vendor_selection_commits_to_vendor_launch_without_native_api_route() {
    let host = EngineTurnHost::new("vendor-selection".into(), "openai".into(), false, false);
    host.apply_route_selection_inner("sonnet", Some("claude-code"), None)
        .await
        .unwrap();
    assert_eq!(host.committed_provider_id(), "claude-code");
    assert_eq!(host.committed_route().model.as_deref(), Some("sonnet"));
    let mut route = host.turn_model.lock().unwrap();
    route.begin_turn();
    assert_eq!(route.provider_id(), "claude-code");
    assert_eq!(route.model_id(), "sonnet");
}

#[test]
fn bare_canonical_command_names_reach_the_parser_in_slash_form() {
    assert_eq!(slash_form("resume"), "/resume");
    assert_eq!(slash_form("/status"), "/status");
    assert_eq!(slash_form("session list"), "/session list");
    // Every registered command, bare or slashed, parses once normalised.
    for spec in coderight_skills::commands::COMMAND_SPECS {
        let typed = slash_form(spec.name);
        assert!(
            coderight_skills::commands::SlashCommand::parse(&typed).is_some(),
            "{typed} must parse as a slash command"
        );
    }
}

#[test]
fn permission_slash_commands_set_the_live_mode() {
    use rightkit_agent_policy::PermissionMode;
    assert_eq!(
        permission_mode_for_command("/ask"),
        Some(PermissionMode::Ask)
    );
    assert_eq!(
        permission_mode_for_command("/accept-edits"),
        Some(PermissionMode::AcceptEdits)
    );
    assert_eq!(
        permission_mode_for_command("/auto"),
        Some(PermissionMode::AutoSafe)
    );
    assert_eq!(
        permission_mode_for_command("/yolo"),
        Some(PermissionMode::Bypass)
    );
    assert_eq!(
        permission_mode_for_command("/plan"),
        Some(PermissionMode::Plan)
    );
    assert_eq!(permission_mode_for_command("/model"), None);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let host = EngineTurnHost::new("s".into(), "qa-mock".into(), false, false);
    assert_eq!(host.permission_mode(), PermissionMode::Ask);
    for (command, mode) in [
        ("/yolo", PermissionMode::Bypass),
        ("/auto", PermissionMode::AutoSafe),
        ("/accept-edits", PermissionMode::AcceptEdits),
        ("/ask", PermissionMode::Ask),
    ] {
        assert!(!runtime.block_on(host.execute_command(command, "")));
        assert_eq!(host.permission_mode(), mode, "{command}");
    }
    // `/plan` toggles: first enters Plan, second returns to Ask.
    assert!(!runtime.block_on(host.execute_command("/plan", "")));
    assert_eq!(host.permission_mode(), PermissionMode::Plan);
    assert!(!runtime.block_on(host.execute_command("/plan", "")));
    assert_eq!(host.permission_mode(), PermissionMode::Ask);
    // The same state Shift+Tab drives.
    host.set_permission_mode(host.permission_mode().next_in_cycle());
    assert_eq!(host.permission_mode(), PermissionMode::AcceptEdits);
}
