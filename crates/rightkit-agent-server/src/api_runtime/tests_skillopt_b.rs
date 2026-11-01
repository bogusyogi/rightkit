#[cfg(test)]
mod tests_skillopt_b {
    use super::tests_support::*;
    use super::*;

    #[tokio::test]
    async fn skillopt_execute_epoch_rejects_then_accepts_and_persists_candidate_history() {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "coderight-skillopt-execute-{}-{suffix}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&root);
        let store_path = root.join("skillopt-runs.json");

        let app = crate::api::build_router(
            DaemonState::new("").with_skillopt_store_path(store_path.clone()),
        );
        let body = serde_json::json!({
            "name": "tighten-review-skill",
            "target": {
                "skillId": "review",
                "objective": "Reduce missed regression comments",
                "optimizer": "gpt-5"
            },
            "gates": [
                { "name": "held-out review fixture", "kind": "hard", "threshold": 0.9 }
            ]
        });
        let (status, created) =
            json_request(app.clone(), "POST", "/runtime/skillopt/runs", body).await;
        assert_eq!(status, StatusCode::OK);
        let run_id = created["id"].as_str().unwrap();
        let initial_hash = created["bestVersionHash"].as_str().unwrap().to_string();

        let (proposal_status, proposal) = json_request(
            app.clone(),
            "POST",
            &format!("/skills/training-runs/{run_id}/propose-candidate"),
            serde_json::json!({
                "summary": "Restore evidence checks",
                "instructions": "Keep the candidate easy to benchmark."
            }),
        )
        .await;
        assert_eq!(proposal_status, StatusCode::OK);
        assert_eq!(proposal["proposed"], true);
        assert_eq!(proposal["proposal"]["runId"], run_id);
        assert_eq!(proposal["proposal"]["epoch"], 1);
        assert!(proposal["proposal"]["proposedBody"]
            .as_str()
            .unwrap()
            .contains("Restore evidence checks"));
        assert_eq!(
            proposal["run"]["pendingCandidateProposals"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(proposal["run"]["history"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["event"] == "candidateProposed"));

        let (rejected_status, rejected) = json_request(
            app.clone(),
            "POST",
            &format!("/skills/training-runs/{run_id}/execute"),
            serde_json::json!({
                "proposedBody": "# Regression\n\nDrop the evidence requirement.",
                "validationScore": 0.7,
                "operation": "delete",
                "summary": "Remove evidence checks",
                "gateMode": "hard"
            }),
        )
        .await;
        assert_eq!(rejected_status, StatusCode::OK);
        assert_eq!(rejected["executed"], true);
        assert_eq!(rejected["accepted"], false);
        assert_eq!(rejected["candidate"]["accepted"], false);
        assert_eq!(rejected["run"]["status"], "rejected");
        assert_eq!(rejected["run"]["currentEpoch"], 1);
        assert_eq!(rejected["run"]["bestVersionHash"], initial_hash);
        assert_eq!(rejected["run"]["candidates"].as_array().unwrap().len(), 1);
        assert!(rejected["run"]["rejectedEdits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|edit| edit["artifactId"] == rejected["candidate"]["id"]));

        let (accepted_status, accepted) = json_request(
            app.clone(),
            "POST",
            &format!("/runtime/skillopt/runs/{run_id}/execute"),
            serde_json::json!({
                "proposedBody": "# Improved review skill\n\nAlways cite file and line evidence before concluding.",
                "validationScore": 0.95,
                "operation": "replace",
                "summary": "Require file and line evidence",
                "gateMode": "hard",
                "threshold": 0.9,
                "evalEvidence": {
                    "train_pass_rate_pct": 95.0,
                    "heldout_pass_rate_pct": 95.0,
                    "safety_pass_rate_pct": 100.0,
                    "cost_ratio_to_baseline": 1.0,
                    "latency_ratio_to_baseline": 1.0,
                    "pass_rate_pct": 95.0,
                    "regression_count": 0,
                    "new_untested_capability_requested": false,
                    "total_runs": 10,
                    "expected_run_count_min": 10
                }
            }),
        )
        .await;
        assert_eq!(accepted_status, StatusCode::OK);
        assert_eq!(accepted["accepted"], true);
        assert_eq!(accepted["run"]["status"], "accepted");
        assert_eq!(accepted["run"]["currentEpoch"], 2);
        assert_eq!(accepted["run"]["candidates"].as_array().unwrap().len(), 2);
        assert_eq!(
            accepted["run"]["bestVersionHash"],
            accepted["candidate"]["proposedHash"]
        );
        assert_eq!(accepted["run"]["metaSkill"]["version"], 3);
        assert!(accepted["run"]["history"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["event"] == "metaSkillRefined" && entry["epoch"] == 2));
        assert!(accepted["run"]["skillBundle"]["skillBody"]
            .as_str()
            .unwrap()
            .contains("Always cite file and line evidence"));
        assert!(accepted["run"]["history"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["event"] == "rollout" && entry["epoch"] == 2));
        assert!(accepted["run"]["history"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["event"] == "evaluate" && entry["epoch"] == 2));
        assert_eq!(
            accepted["run"]["resumeCheckpoint"]["acceptedCandidateCount"],
            1
        );

        let reloaded = crate::api::build_router(
            DaemonState::new("").with_skillopt_store_path(store_path.clone()),
        );
        let (reload_status, reload_json) = json_request(
            reloaded,
            "GET",
            &format!("/runtime/skillopt/runs/{run_id}"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(reload_status, StatusCode::OK);
        assert_eq!(reload_json["run"]["status"], "accepted");
        assert_eq!(
            reload_json["run"]["candidates"].as_array().unwrap().len(),
            2
        );
        assert_eq!(
            reload_json["run"]["pendingCandidateProposals"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            reload_json["run"]["bestVersionHash"],
            accepted["candidate"]["proposedHash"]
        );
        assert_eq!(
            reload_json["run"]["metaSkill"]["contentHash"],
            accepted["run"]["metaSkill"]["contentHash"]
        );

        let _ = std::fs::remove_dir_all(root);
    }
}
