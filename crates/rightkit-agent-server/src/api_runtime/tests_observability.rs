#[cfg(test)]
mod tests_observability {
    use super::tests_support::*;
    use super::*;

    use rightkit_agent_engine_runtime_middleware::{
        MiddlewarePhase, RuntimeObservation, RuntimeSpan,
    };

    #[tokio::test]
    async fn runtime_spans_route_filters_by_session_and_reads_span() {
        let state = DaemonState::new("");
        state
            .runtime_middleware
            .lock()
            .unwrap()
            .record_span(RuntimeSpan {
                id: "span-1".to_string(),
                parent_id: None,
                session_id: "s1".to_string(),
                turn_id: Some("turn-1".to_string()),
                phase: MiddlewarePhase::PreToolCall,
                status: "completed".to_string(),
                started_at: "2026-06-09T00:00:00Z".to_string(),
                ended_at: Some("2026-06-09T00:00:01Z".to_string()),
                attributes: serde_json::json!({ "tool": "shell" }),
            });
        state
            .runtime_middleware
            .lock()
            .unwrap()
            .record_span(RuntimeSpan {
                id: "span-2".to_string(),
                parent_id: None,
                session_id: "s2".to_string(),
                turn_id: None,
                phase: MiddlewarePhase::SessionStart,
                status: "completed".to_string(),
                started_at: "2026-06-09T00:00:00Z".to_string(),
                ended_at: None,
                attributes: serde_json::json!({}),
            });
        let app = crate::api::build_router(state);

        let (status, json) = json_request(
            app.clone(),
            "GET",
            "/runtime/spans?sessionId=s1",
            serde_json::json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let spans = json["spans"].as_array().unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0]["id"], "span-1");

        let (read_status, span) =
            json_request(app, "GET", "/runtime/spans/span-1", serde_json::json!({})).await;
        assert_eq!(read_status, StatusCode::OK);
        assert_eq!(span["attributes"]["tool"], "shell");
    }

    #[tokio::test]
    async fn runtime_spans_and_observations_survive_registry_reload() {
        let root =
            std::env::temp_dir().join(format!("coderight-runtime-route-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&root);
        let path = root.join("runtime-middleware.json");
        let _ = std::fs::remove_file(&path);

        let state = DaemonState::new("").with_runtime_middleware_store_path(path.clone());
        {
            let mut registry = state.runtime_middleware.lock().unwrap();
            registry.record_span(RuntimeSpan {
                id: "span-reload".to_string(),
                parent_id: None,
                session_id: "s-reload".to_string(),
                turn_id: Some("turn-reload".to_string()),
                phase: MiddlewarePhase::ApprovalRequested,
                status: "completed".to_string(),
                started_at: "2026-06-09T00:00:00Z".to_string(),
                ended_at: Some("2026-06-09T00:00:01Z".to_string()),
                attributes: serde_json::json!({ "tool": "shell" }),
            });
            registry.record_observation(RuntimeObservation {
                id: "obs-reload".to_string(),
                span_id: "span-reload".to_string(),
                session_id: "s-reload".to_string(),
                severity: "info".to_string(),
                message: "middleware decision persisted".to_string(),
                payload: serde_json::json!({ "decision": "emitObservation" }),
                created_at: "2026-06-09T00:00:01Z".to_string(),
            });
        }

        let app = crate::api::build_router(
            DaemonState::new("").with_runtime_middleware_store_path(path.clone()),
        );
        let (span_status, spans) = json_request(
            app.clone(),
            "GET",
            "/runtime/spans?sessionId=s-reload",
            serde_json::json!({}),
        )
        .await;
        assert_eq!(span_status, StatusCode::OK);
        assert_eq!(spans["spans"][0]["id"], "span-reload");

        let (observation_status, observations) = json_request(
            app,
            "GET",
            "/runtime/observations?sessionId=s-reload",
            serde_json::json!({}),
        )
        .await;
        assert_eq!(observation_status, StatusCode::OK);
        let rows = observations["observations"]
            .as_array()
            .expect("runtime observations");
        assert!(rows.iter().any(|row| row["id"] == "obs-reload"));
        let execution = rows
            .iter()
            .find(|row| row["id"] == "execution:span-reload")
            .expect("automatic execution observation");
        assert_eq!(
            execution["payload"]["schemaVersion"],
            "coderight.execution-observation.v1"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn resource_metrics_exposes_bounded_prompt_overhead_observations() {
        rightkit_logs::agent_telemetry::record_prompt_overhead(
            rightkit_logs::agent_telemetry::PromptOverheadEvent {
                provider: Some("fixture-provider".to_string()),
                model: Some("fixture-model".to_string()),
                system_prompt_bytes: 1,
                tool_prelude_bytes: 5,
            },
        );

        let metrics = resource_metrics_json(&DaemonState::new("")).await;
        assert_eq!(metrics["promptOverhead"]["available"], true);
        let row = metrics["promptOverhead"]["rows"]
            .as_array()
            .expect("prompt overhead rows")
            .iter()
            .find(|row| row["provider"] == "fixture-provider")
            .expect("fixture provider row");
        assert_eq!(row["totalSystemPromptEstTokens"], 1);
        assert_eq!(row["totalToolPreludeEstTokens"], 2);
    }

    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn resource_metrics_reports_rss_bytes_on_windows() {
        // Windows previously fell through to the platform-agnostic `None`
        // fallback (no `GetProcessMemoryInfo` call), losing memory telemetry
        // on this desktop target. A running test process always has a
        // non-zero working set, so a live daemon must report a real figure
        // here rather than `null`.
        let metrics = resource_metrics_json(&DaemonState::new("")).await;
        let rss = metrics["daemon"]["rssBytes"]
            .as_u64()
            .expect("rssBytes must be a non-null integer on Windows");
        assert!(rss > 0, "rssBytes must be a positive figure, got {rss}");
    }

    /// OPS-009 endpoint regression: the resource report must actually appear in
    /// the live `/runtime/resource-metrics` response, and must REFUSE a combined
    /// RightKit+Membrane claim when no Membrane process was observed.
    ///
    /// The adversarial pass noted the module was wired but nothing asserted the
    /// endpoint's shape, so a revert of the wiring would have gone unnoticed.
    #[tokio::test]
    async fn live_metrics_report_incremental_cost_and_refuse_the_combined_claim() {
        let state = DaemonState::new("");
        let metrics = resource_metrics_json(&state).await;

        let ops009 = metrics
            .get("ops009")
            .expect("OPS-009 report must be present in the live metrics response");
        assert_eq!(
            ops009.get("schema").and_then(|v| v.as_str()),
            Some("coderight.ops009.resource-report.v1")
        );

        // (1) incremental RightKit cost, measured for real.
        let incremental = ops009
            .get("incrementalRightKit")
            .expect("incremental RightKit cost must be reported");
        // The test binary legitimately has live children under `cargo test`
        // (spawned by sibling tests), and helpers are honestly counted — so the
        // claim is exactly one RightKit primary plus whatever helpers the OS
        // actually enumerated, never a fabricated lone process.
        let process_count = incremental
            .get("processCount")
            .and_then(|v| v.as_u64())
            .expect("processCount must be reported");
        let helpers = incremental
            .get("helperProcesses")
            .and_then(|v| v.as_u64())
            .expect("helperProcesses must be reported");
        assert_eq!(
            process_count,
            helpers + 1,
            "this process must be counted as exactly one RightKit primary"
        );
        assert!(
            incremental
                .get("rssBytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
                > 0,
            "RSS must be a real live reading, not a placeholder"
        );

        // (2) the combined claim must be refused, not silently reported.
        let combined = ops009
            .get("combinedWithMembrane")
            .expect("combined claim must be represented");
        assert_eq!(
            combined.get("available").and_then(|v| v.as_bool()),
            Some(false),
            "no Membrane process is observable from here, so a whole-system \
         claim must be refused rather than reported"
        );
        let refusal = combined
            .get("refusal")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(
            refusal.contains("not a whole-system result"),
            "the refusal must say why; got: {refusal}"
        );
    }
}
