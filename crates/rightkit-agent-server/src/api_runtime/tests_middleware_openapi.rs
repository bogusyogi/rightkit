#[cfg(test)]
mod tests_middleware_openapi {
    use super::tests_support::*;
    use super::*;

    #[tokio::test]
    async fn runtime_middleware_route_lists_and_toggles_builtin_middleware() {
        let app = crate::api::build_router(DaemonState::new(""));

        let (status, json) = json_request(
            app.clone(),
            "GET",
            "/runtime/middleware",
            serde_json::json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(json["middleware"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| { row["id"] == "builtin.guard-tool-arguments" && row["enabled"] == true }));

        let (update_status, update) = json_request(
            app.clone(),
            "PUT",
            "/runtime/middleware/builtin.guard-tool-arguments/enabled",
            serde_json::json!({ "enabled": false }),
        )
        .await;
        assert_eq!(update_status, StatusCode::OK);
        assert_eq!(update["middleware"]["enabled"], false);

        let (_, after) =
            json_request(app, "GET", "/runtime/middleware", serde_json::json!({})).await;
        assert!(after["middleware"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| { row["id"] == "builtin.guard-tool-arguments" && row["enabled"] == false }));
    }

    fn ref_target_name(schema: &Value) -> &str {
        schema["$ref"]
            .as_str()
            .and_then(|reference| reference.strip_prefix("#/components/schemas/"))
            .expect("component schema ref")
    }

    #[test]
    fn runtime_openapi_components_cover_learning_skillopt_harness_and_benchmarks() {
        let components = runtime_openapi_components();
        let expected = [
            "RuntimeLearningResponse",
            "RuntimeLearningProposal",
            "SkillOptTarget",
            "SkillOptRejectedEdit",
            "SkillOptBenchmarkGate",
            "SkillOptEvalEvidence",
            "CreateSkillOptRunRequest",
            "ResumeSkillOptRunRequest",
            "ExecuteSkillOptEpochRequest",
            "ProposeSkillOptCandidateRequest",
            "CancelSkillOptRunRequest",
            "SkillOptRun",
            "SkillOptHistoryEntry",
            "SkillOptRunsResponse",
            "SkillOptRunResponse",
            "CreateSkillOptRunResponse",
            "ResumeSkillOptRunResponse",
            "ExecuteSkillOptEpochResponse",
            "ProposeSkillOptCandidateResponse",
            "CancelSkillOptRunResponse",
            "SkillOptRunHistoryResponse",
            "SkillOptRejectedEditsResponse",
            "HarnessRadarResponse",
            "PromoteHarnessRadarProposalResponse",
            "ScheduleHarnessRadarRefreshRequest",
            "ScheduleHarnessRadarRefreshResponse",
            "RuntimeBenchmarksResponse",
            "RuntimeBenchmarkReport",
        ];

        for name in expected {
            assert!(components.contains_key(name), "missing component {name}");
        }

        let helper_refs = [
            runtime_learning_response_schema(),
            create_skillopt_run_request_schema(),
            create_skillopt_run_response_schema(),
            skillopt_runs_response_schema(),
            skillopt_run_response_schema(),
            resume_skillopt_run_request_schema(),
            resume_skillopt_run_response_schema(),
            execute_skillopt_epoch_request_schema(),
            execute_skillopt_epoch_response_schema(),
            propose_skillopt_candidate_request_schema(),
            propose_skillopt_candidate_response_schema(),
            cancel_skillopt_run_request_schema(),
            cancel_skillopt_run_response_schema(),
            skillopt_run_history_response_schema(),
            skillopt_rejected_edits_response_schema(),
            harness_radar_response_schema(),
            promote_harness_radar_proposal_response_schema(),
            schedule_harness_radar_refresh_request_schema(),
            schedule_harness_radar_refresh_response_schema(),
            runtime_benchmarks_response_schema(),
        ];

        for schema in helper_refs {
            assert!(
                components.contains_key(ref_target_name(&schema)),
                "helper ref does not point at a runtime component: {schema:?}"
            );
        }

        assert_eq!(
            components["CreateSkillOptRunRequest"]["properties"]["target"]["$ref"],
            "#/components/schemas/SkillOptTarget"
        );
        assert_eq!(
            components["ExecuteSkillOptEpochRequest"]["properties"]["evalEvidence"]["$ref"],
            "#/components/schemas/SkillOptEvalEvidence"
        );
        assert!(components["ExecuteSkillOptEpochRequest"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field == "evalEvidence"));
        assert_eq!(
            components["SkillOptRun"]["properties"]["history"]["items"]["$ref"],
            "#/components/schemas/SkillOptHistoryEntry"
        );
        assert_eq!(
            components["HarnessRadarResponse"]["properties"]["capabilities"]["items"]["type"],
            "string"
        );
        assert_eq!(
            components["RuntimeBenchmarkReport"]["properties"]["successRate"]["type"],
            "number"
        );
    }

    #[test]
    fn runtime_openapi_helpers_cover_daemon_learning_skillopt_harness_and_benchmark_routes() {
        let components = runtime_openapi_components();
        let runtime_paths = [
            "/runtime/learning",
            "/runtime/skillopt/runs",
            "/runtime/skillopt/runs/{id}",
            "/runtime/skillopt/runs/{id}/resume",
            "/runtime/skillopt/runs/{id}/execute",
            "/runtime/skillopt/runs/{id}/propose-candidate",
            "/runtime/skillopt/runs/{id}/cancel",
            "/runtime/skillopt/runs/{id}/history",
            "/runtime/skillopt/runs/{id}/rejected-edits",
            "/runtime/harness-radar",
            "/runtime/harness-radar/proposals/{id}/promote",
            "/runtime/harness-radar/schedule",
            "/runtime/benchmarks",
        ];

        let endpoints: Vec<_> = crate::api_contract::DAEMON_API_ENDPOINTS
            .iter()
            .filter(|endpoint| runtime_paths.contains(&endpoint.path))
            .collect();
        assert_eq!(endpoints.len(), runtime_paths.len() + 1);

        for endpoint in endpoints {
            let method = endpoint.method.to_ascii_lowercase();
            let response = runtime_openapi_response_schema(&method, endpoint.path)
                .unwrap_or_else(|| panic!("missing response schema for {}", endpoint.path));
            assert!(
                components.contains_key(ref_target_name(&response)),
                "missing response component for {} {}",
                endpoint.method,
                endpoint.path
            );

            let request = runtime_openapi_request_schema(&method, endpoint.path);
            if matches!(method.as_str(), "post" | "put" | "patch") {
                let request = request
                    .unwrap_or_else(|| panic!("missing request schema for {}", endpoint.path));
                assert!(
                    components.contains_key(ref_target_name(&request)),
                    "missing request component for {} {}",
                    endpoint.method,
                    endpoint.path
                );
            } else {
                assert!(
                    request.is_none(),
                    "unexpected request schema for {} {}",
                    endpoint.method,
                    endpoint.path
                );
            }
        }
    }

    #[test]
    fn runtime_openapi_components_cover_middleware_routes() {
        let components = runtime_openapi_components();
        assert!(components.contains_key("RuntimeMiddlewareListResponse"));
        assert!(components.contains_key("SetRuntimeMiddlewareEnabledRequest"));
        assert!(components.contains_key("RuntimeMiddlewareUpdateResponse"));
        assert!(components.contains_key("RuntimeSpansResponse"));
        assert!(components.contains_key("RuntimeSpanResponse"));
        assert!(components.contains_key("RuntimeObservationsResponse"));
        assert_eq!(
            runtime_middleware_list_response_schema()["$ref"],
            "#/components/schemas/RuntimeMiddlewareListResponse"
        );
        assert_eq!(
            set_runtime_middleware_enabled_request_schema()["$ref"],
            "#/components/schemas/SetRuntimeMiddlewareEnabledRequest"
        );
        assert_eq!(
            runtime_spans_response_schema()["$ref"],
            "#/components/schemas/RuntimeSpansResponse"
        );
        assert_eq!(
            runtime_span_response_schema()["$ref"],
            "#/components/schemas/RuntimeSpanResponse"
        );
        assert_eq!(
            components["RuntimeSpan"]["properties"]["attributes"]["type"],
            "object"
        );
    }
}
