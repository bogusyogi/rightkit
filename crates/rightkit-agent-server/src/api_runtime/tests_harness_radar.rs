#[cfg(test)]
mod tests_harness_radar {
    use super::tests_support::*;
    use super::*;

    #[tokio::test]
    async fn harness_radar_route_exposes_sources_and_capability_map() {
        let app = crate::api::build_router(DaemonState::new(""));

        let (status, json) =
            json_request(app, "GET", "/runtime/harness-radar", serde_json::json!({})).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["sources"][0]["id"], "awesome-agent-harness");
        assert_eq!(json["sources"][0]["cadence"], "weekly");
        assert_eq!(json["sources"][0]["snapshotUpdatedAt"], "2026-06-08");
        assert_eq!(json["sources"][0]["minEntries"], 4);
        assert!(json["capabilities"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("execution-sandbox")));
        assert!(json["capabilities"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("protocol-contracts")));
        assert!(json["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["projectName"] == "Open Protocol"));
        assert!(
            json["capabilityMap"]["protocol-contracts"]
                .as_u64()
                .unwrap()
                > 0
        );
        assert!(json["proposals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|proposal| proposal["capability"] == "protocol-contracts"));
        assert_eq!(json["validations"][0]["sourceId"], "awesome-agent-harness");
        assert_eq!(json["validations"][0]["status"], "pass");
        assert_eq!(json["validations"][0]["entryCount"], 4);
        assert_eq!(
            json["validations"][0]["missingCapabilities"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        assert!(json["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|change| change["projectName"] == "Hermes Agent" && change["status"] == "new"));
        assert!(json["changes"].as_array().unwrap().iter().any(|change| {
            change["projectName"] == "Open Protocol"
                && change["status"] == "changed"
                && change["changedFields"]
                    .as_array()
                    .unwrap()
                    .contains(&serde_json::json!("summary"))
        }));
    }

    #[tokio::test]
    async fn harness_radar_proposal_can_be_promoted_to_learning_queue_once() {
        let app = crate::api::build_router(DaemonState::new(""));

        let (radar_status, radar) = json_request(
            app.clone(),
            "GET",
            "/runtime/harness-radar",
            serde_json::json!({}),
        )
        .await;
        assert_eq!(radar_status, StatusCode::OK);
        let proposal_id = radar["proposals"]
            .as_array()
            .unwrap()
            .iter()
            .find(|proposal| proposal["capability"] == "protocol-contracts")
            .and_then(|proposal| proposal["id"].as_str())
            .expect("protocol-contracts radar proposal")
            .to_string();

        let (promote_status, promoted) = json_request(
            app.clone(),
            "POST",
            &format!("/runtime/harness-radar/proposals/{proposal_id}/promote"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(promote_status, StatusCode::OK);
        assert_eq!(promoted["id"], proposal_id);
        assert_eq!(promoted["kind"], "HarnessRule");
        assert_eq!(promoted["status"], "Candidate");
        assert!(promoted["evidence"]
            .as_str()
            .unwrap()
            .contains("Open Protocol"));

        let (again_status, again) = json_request(
            app.clone(),
            "POST",
            &format!("/runtime/harness-radar/proposals/{proposal_id}/promote"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(again_status, StatusCode::OK);
        assert_eq!(again["id"], proposal_id);

        let (learning_status, learning) =
            json_request(app, "GET", "/runtime/learning", serde_json::json!({})).await;
        assert_eq!(learning_status, StatusCode::OK);
        assert_eq!(
            learning
                .as_array()
                .unwrap()
                .iter()
                .filter(|proposal| proposal["id"] == proposal_id)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn harness_radar_refresh_schedule_upserts_safe_research_job() {
        let root = std::env::temp_dir().join(format!(
            "coderight-harness-radar-schedule-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let app = crate::api::build_router(DaemonState::new(""));

        let body = serde_json::json!({
            "workspaceRoot": root,
            "cronExpr": "0 9 * * 1",
            "timeZone": "UTC",
            "paused": true
        });
        let (status, json) = json_request(
            app.clone(),
            "POST",
            "/runtime/harness-radar/schedule",
            body.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["id"], "harness-radar-refresh");
        assert_eq!(json["created"], true);
        assert_eq!(json["schedule"]["toolset"], "safe-research");
        assert_eq!(json["schedule"]["mode"], "agent");
        assert_eq!(json["schedule"]["executionBackend"], "local");
        assert_eq!(
            json["schedule"]["delivery"][0]["kind"],
            "desktop_notification"
        );
        assert!(json["schedule"]["prompt"]
            .as_str()
            .unwrap()
            .contains("assets/harness-radar/sources.toml"));

        let (again_status, again) =
            json_request(app, "POST", "/runtime/harness-radar/schedule", body).await;
        assert_eq!(again_status, StatusCode::OK);
        assert_eq!(again["updated"], true);

        let schedules = crate::api_schedules::load_api_schedules(&root).unwrap();
        let entry = schedules.get("harness-radar-refresh").unwrap();
        assert_eq!(entry.cron_expr, "0 9 * * 1");
        assert_eq!(entry.toolset.as_deref(), Some("safe-research"));
        assert_eq!(entry.delivery[0].kind, "desktop_notification");

        let _ = std::fs::remove_dir_all(&root);
    }
}
