mod live_plugin_catalog_tests {
    use super::*;
    use axum::body::{Body, Bytes};
    use axum::http::{Method, Request};
    use tower::ServiceExt;

    type Capture = (Value, tokio::sync::oneshot::Sender<String>);

    async fn next_request(
        captured: &mut tokio::sync::mpsc::Receiver<Capture>,
        state: &DaemonState,
        app: &Router,
    ) -> Capture {
        // The bound exists to catch a hung turn, not to assert latency: a
        // ~2s-per-request debug turn starves past 5s under the full-suite
        // parallel load on a shared runtime thread.
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                tokio::select! {
                    request = captured.recv() => return request.expect("provider capture closed"),
                    _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {
                        for live in state.list_sessions() {
                            let pending = crate::api::lock_recover(&live.pending_approvals, "test.approvals")
                                .keys().cloned().collect::<Vec<_>>();
                            for id in pending {
                                assert_eq!(live.id, "catalog-main");
                                let expected_tool = match id.as_str() {
                                    "discover-alpha" | "discover-beta" => "tool_search",
                                    "clock-0" | "clock-1" | "clock-2" => "get_time",
                                    other => panic!("unexpected fixture approval: {other}"),
                                };
                                let context = crate::api::lock_recover(&live.pending_approval_contexts, "test.approval_contexts")
                                    .get(&id).cloned().expect("pending approval has context");
                                assert_eq!(context.tool_name, expected_tool);
                                request(app, Method::POST, &format!("/sessions/{}/tool-approval", live.id),
                                    json!({"toolCallId":id,"approved":true})).await;
                            }
                        }
                    }
                }
            }
        }).await.expect("next catalog request must arrive within 15 seconds")
    }
    async fn provider(
        State(captures): State<tokio::sync::mpsc::Sender<Capture>>,
        Json(body): Json<Value>,
    ) -> axum::response::Response {
        eprintln!(
            "catalog provider request: plugins={:?}, messages={}",
            plugin_names(&body),
            body["messages"].as_array().map_or(0, Vec::len)
        );
        let (release, waiting) = tokio::sync::oneshot::channel();
        captures.send((body, release)).await.unwrap();
        axum::response::Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from_stream(futures_util::stream::once(async move {
                Ok::<_, std::io::Error>(Bytes::from(waiting.await.unwrap()))
            })))
            .unwrap()
    }

    fn response(tool: bool, index: usize) -> String {
        let delta = if tool {
            json!({"tool_calls":[{"index":0,"id":format!("clock-{index}"),"type":"function",
                "function":{"name":"get_time","arguments":json!({"operation":"current_time",
                    "timezone":(["UTC","Asia/Kolkata","America/New_York"][index])}).to_string()}}]})
        } else {
            json!({"content":"CATALOG_OK"})
        };
        format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"index":0,"delta":delta,"finish_reason":null}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":if tool {"tool_calls"} else {"stop"}}]})
        )
    }

    fn discover_response(query: &str) -> String {
        format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,
                "id":format!("discover-{query}"),"type":"function",
                "function":{"name":"tool_search","arguments":json!({"query":query}).to_string()}}]},"finish_reason":null}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]})
        )
    }

    fn write_plugin(root: &FsPath, id: &str, enabled: bool) {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("plugin.py"), "").unwrap();
        std::fs::write(
            dir.join("plugin.json"),
            json!({
                "id":id,"name":id,"version":"1.0.0","description":"fixture",
                "entry":"plugin.py","provides_tools":[format!("{id}_tool")],
                "provides_commands":[],"enabled":enabled
            })
            .to_string(),
        )
        .unwrap();
    }

    async fn request(app: &Router, method: Method, path: &str, body: Value) {
        eprintln!("catalog route: {method} {path}");
        let result = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(result.status().is_success(), "{path}: {}", result.status());
        eprintln!("catalog route complete: {path}");
    }

    fn plugin_names(body: &Value) -> Vec<String> {
        let mut names = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["function"]["name"].as_str())
            .filter(|name| matches!(*name, "alpha_tool" | "beta_tool"))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    async fn settled(state: &DaemonState, id: &str) {
        let live = state.get_session(id).unwrap();
        loop {
            let completed = crate::api::lock_recover(&live.transcript, "test.transcript").iter().any(|message|
                matches!(message, rightkit_agent_engine_protocol::TranscriptMessage::Assistant { content, .. } if content == "CATALOG_OK"));
            if !live.live_turn_handle.is_active() && completed {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn routes_refresh_only_future_provider_requests_with_session_policy() {
        rightkit_agent_tools::coding_tools::process_resource_authority::install_process_resource_profile(
            rightkit_agent_tools::coding_tools::process_resource_authority::ResourceProfile::Expanded,
        )
        .expect("catalog fixture must install its process resource authority");
        tokio::time::timeout(std::time::Duration::from_secs(120), async {
            let root = tempfile::tempdir().unwrap();
            let plugins = root.path().join("plugins");
            write_plugin(&plugins, "alpha", true);
            write_plugin(&plugins, "beta", false);
            std::fs::create_dir(root.path().join(".coderight")).unwrap();
            std::fs::write(root.path().join(".coderight/config.toml"), "[tools]\nlazy-tool-loading = false\n").unwrap();
            assert!(!crate::api::load_workspace_config_with_env(root.path()).tools.lazy_tool_loading());
            let (captures, mut captured) = tokio::sync::mpsc::channel::<Capture>(8);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/v1", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                axum::serve(listener, Router::new().route("/v1/chat/completions", post(provider)).with_state(captures)).await.unwrap();
            });
            let storage = crate::api::SharedStorage::new(
                rightkit_agent_engine_storage::Storage::open(root.path().join("catalog.db")).unwrap(),
            );
            let state = DaemonState::new("").with_storage(storage).with_plugin_dir(plugins).with_engine(crate::api::EngineConfig {
                api_key:"test-key".into(),model_id:"moonshotai/kimi-k2.6".into(),base_url:url,fallback_models:vec![],
            });
            let app = crate::api::build_router(state.clone());
            for (id, allowed) in [("catalog-main", vec!["get_time","tool_search","alpha_tool","beta_tool"]),
                                  ("catalog-restricted", vec!["get_time","tool_search","alpha_tool"])] {
                request(&app, Method::POST, "/sessions", json!({"id":id,
                    "title":"Pinned catalog fixture","workspaceRoot":root.path(),"allowedTools":allowed})).await;
                assert_eq!(state.get_session(id).unwrap().allowed_tools, allowed);
            }
            request(&app, Method::POST, "/sessions/catalog-main/messages", json!({"text":"Run catalog fixture."})).await;
            let (initial, release_initial) = next_request(&mut captured, &state, &app).await;
            assert!(plugin_names(&initial).is_empty(), "plugin schemas require discovery");
            release_initial.send(discover_response("alpha")).unwrap();
            let (first, release_first) = next_request(&mut captured, &state, &app).await;
            assert_eq!(plugin_names(&first), ["alpha_tool"], "first discovered request: {first}");
            let original_schema = serde_json::to_vec(&first["tools"]).unwrap();

            request(&app, Method::POST, "/sessions/catalog-main/plugins/alpha/disable", json!({})).await;
            request(&app, Method::POST, "/sessions/catalog-main/plugins/beta/enable", json!({})).await;
            assert_eq!(serde_json::to_vec(&first["tools"]).unwrap(), original_schema);
            release_first.send(response(true, 0)).unwrap();
            let (undiscovered, release_undiscovered) = next_request(&mut captured, &state, &app).await;
            assert!(plugin_names(&undiscovered).is_empty(), "removed alpha disappears; beta awaits discovery");
            release_undiscovered.send(discover_response("beta")).unwrap();
            let (second, release_second) = next_request(&mut captured, &state, &app).await;
            assert_eq!(plugin_names(&second), ["beta_tool"], "session override must enable globally disabled plugin");

            request(&app, Method::DELETE, "/sessions/catalog-main/plugins", json!({})).await;
            release_second.send(response(true, 1)).unwrap();
            let (third, release_third) = next_request(&mut captured, &state, &app).await;
            assert_eq!(plugin_names(&third), ["alpha_tool"], "clear restores global activation");

            request(&app, Method::POST, "/plugins/alpha/disable", json!({})).await;
            request(&app, Method::POST, "/plugins/beta/enable", json!({})).await;
            let beta_path = state.plugin_dir.join("beta/plugin.json");
            let mut beta: Value = serde_json::from_slice(&std::fs::read(&beta_path).unwrap()).unwrap();
            beta["tools"] = json!({"beta_tool":{
                "description":"Reloaded beta schema",
                "inputSchema":{"type":"object","properties":{"revision":{"type":"integer"}}}
            }});
            std::fs::write(beta_path, serde_json::to_vec(&beta).unwrap()).unwrap();
            request(&app, Method::POST, "/plugins/reload", json!({})).await;
            release_third.send(response(true, 2)).unwrap();
            let (fourth, release_fourth) = next_request(&mut captured, &state, &app).await;
            assert_eq!(plugin_names(&fourth), ["beta_tool"]);
            let beta_schema = fourth["tools"].as_array().unwrap().iter()
                .find(|tool| tool["function"]["name"] == "beta_tool").unwrap();
            assert_eq!(beta_schema["function"]["description"], "Reloaded beta schema");
            assert_eq!(beta_schema["function"]["parameters"]["properties"]["revision"]["type"], "integer");
            release_fourth.send(response(false, 3)).unwrap();
            settled(&state, "catalog-main").await;
            let live = state.get_session("catalog-main").unwrap();
            let successful_tools = crate::api::lock_recover(&live.transcript, "test.transcript")
                .iter().filter(|message| matches!(message,
                    rightkit_agent_engine_protocol::TranscriptMessage::Tool { tool_call_id, is_error: false, .. }
                    if tool_call_id.starts_with("clock-"))).count();
            assert_eq!(successful_tools, 3, "all provider-boundary tool rounds must execute successfully");
            let successful_discoveries = crate::api::lock_recover(&live.transcript, "test.transcript")
                .iter().filter(|message| matches!(message,
                    rightkit_agent_engine_protocol::TranscriptMessage::Tool { tool_call_id, is_error: false, .. }
                    if tool_call_id.starts_with("discover-"))).count();
            assert_eq!(successful_discoveries, 2, "both catalogs must be discovered through live tool_search");

            request(&app, Method::POST, "/sessions/catalog-restricted/messages", json!({"text":"Check restricted catalog."})).await;
            let (restricted, release_restricted) = next_request(&mut captured, &state, &app).await;
            assert!(plugin_names(&restricted).is_empty(), "global enable must not widen another session's allow-list");
            release_restricted.send(response(false, 4)).unwrap();
            settled(&state, "catalog-restricted").await;
            server.abort();
        }).await.expect("live plugin catalog fixture must settle within 30 seconds");
    }

    #[test]
    fn session_overrides_preserve_role_deny_and_safe_mode_filters() {
        let root = tempfile::tempdir().unwrap();
        write_plugin(root.path(), "alpha", false);
        write_plugin(root.path(), "beta", false);
        let enabled = vec!["alpha".to_owned(), "beta".to_owned()];
        let role = vec!["get_time".to_owned(), "alpha_tool".to_owned()];
        let requested = vec![
            "get_time".to_owned(),
            "alpha_tool".to_owned(),
            "beta_tool".to_owned(),
        ];
        for (safe, denied, expect_alpha) in [
            (false, vec![], true),
            (false, vec!["alpha_tool".to_owned()], false),
            (true, vec![], false),
        ] {
            let registry = crate::api::build_session_tool_registry_for_plugins(
                (root.path().to_path_buf(), &enabled),
                safe,
                Some(&role),
                &requested,
                &denied,
                false,
                rightkit_agent_tools::coding_tools::ModelToolInterface::LegacyPromoted,
                None,
            );
            assert_eq!(registry.get("alpha_tool").is_some(), expect_alpha);
            assert!(registry.get("beta_tool").is_none());
        }
    }

    #[cfg(feature = "qualification")]
    async fn eff001_provider(
        State(captures): State<tokio::sync::mpsc::Sender<Capture>>,
        Json(_body): Json<Value>,
    ) -> axum::response::Response {
        let (release, waiting) = tokio::sync::oneshot::channel();
        captures
            .send((json!({"capture":"eff001"}), release))
            .await
            .unwrap();
        axum::response::Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from_stream(futures_util::stream::once(async move {
                Ok::<_, std::io::Error>(Bytes::from(waiting.await.unwrap()))
            })))
            .unwrap()
    }

    #[cfg(feature = "qualification")]
    #[tokio::test]
    async fn eff001_real_session_route_records_shared_admission_trace() {
        rightkit_agent_tools::coding_tools::process_resource_authority::install_process_resource_profile(
            rightkit_agent_tools::coding_tools::process_resource_authority::ResourceProfile::Expanded,
        )
        .expect("qualification fixture must install process authority");
        let root = tempfile::tempdir().unwrap();
        let (captures, mut captured) = tokio::sync::mpsc::channel::<Capture>(2);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/v1/chat/completions", post(eff001_provider))
                    .with_state(captures),
            )
            .await
            .unwrap();
        });
        let storage = crate::api::SharedStorage::new(
            rightkit_agent_engine_storage::Storage::open(&root.path().join("eff001.db")).unwrap(),
        );
        let state =
            DaemonState::new("")
                .with_storage(storage)
                .with_engine(crate::api::EngineConfig {
                    api_key: "test-key".into(),
                    model_id: "moonshotai/kimi-k2.6".into(),
                    base_url: url,
                    fallback_models: vec![],
                });
        let app = crate::api::build_router(state.clone());
        request(&app, Method::POST, "/sessions", json!({"id":"eff001-session","title":"EFF-001 route fixture","permissionMode":"bypass","workspaceRoot":root.path(),"allowedTools":["get_time"]})).await;
        request(
            &app,
            Method::POST,
            "/sessions/eff001-session/messages",
            json!({"text":"run"}),
        )
        .await;
        assert!(rightkit_agent_engine_core::qualification_trace::snapshot()
            .stages_for("eff001-call-9f7e")
            .is_empty());
        let (_, release) =
            tokio::time::timeout(std::time::Duration::from_secs(15), captured.recv())
                .await
                .unwrap()
                .unwrap();
        release.send(format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n", json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"eff001-call-9f7e","type":"function","function":{"name":"get_time","arguments":json!({"operation":"current_time","timezone":"UTC"}).to_string()}}]},"finish_reason":null}]}), json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}))).unwrap();
        let (_, release_terminal) =
            tokio::time::timeout(std::time::Duration::from_secs(15), captured.recv())
                .await
                .unwrap()
                .unwrap();
        release_terminal.send(response(false, 1)).unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            settled(&state, "eff001-session"),
        )
        .await
        .expect("EFF-001 admitted session must settle after terminal provider response");
        let trace = rightkit_agent_engine_core::qualification_trace::snapshot();
        assert_eq!(
            trace.stages_for("eff001-call-9f7e"),
            rightkit_agent_engine_core::qualification_trace::EffectAdmissionStage::canonical_order()
        );
        assert!(trace.is_canonically_ordered("eff001-call-9f7e"));
        assert!(trace.reached_execute("eff001-call-9f7e"));
        server.abort();
    }

    #[cfg(feature = "qualification")]
    #[tokio::test]
    async fn eff001_workflow_node_records_shared_admission_trace() {
        async fn next_workflow_request(
            captured: &mut tokio::sync::mpsc::Receiver<Capture>,
        ) -> tokio::sync::oneshot::Sender<String> {
            loop {
                let (body, release) = captured
                    .recv()
                    .await
                    .expect("workflow provider capture closed");
                let is_workflow = body["messages"].as_array().is_some_and(|messages| {
                    let is_title = messages.iter().any(|message| {
                        message["content"].as_str().is_some_and(|content| {
                            content.starts_with("Create a short title for this coding session.")
                        })
                    });
                    !is_title
                        && messages.iter().any(|message| {
                            message["content"]
                                .as_str()
                                .is_some_and(|content| content.contains("run workflow effect"))
                        })
                });
                if is_workflow {
                    return release;
                }
                release
                    .send(response(false, 0))
                    .expect("auxiliary provider request must settle");
            }
        }
        rightkit_agent_tools::coding_tools::process_resource_authority::install_process_resource_profile(
            rightkit_agent_tools::coding_tools::process_resource_authority::ResourceProfile::Expanded,
        )
        .expect("qualification fixture must install process authority");
        let root = tempfile::tempdir().unwrap();
        let (captures, mut captured) = tokio::sync::mpsc::channel::<Capture>(2);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/v1/chat/completions", post(provider))
                    .with_state(captures),
            )
            .await
            .unwrap();
        });
        let storage = crate::api::SharedStorage::new(
            rightkit_agent_engine_storage::Storage::open(&root.path().join("eff001-workflow.db")).unwrap(),
        );
        let state =
            DaemonState::new("")
                .with_storage(storage)
                .with_engine(crate::api::EngineConfig {
                    api_key: "test-key".into(),
                    model_id: "moonshotai/kimi-k2.6".into(),
                    base_url: url,
                    fallback_models: vec![],
                });
        let dispatcher = crate::workflow::dispatcher::RuntimeNodeDispatcher::new(
            std::sync::Arc::new(state.clone()),
        );
        let mut ctx = crate::workflow::dispatcher::NodeDispatchContext::for_test();
        ctx.workspace_root = root.path().to_string_lossy().into_owned();
        let spec = rightkit_workflow::domain::NodeSpec {
            id: "node-eff001-workflow".into(),
            kind: rightkit_workflow::domain::NodeKind::Agent,
            label: Some("EFF-001 workflow fixture".into()),
            capabilities: Default::default(),
            edges: vec![],
            retry: rightkit_workflow::domain::RetryPolicy {
                max_attempts: 1,
                backoff: rightkit_workflow::domain::BackoffStrategy::Fixed { delay_ms: 0 },
                retryable: vec![],
            },
            config: rightkit_workflow::domain::NodeConfig::Execution {
                runtime: "native".into(),
                instruction: "run workflow effect".into(),
                runtime_target: None,
                recipe_ref: None,
                inputs_json: "{}".into(),
                transform_operation: None,
                template: None,
                max_runtime_ms: 10_000,
                max_output_bytes: 65_536,
                output_schema: "schema://reference/opaque".into(),
            },
        };
        ctx.compiled_plan = rightkit_workflow::prelude::CompiledPlan {
            entry_node: spec.id.clone(),
            nodes: vec![spec.clone()],
            effective_capabilities: spec.capabilities.clone(),
        };
        let run = tokio::spawn(async move {
            use crate::workflow::dispatcher::ExecuteNode;
            dispatcher.execute_node(&spec, &ctx).await
        });
        let release = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            next_workflow_request(&mut captured),
        )
        .await
        .unwrap();
        release.send(format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n", json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"workflow-eff001-call-9f7e","type":"function","function":{"name":"get_time","arguments":json!({"operation":"current_time","timezone":"UTC"}).to_string()}}]},"finish_reason":null}]}), json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}))).unwrap();
        let approval_id = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                if let Some(live) = state.list_sessions().into_iter().find(|live| {
                    crate::api::lock_recover(&live.pending_approvals, "workflow.approvals")
                        .contains_key("workflow-eff001-call-9f7e")
                }) {
                    break (live.id).to_string();
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("workflow approval must become pending");
        request(
            &crate::api::build_router(state.clone()),
            Method::POST,
            &format!("/sessions/{approval_id}/tool-approval"),
            json!({"toolCallId":"workflow-eff001-call-9f7e","approved":true}),
        )
        .await;
        let release_terminal = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            next_workflow_request(&mut captured),
        )
        .await
        .unwrap();
        release_terminal.send(response(false, 1)).unwrap();
        let output = tokio::time::timeout(std::time::Duration::from_secs(30), run)
            .await
            .unwrap()
            .unwrap();
        assert!(output.is_ok(), "workflow node must settle: {output:?}");
        let trace = rightkit_agent_engine_core::qualification_trace::snapshot();
        assert_eq!(
            trace.stages_for("workflow-eff001-call-9f7e"),
            rightkit_agent_engine_core::qualification_trace::EffectAdmissionStage::canonical_order()
        );
        assert!(trace.is_canonically_ordered("workflow-eff001-call-9f7e"));
        assert!(trace.reached_execute("workflow-eff001-call-9f7e"));
        server.abort();
    }
    #[cfg(feature = "qualification")]
    #[tokio::test]
    async fn eff001_denied_session_route_settles_without_execute() {
        rightkit_agent_tools::coding_tools::process_resource_authority::install_process_resource_profile(
            rightkit_agent_tools::coding_tools::process_resource_authority::ResourceProfile::Expanded,
        )
        .expect("qualification fixture must install process authority");
        let root = tempfile::tempdir().unwrap();
        let (captures, mut captured) = tokio::sync::mpsc::channel::<Capture>(2);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/v1/chat/completions", post(eff001_provider))
                    .with_state(captures),
            )
            .await
            .unwrap();
        });
        let storage = crate::api::SharedStorage::new(
            rightkit_agent_engine_storage::Storage::open(&root.path().join("eff001-denied.db")).unwrap(),
        );
        let state =
            DaemonState::new("")
                .with_storage(storage)
                .with_engine(crate::api::EngineConfig {
                    api_key: "test-key".into(),
                    model_id: "moonshotai/kimi-k2.6".into(),
                    base_url: url,
                    fallback_models: vec![],
                });
        let app = crate::api::build_router(state.clone());
        request(&app, Method::POST, "/sessions", json!({"id":"eff001-denied","title":"EFF-001 denied fixture","permissionMode":"ask","workspaceRoot":root.path(),"allowedTools":["get_time"]})).await;
        request(
            &app,
            Method::POST,
            "/sessions/eff001-denied/messages",
            json!({"text":"run"}),
        )
        .await;
        let (_, release) =
            tokio::time::timeout(std::time::Duration::from_secs(15), captured.recv())
                .await
                .unwrap()
                .unwrap();
        release.send(format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n", json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"eff001-denied-9f7e","type":"function","function":{"name":"get_time","arguments":json!({"operation":"current_time","timezone":"UTC"}).to_string()}}]},"finish_reason":null}]}), json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}))).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                let live = state.get_session("eff001-denied").unwrap();
                let pending =
                    crate::api::lock_recover(&live.pending_approvals, "eff001.denied.approvals")
                        .contains_key("eff001-denied-9f7e");
                if pending {
                    let context = crate::api::lock_recover(
                        &live.pending_approval_contexts,
                        "eff001.denied.contexts",
                    )
                    .get("eff001-denied-9f7e")
                    .cloned()
                    .expect("pending approval has context");
                    assert_eq!(context.tool_name, "get_time");
                    request(
                        &app,
                        Method::POST,
                        "/sessions/eff001-denied/tool-approval",
                        json!({"toolCallId":"eff001-denied-9f7e","approved":false}),
                    )
                    .await;
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("EFF-001 denied approval must become pending");
        let (_, release_terminal) =
            tokio::time::timeout(std::time::Duration::from_secs(15), captured.recv())
                .await
                .unwrap()
                .unwrap();
        release_terminal.send(response(false, 1)).unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            settled(&state, "eff001-denied"),
        )
        .await
        .expect("EFF-001 denied session must settle after terminal provider response");
        let trace = rightkit_agent_engine_core::qualification_trace::snapshot();
        let stages = trace.stages_for("eff001-denied-9f7e");
        assert!(!stages.is_empty());
        assert!(trace.is_canonically_ordered("eff001-denied-9f7e"));
        assert!(!trace.reached_execute("eff001-denied-9f7e"));
        assert!(
            !stages.contains(&rightkit_agent_engine_core::qualification_trace::EffectAdmissionStage::Execute)
        );
        server.abort();
    }
}
