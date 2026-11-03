use axum::{
    body::{Body, Bytes},
    extract::State,
    response::Json,
    routing::post,
    Router,
};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};
use tokio_util::task::AbortOnDropHandle;

type Capture = (Value, oneshot::Sender<String>);

fn hosted_fixture_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn install_hosted_fixture_resource_governor() {
    rightkit_agent_tools::coding_tools::process_resource_authority::install_process_resource_profile(
        rightkit_agent_tools::coding_tools::process_resource_authority::ResourceProfile::Expanded,
    )
    .expect("hosted cancellation fixture must install its process resource authority");
}

fn bind_canonical_membrane_fixture(state: api::DaemonState) -> api::DaemonState {
    // Membrane is Pulse's CLI; hosted fixtures run with context off and need no binding.
    state
}

fn pin_hosted_fixture_route(root: &std::path::Path, provider_url: &str) {
    // Session admission reads workspace config even when EngineConfig is supplied.
    // Keep both hosts on this fixture's loopback server, independent of user config.
    let config = root.join(".coderight");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("config.toml"), format!(
        "[model]\ndefault-model = \"moonshotai/kimi-k2.6\"\nfallback-models = []\n\n[provider-routes.qa-hosted]\nbase-url = \"{provider_url}\"\nmodel = \"moonshotai/kimi-k2.6\"\napi-key-required = false\n"
    )).unwrap();
}

async fn provider(
    State(tx): State<mpsc::Sender<Capture>>,
    Json(body): Json<Value>,
) -> axum::response::Response {
    let (release, waiting) = oneshot::channel();
    tx.send((body, release)).await.unwrap();
    axum::response::Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(futures_util::stream::once(async move {
            Ok::<_, std::io::Error>(Bytes::from(waiting.await.unwrap_or_default()))
        })))
        .unwrap()
}

fn reply(tool: bool) -> String {
    reply_with_call_id(tool, "cancel-clock")
}

// Each turn on one session needs its own tool-call id: the engine drops a call whose id an earlier
// turn already used (cross-turn duplicate).
fn reply_with_call_id(tool: bool, call_id: &str) -> String {
    let delta = if tool {
        json!({"tool_calls":[{"index":0,"id":call_id,"type":"function","type":"function",
            "function":{"name":"write_file","arguments":"{\"path\":\"approval-fixture.txt\",\"content\":\"approved\"}"}}]})
    } else {
        json!({"content":"REPLACEMENT_OK"})
    };
    format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":delta,"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{},"finish_reason":if tool {"tool_calls"} else {"stop"}}]})
    )
}

fn reply_ask_user(call_id: &str) -> String {
    let args = r#"{"questions":[{"question":"Continue?","header":"Confirm","options":[{"label":"yes","description":"Continue"},{"label":"no","description":"Stop"}],"multiSelect":false}]}"#;
    format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":call_id,"type":"function","function":{"name":"ask_user","arguments":args}}]},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]})
    )
}

fn launch(
    args: RunArgs,
    host: SessionExecutionHost,
    control: Arc<RunCancellation>,
) -> tokio::task::JoinHandle<i32> {
    tokio::spawn(async move {
        let _completion = RunCompletionGuard(control.clone());
        run_hosted_with(args, control, || async move { Ok(host) }).await
    })
}

#[tokio::test]
async fn preadmission_cancel_never_selects_or_submits() {
    let mut args = output_mode_args_for_daemon_client_tests();
    args.prompt = "cancel before admission".into();
    let control = RunCancellation::new();
    control.requested.cancel();
    let result = {
        let _completion = RunCompletionGuard(control.clone());
        run_hosted_with(args, control.clone(), || async {
            panic!("cancelled presentation selected an execution host");
            #[allow(unreachable_code)]
            Err("unreachable".into())
        })
        .await
    };
    assert_eq!(result, 130);
    assert!(control.cancel_and_wait().await.is_ok());
}

#[tokio::test]
async fn unsupported_run_option_fails_before_host_selection() {
    let mut args = output_mode_args_for_daemon_client_tests();
    // CG-A01's canonical grammar consumes native turn limits; A010's current
    // contract is recorded in docs/pending/cli-tui-capability-ledger.md.
    args.max_turns = Some(2);
    assert!(validate_run_options(&args).is_ok());
    args.max_turns = Some(0);
    let control = RunCancellation::new();
    let result = {
        let _completion = RunCompletionGuard(control.clone());
        run_hosted_with(args, control.clone(), || async {
            panic!("invalid option must not select an execution host");
            #[allow(unreachable_code)]
            Err("unreachable".into())
        })
        .await
    };
    assert_eq!(result, RunExitCode::Usage.as_i32());
    assert!(control.cancel_and_wait().await.is_ok());
}

#[tokio::test]
async fn uncertain_feed_completion_never_acknowledges_cancellation() {
    let control = RunCancellation::new();
    assert!(control.require_settled().is_err());
    control
        .admission_started
        .store(true, std::sync::atomic::Ordering::Release);
    control.finish();
    assert!(control.require_settled().is_err());
    assert!(control
        .cancel_and_wait()
        .await
        .unwrap_err()
        .contains("without confirmed engine settlement"));
    control.settled();
    assert!(control.require_settled().is_ok());
}

#[test]
fn headless_interaction_interrupts_and_silent_mode_waits() {
    install_hosted_fixture_resource_governor();
    // Declare TempDir before the runtime so every task/process is stopped
    // before SQLite files are removed, including during panic unwinding.
    let root = tempfile::tempdir().unwrap();
    let runtime = hosted_fixture_runtime();
    runtime.block_on(headless_interaction_fixture(&root));
}

async fn headless_interaction_fixture(root: &tempfile::TempDir) {
    let (tx, mut captures) = mpsc::channel::<Capture>(8);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider_url = format!("http://{}/v1", listener.local_addr().unwrap());
    pin_hosted_fixture_route(root.path(), &provider_url);
    let _provider = AbortOnDropHandle::new(tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/v1/chat/completions", post(provider))
                .with_state(tx),
        )
        .await
        .unwrap();
    }));
    let storage = api::SharedStorage::new(
        rightkit_agent_engine_storage::Storage::open(root.path().join("interaction.db")).unwrap(),
    );
    let state = bind_canonical_membrane_fixture(
        api::DaemonState::new("")
            .with_storage(storage)
            .with_engine(api::EngineConfig {
                api_key: "test-key".into(),
                model_id: "moonshotai/kimi-k2.6".into(),
                base_url: provider_url,
                fallback_models: vec![],
            }),
    );
    let id = "headless-interaction";
    let _ = crate::api_sessions::lifecycle::create_session_handler(
        AxumState(state.clone()),
        AxumJson(
            serde_json::from_value(json!({
                "id":id, "title":"Interaction fixture", "workspaceRoot":root.path(),
                "model":"moonshotai/kimi-k2.6", "providerId":"qa-hosted",
                "allowedTools":["ask_user"], "permissionMode":"ask"
            }))
            .unwrap(),
        ),
    )
    .await
    .unwrap();
    let host = SessionExecutionHost::Inline(Box::new(InlineHost {
        state: state.clone(),
    }));
    let mut args = output_mode_args_for_daemon_client_tests();
    args.prompt = "Ask me.".into();
    args.workspace = Some(root.path().to_string_lossy().into_owned());
    args.session_id = Some(id.into());
    args.allowed_tools = vec!["ask_user".into()];
    args.auto_approve = false;
    let control = RunCancellation::new();
    let running = launch(args.clone(), host.clone(), control.clone());
    let (_, release) = captures.recv().await.unwrap();
    release.send(reply_ask_user("ask-clock-headless")).unwrap();
    let live = state.get_session(id).unwrap();
    let exit = tokio::time::timeout(std::time::Duration::from_secs(10), running)
        .await
        .expect("headless interaction must settle")
        .unwrap();
    assert_eq!(exit, RunExitCode::Generic.as_i32());
    assert!(control.require_settled().is_ok());
    assert!(!live.live_turn_handle.is_active());
    assert!(api::lock_recover(&live.pending_questions, "test.questions").is_empty());

    let mut silent_args = args;
    silent_args.output_format = OutputFormat::Silent;
    let silent_control = RunCancellation::new();
    let silent_running = launch(silent_args, host.clone(), silent_control.clone());
    let (_, release) = captures.recv().await.unwrap();
    release.send(reply_ask_user("ask-clock-silent")).unwrap();
    let question_id = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let question_id = api::lock_recover(&live.pending_questions, "test.questions")
                .keys()
                .next()
                .cloned();
            if let Some(question_id) = question_id {
                break question_id;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("Silent/TUI path must reach question response gate");
    assert!(
        live.live_turn_handle.is_active(),
        "Silent/TUI path must retain pending question for its answer surface"
    );
    // The answer surface keys requests by questionId, independent of toolCallId.
    host.answer_question(id, &question_id, json!({"Confirm":"yes"}))
        .await
        .unwrap();
    let (request, release) = captures.recv().await.unwrap();
    let answer = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|message| message["role"] == "tool")
        .expect("question answer reaches provider");
    assert!(answer["content"].as_str().unwrap().contains("Confirm: yes"));
    release.send(reply(false)).unwrap();
    assert_eq!(silent_running.await.unwrap(), 0);
    assert!(silent_control.require_settled().is_ok());
    assert!(api::lock_recover(&live.pending_questions, "test.questions").is_empty());
}

#[test]
fn inline_and_service_cancel_stream_and_approval_before_replacement() {
    install_hosted_fixture_resource_governor();
    let roots = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    let runtime = hosted_fixture_runtime();
    runtime.block_on(cancellation_fixture(&roots));
}

async fn cancellation_fixture(roots: &[tempfile::TempDir; 2]) {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        for (service, root) in [false, true].into_iter().zip(roots) {
            let (tx, mut captures) = mpsc::channel::<Capture>(8);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let provider_url = format!("http://{}/v1", listener.local_addr().unwrap());
            pin_hosted_fixture_route(root.path(), &provider_url);
            let _provider = AbortOnDropHandle::new(tokio::spawn(async move {
                axum::serve(listener, Router::new().route("/v1/chat/completions", post(provider)).with_state(tx)).await.unwrap();
            }));
            let storage = api::SharedStorage::new(rightkit_agent_engine_storage::Storage::open(root.path().join("cancel.db")).unwrap());
            let state = bind_canonical_membrane_fixture(api::DaemonState::new("").with_storage(storage).with_engine(api::EngineConfig {
                api_key: "test-key".into(), model_id: "moonshotai/kimi-k2.6".into(), base_url: provider_url, fallback_models: vec![],
            }));
            let id = if service { "cancel-service" } else { "cancel-inline" };
            let _ = crate::api_sessions::lifecycle::create_session_handler(AxumState(state.clone()), AxumJson(
                serde_json::from_value(json!({"id":id,"title":"Pinned cancellation fixture", "workspaceRoot":root.path(),
                    "model":"moonshotai/kimi-k2.6","providerId":"qa-hosted",
                    "allowedTools":["write_file"],"permissionMode":"ask"})).unwrap(),
            )).await.unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let daemon_url = format!("http://{}", listener.local_addr().unwrap());
            let router = crate::api::build_router(state.clone());
            let _daemon = AbortOnDropHandle::new(tokio::spawn(async move { axum::serve(listener, router).await.unwrap(); }));
            let host = if service {
                SessionExecutionHost::Service(ServiceHost { client: Arc::new(DaemonSessionClient::new(daemon_url, "".into())), context_degraded_reason: None })
            } else { SessionExecutionHost::Inline(Box::new(InlineHost { state: state.clone() })) };
            let mut args = output_mode_args_for_daemon_client_tests();
            args.prompt = "Wait for cancellation.".into();
            args.workspace = Some(root.path().to_string_lossy().into_owned());
            args.session_id = Some(id.into());
            args.auto_approve = false;
            args.bypass_approvals = false;
            args.output_format = OutputFormat::Silent;

            let control = RunCancellation::new();
            let running = launch(args.clone(), host.clone(), control.clone());
            let (_request, _held_stream) = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                captures.recv(),
            )
            .await
            .expect("provider must receive cancellable stream request")
            .unwrap();
            assert!(state.get_session(id).unwrap().live_turn_handle.is_active());
            control.cancel_and_wait().await.unwrap();
            assert_eq!(running.await.unwrap(), 130);
            assert!(!state.get_session(id).unwrap().live_turn_handle.is_active());

            let control = RunCancellation::new();
            let running = launch(args.clone(), host.clone(), control.clone());
            let (_, release) = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                captures.recv(),
            )
            .await
            .expect("provider must receive approval request")
            .unwrap();
            release.send(reply(true)).unwrap();
            let live = state.get_session(id).unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    if api::lock_recover(&live.pending_approvals, "test.approvals")
                        .contains_key("cancel-clock")
                    {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("tool approval must be requested");
            control.cancel_and_wait().await.unwrap();
            assert_eq!(running.await.unwrap(), 130);
            assert!(!live.live_turn_handle.is_active());
            assert!(api::lock_recover(&live.pending_approvals, "test.approvals").is_empty());

            let control = RunCancellation::new();
            args.prompt = "Complete replacement.".into();
            let running = launch(args.clone(), host.clone(), control.clone());
            let (_, release) = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                captures.recv(),
            )
            .await
            .expect("provider must receive replacement request")
            .unwrap();
            release.send(reply(false)).unwrap();
            assert_eq!(running.await.unwrap(), 0);
            assert!(control.cancel_and_wait().await.is_ok());
            assert!(!live.live_turn_handle.is_active());
            assert!(api::lock_recover(&live.transcript, "test.transcript").iter().any(|message|
                matches!(message, rightkit_agent_engine_protocol::TranscriptMessage::Assistant { content, .. } if content == "REPLACEMENT_OK")));

            for approved in [true, false] {
                let call_id = if approved { "cancel-clock-approved" } else { "cancel-clock-denied" };
                let control = RunCancellation::new();
                let running = launch(args.clone(), host.clone(), control.clone());
                let (_, release) = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    captures.recv(),
                )
                .await
                .expect("provider must receive approved tool request")
                .unwrap();
                if approved { host.steer(id, "HOST_STEERING_MARKER").await.unwrap(); }
                release.send(reply_with_call_id(true, call_id)).unwrap();
                tokio::time::timeout(std::time::Duration::from_secs(10), async {
                    loop {
                        if api::lock_recover(&live.pending_approvals, "test.approvals")
                            .contains_key(call_id)
                        {
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    }
                })
                .await
                .expect("tool approval must be requested");
                host.resolve_approval(id, call_id, approved).await.unwrap();
                assert!(host.resolve_approval(id, "missing-approval", approved).await.is_err());
                let (request, release) = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    captures.recv(),
                )
                .await
                .expect("provider must receive tool outcome")
                .unwrap();
                if approved { assert!(request["messages"].to_string().contains("HOST_STEERING_MARKER"), "steering reaches next provider boundary"); }
                let result = request["messages"].as_array().unwrap().iter().rev()
                    .find(|message| message["role"] == "tool").expect("tool outcome reaches provider");
                let content = result["content"].as_str().unwrap().to_lowercase();
                assert_eq!(content.contains("denied"), !approved, "unexpected tool outcome: {content}");
                release.send(reply(false)).unwrap();
                assert_eq!(running.await.unwrap(), 0);
                assert!(control.require_settled().is_ok());
            }
        }
    }).await.expect("both host cancellation paths must settle within 30 seconds");
}

async fn parity_provider(
    State(tx): State<mpsc::Sender<Capture>>,
    Json(body): Json<Value>,
) -> axum::response::Response {
    let fail = body.to_string().contains("__HOST_PARITY_ERROR__");
    let (release, waiting) = oneshot::channel();
    tx.send((body, release)).await.unwrap();
    if fail {
        return axum::response::Response::builder()
            .status(400)
            .body(Body::from(
                r#"{"error":{"message":"HOST_PARITY_ERROR","type":"invalid_request_error"}}"#,
            ))
            .unwrap();
    }
    // Keep headers pending, making cancellation position deterministic.
    let payload = waiting.await.unwrap_or_default();
    axum::response::Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from(payload))
        .unwrap()
}

fn parity_metadata(value: &mut Value, provider_url: &str) {
    match value {
        Value::Object(fields) => {
            for key in ["seq", "createdAt", "turnId"] {
                fields.remove(key);
            }
            for value in fields.values_mut() {
                parity_metadata(value, provider_url);
            }
        }
        Value::Array(items) => {
            for value in items {
                parity_metadata(value, provider_url);
            }
        }
        Value::String(text) => *text = text.replace(provider_url, "<provider-transport>"),
        _ => {}
    }
}

#[test]
fn inline_service_semantic_golden() {
    install_hosted_fixture_resource_governor();
    let root = tempfile::tempdir().unwrap();
    let runtime = hosted_fixture_runtime();
    runtime.block_on(semantic_golden_fixture(&root));
}

async fn semantic_golden_fixture(root: &tempfile::TempDir) {
    tokio::time::timeout(std::time::Duration::from_secs(45), async {
        for scenario in ["text", "allow", "deny", "cancel", "error"] {
            let mut traces = Vec::new();
            let mut idle_cancellation_telemetry = Vec::new();
            let mut exits = Vec::new();
            for service in [false, true] {
                // KRN-002 compares effects from identical workspace preimages.
                // Both hosts must perform the same replacement, rather than
                // the service host overwriting the inline host's new file.
                std::fs::write(root.path().join("approval-fixture.txt"), "before").unwrap();
                let (tx, mut captures) = mpsc::channel::<Capture>(8);
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let provider_url = format!("http://{}/v1", listener.local_addr().unwrap());
                pin_hosted_fixture_route(root.path(), &provider_url);
                let _provider = AbortOnDropHandle::new(tokio::spawn(async move {
                    axum::serve(listener, Router::new().route("/v1/chat/completions", post(parity_provider)).with_state(tx)).await.unwrap();
                }));
                let storage = api::SharedStorage::new(rightkit_agent_engine_storage::Storage::open(root.path().join(format!("{scenario}-{service}.db"))).unwrap());
                let state = bind_canonical_membrane_fixture(api::DaemonState::new("").with_storage(storage).with_engine(api::EngineConfig {
                    api_key: "test-key".into(), model_id: "moonshotai/kimi-k2.6".into(), base_url: provider_url.clone(), fallback_models: vec![],
                }));
                let id = format!("golden-{scenario}");
                let _ = crate::api_sessions::lifecycle::create_session_handler(AxumState(state.clone()), AxumJson(
                    serde_json::from_value(json!({"id":id,"title":"Golden fixture", "workspaceRoot":root.path(),
                        "model":"moonshotai/kimi-k2.6","providerId":"qa-hosted",
                    "allowedTools":["read_file","write_file"],"permissionMode":"ask"})).unwrap(),
                )).await.unwrap();
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let daemon_url = format!("http://{}", listener.local_addr().unwrap());
                let router = crate::api::build_router(state.clone());
                let _daemon = AbortOnDropHandle::new(tokio::spawn(async move { axum::serve(listener, router).await.unwrap(); }));
                let host = if service { SessionExecutionHost::Service(ServiceHost { client: Arc::new(DaemonSessionClient::new(daemon_url, "".into())), context_degraded_reason: None }) }
                    else { SessionExecutionHost::Inline(Box::new(InlineHost { state: state.clone() })) };
                let mut args = output_mode_args_for_daemon_client_tests();
                args.prompt = if scenario == "error" { "__HOST_PARITY_ERROR__" } else { "Execute golden turn." }.into();
                args.workspace = Some(root.path().to_string_lossy().into_owned());
                args.session_id = Some(id.clone());
                args.auto_approve = false;
                args.bypass_approvals = false;
                args.output_format = OutputFormat::Silent;
                let control = RunCancellation::new();
                let running = launch(args.clone(), host.clone(), control.clone());
                let (_, release) = captures.recv().await.unwrap();
                match scenario {
                    "cancel" => { control.cancel_and_wait().await.unwrap(); drop(release); }
                    "error" => drop(release),
                    "allow" | "deny" => {
                        // EFF-005: replacing an existing file requires the
                        // session's complete read receipt. Exercise read_file
                        // through both hosts before requesting the overwrite.
                        let read_reply = format!(
                            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"golden-read","type":"function",
                                "function":{"name":"read_file","arguments":json!({"path":"approval-fixture.txt"}).to_string()}}]},"finish_reason":null}]}),
                            json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
                        );
                        release.send(read_reply).unwrap();
                        let (request, release) = captures.recv().await.unwrap();
                        let read_result = request["messages"].as_array().unwrap().iter()
                            .find(|message| message["role"] == "tool" && message["tool_call_id"] == "golden-read")
                            .expect("complete file read reaches provider");
                        assert!(read_result["content"].as_str().unwrap().contains("before"), "unexpected read outcome: {read_result}");
                        if scenario == "allow" { host.steer(&id, "GOLDEN_STEER").await.unwrap(); }
                        let tool_reply = reply(true);
                        release.send(tool_reply).unwrap();
                        let live = state.get_session(&id).unwrap();
                        loop {
                            if api::lock_recover(&live.pending_approvals, "golden.approvals").contains_key("cancel-clock") { break; }
                            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                        }
                        host.resolve_approval(&id, "cancel-clock", scenario == "allow").await.unwrap();
                        let (request, release) = captures.recv().await.unwrap();
                        if scenario == "allow" { assert!(request["messages"].to_string().contains("GOLDEN_STEER")); }
                        let write_result = request["messages"].as_array().unwrap().iter()
                            .find(|message| message["role"] == "tool" && message["tool_call_id"] == "cancel-clock")
                            .expect("write outcome reaches provider");
                        let content = write_result["content"].as_str().unwrap();
                        assert!(
                            if scenario == "allow" { content.contains("Overwrote") } else { content.to_lowercase().contains("denied") },
                            "unexpected {scenario} write outcome: {content}",
                        );
                        release.send(reply(false)).unwrap();
                    }
                    _ => { release.send(reply(false)).unwrap(); }
                }
                let exit = running.await.unwrap();
                assert_eq!(
                    std::fs::read_to_string(root.path().join("approval-fixture.txt")).unwrap(),
                    if scenario == "allow" { "approved" } else { "before" },
                    "{scenario} workspace effect",
                );
                assert!(control.require_settled().is_ok(), "{scenario} must settle");
                assert!(!state.get_session(&id).unwrap().live_turn_handle.is_active());
                if scenario == "cancel" { assert_eq!(exit, 130); }
                else if scenario == "error" { assert_ne!(exit, 0); }
                else { assert_eq!(exit, 0); }
                let mut events = host.events_after(&id, 0).await.unwrap();
                if scenario == "cancel" {
                    // The lifecycle observer publishes independently of turn
                    // settlement. Await its receipt before comparing counts.
                    events = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                        loop {
                            let snapshot = host.events_after(&id, 0).await.unwrap();
                            if snapshot.iter().any(|event| event["observation"]["kind"] == "background_shell_cancel") {
                                break snapshot;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                        }
                    }).await.expect("cancellation lifecycle receipt must arrive");
                }
                for event in &mut events { parity_metadata(event, &provider_url); }
                // A zero-victim background cancellation reports lifecycle only:
                // its independent observer may publish on either side of turn
                // termination. Preserve content/count separately. Any actual
                // cancelled shell remains in the ordered semantic trace.
                let mut lifecycle = Vec::new();
                events.retain(|event| {
                    let observation = &event["observation"];
                    if event["$kind"] == "orchestrationObservationRecorded"
                        && observation["kind"] == "background_shell_cancel"
                        && observation["fields"]["victims"] == "0"
                        && observation["fields"]["killedShellIds"] == ""
                    {
                        let mut telemetry = event.clone();
                        telemetry["observation"].as_object_mut().unwrap().remove("id");
                        lifecycle.push(telemetry);
                        false
                    } else { true }
                });
                idle_cancellation_telemetry.push(lifecycle);
                let kinds: Vec<_> = events.iter().filter_map(|event| event["$kind"].as_str()).collect();
                assert!(kinds.contains(&if scenario == "error" { "runFailed" } else if scenario == "cancel" { "runInterrupted" } else { "assistantMessageCompleted" }));
                let expected_tool_calls = match scenario { "allow" => 2, "deny" => 1, _ => 0 };
                assert_eq!(kinds.iter().filter(|kind| **kind == "toolCallStarted").count(), expected_tool_calls);
                exits.push(exit);
                traces.push(events);
            }
            assert_eq!(exits[0], exits[1], "{scenario} exit parity");
            assert_eq!(idle_cancellation_telemetry[0], idle_cancellation_telemetry[1], "{scenario} zero-effect lifecycle telemetry parity");
            assert_eq!(traces[0], traces[1], "{scenario} semantic event/effect parity");
            println!("GOLDEN {scenario}: {} events, exit {}", traces[0].len(), exits[0]);
        }
    }).await.expect("semantic golden must complete within 45 seconds");
}
