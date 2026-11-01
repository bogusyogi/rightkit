#[cfg(test)]
mod eff001_subagent_route_tests {
    use super::*;
    use axum::body::{Body, Bytes};
    use axum::extract::{Json, State};
    use axum::http::{Method, Request};
    use axum::routing::post;
    use axum::{response::Response, Router};
    use serde_json::{json, Value};
    use tower::ServiceExt;
    type Capture = (Value, tokio::sync::oneshot::Sender<String>);
    async fn request(app: &Router, method: Method, path: &str, body: Value) {
        let r = app
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
        assert!(r.status().is_success(), "{path}: {}", r.status());
    }
    fn terminal() -> String {
        format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"index":0,"delta":{"content":"DONE"},"finish_reason":null}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
        )
    }
    async fn provider(
        State(tx): State<tokio::sync::mpsc::Sender<Capture>>,
        Json(payload): Json<Value>,
    ) -> Response {
        let (release, wait) = tokio::sync::oneshot::channel();
        tx.send((payload, release)).await.unwrap();
        Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from_stream(futures_util::stream::once(async move {
                Ok::<_, std::io::Error>(Bytes::from(wait.await.unwrap()))
            })))
            .unwrap()
    }
    #[cfg(feature = "qualification")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn eff001_real_subagent_route_records_shared_admission_trace() {
        rightkit_agent_tools::coding_tools::process_resource_authority::install_process_resource_profile(
            rightkit_agent_tools::coding_tools::process_resource_authority::ResourceProfile::Expanded,
        )
        .unwrap();
        let root = tempfile::tempdir().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Capture>(8);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/v1/chat/completions", post(provider))
                    .with_state(tx),
            )
            .await
            .unwrap();
        });
        let storage = crate::api::SharedStorage::new(
            rightkit_agent_engine_storage::Storage::open(&root.path().join("eff001-subagent.db")).unwrap(),
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
        request(&app, Method::POST, "/sessions", json!({"id":"eff001-subagent","title":"EFF-001 subagent","permissionMode":"bypass","workspaceRoot":root.path(),"allowedTools":["spawn_subagent","write_file"]})).await;
        request(
            &app,
            Method::POST,
            "/sessions/eff001-subagent/messages",
            json!({"text":"run subagent"}),
        )
        .await;
        let (_, release) = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        release.send(format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n", json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"eff001-subagent-call-9f7e","type":"function","function":{"name":"spawn_subagent","arguments":json!({"role":"coder","task":"return fixture result","maxTurns":1}).to_string()}}]},"finish_reason":null}]}), json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}))).unwrap();
        let (child_request, child_release) =
            tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
                .await
                .unwrap()
                .unwrap();
        assert!(
            child_request["messages"]
                .to_string()
                .contains("return fixture result"),
            "provider request must belong to child task: {child_request}"
        );
        let child_tools = child_request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["function"]["name"].as_str())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert!(
            child_tools.contains(&"write_file".to_string()),
            "coder must retain parent-admitted write capability: {child_tools:?}"
        );
        assert!(
            !child_tools.contains(&"spawn_subagent".to_string()),
            "coder role ceiling must exclude spawning: {child_tools:?}"
        );
        child_release.send(format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"eff001-child-call-9f7e","type":"function","function":{"name":"write_file","arguments":json!({"path":"child-effect.txt","content":"child effect executed"}).to_string()}}]},"finish_reason":null}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}))).unwrap();
        let (_child_followup_request, child_followup_release) =
            tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
                .await
                .unwrap()
                .unwrap();
        child_followup_release.send(terminal()).unwrap();
        // A mutating child may enter one more provider turn for post-edit
        // diagnostics before its awaited spawn result reaches the parent.
        // Drive those turns to completion; otherwise the test waits for a
        // parent request while the child is blocked on an unreleased route.
        let parent_release = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let (request, release) = rx.recv().await.expect("provider route closed");
                if request["messages"]
                    .to_string()
                    .contains("eff001-subagent-call-9f7e")
                {
                    break release;
                }
                release
                    .send(terminal())
                    .expect("child follow-up must settle");
            }
        })
        .await
        .unwrap();
        parent_release.send(terminal()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                if state
                    .get_session("eff001-subagent")
                    .is_none_or(|s| !s.live_turn_handle.is_active())
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await
            }
        })
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join("child-effect.txt")).unwrap(),
            "child effect executed"
        );
        let trace = rightkit_agent_engine_core::qualification_trace::snapshot();
        assert_eq!(
            trace.stages_for("eff001-subagent-call-9f7e"),
            rightkit_agent_engine_core::qualification_trace::EffectAdmissionStage::canonical_order()
        );
        assert!(trace.is_canonically_ordered("eff001-subagent-call-9f7e"));
        assert!(trace.reached_execute("eff001-subagent-call-9f7e"));
        server.abort();
        assert_eq!(
            trace.stages_for("eff001-child-call-9f7e"),
            rightkit_agent_engine_core::qualification_trace::EffectAdmissionStage::canonical_order()
        );
        assert!(trace.is_canonically_ordered("eff001-child-call-9f7e"));
        assert!(trace.reached_execute("eff001-child-call-9f7e"));
    }
}
