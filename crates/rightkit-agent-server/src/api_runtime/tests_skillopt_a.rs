#[cfg(test)]
mod tests_skillopt_a {
    use super::tests_support::*;
    use super::*;

    #[tokio::test]
    async fn skillopt_run_route_records_target_rejected_edits_and_gates() {
        let app = crate::api::build_router(DaemonState::new(""));

        let body = serde_json::json!({
            "name": "tighten-review-skill",
            "target": {
                "skillId": "review",
                "objective": "Reduce missed regression comments",
                "optimizer": "skillopt-local",
                "targetModel": "kimi-k2.6"
            },
            "rejectedEdits": [
                {
                    "artifactId": "diff-1",
                    "reason": "Suggested unsafe shell command",
                    "patchPreview": "- rm -rf target"
                }
            ],
            "gates": [
                { "name": "unit route tests", "kind": "hard", "threshold": 1.0 },
                { "name": "review quality", "kind": "soft", "threshold": 0.75 },
                { "name": "mixed rollout", "kind": "mixed", "threshold": 0.85 }
            ]
        });
        let (status, json) =
            json_request(app.clone(), "POST", "/runtime/skillopt/runs", body).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["name"], "tighten-review-skill");
        assert_eq!(json["target"]["skillId"], "review");
        assert_eq!(json["target"]["targetModel"], "kimi-k2.6");
        assert_eq!(
            json["rejectedEdits"][0]["reason"],
            "Suggested unsafe shell command"
        );
        assert_eq!(json["gates"][0]["kind"], "hard");
        assert_eq!(json["status"], "pending");
        assert_eq!(json["currentEpoch"], 0);
        assert_eq!(json["history"][0]["event"], "created");
        assert_eq!(json["resumeCheckpoint"]["status"], "pending");
        assert_eq!(json["metaSkill"]["version"], 1);
        assert!(json["metaSkill"]["contentHash"]
            .as_str()
            .unwrap()
            .starts_with("metaskill:"));
        assert!(json["skillBundle"]["skillBody"]
            .as_str()
            .unwrap()
            .contains("Rejected edit buffer"));

        let (list_status, list) =
            json_request(app, "GET", "/runtime/skillopt/runs", serde_json::json!({})).await;
        assert_eq!(list_status, StatusCode::OK);
        assert_eq!(list["runs"].as_array().unwrap().len(), 1);
        assert_eq!(list["runs"][0]["id"], json["id"]);
    }

    #[test]
    fn a_damaged_skillopt_store_is_set_aside_and_loads_empty() {
        let root =
            std::env::temp_dir().join(format!("coderight-skillopt-damaged-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("skillopt-runs.json");
        std::fs::write(&path, br#"{"runs": [{"id": "run-1""#).unwrap();
        let _store = load_skillopt_store(&path);
        assert!(
            !path.exists(),
            "the partial file is kept aside, not left to be overwritten"
        );
        assert!(std::fs::read_dir(&root).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("skillopt-runs.json.damaged-")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn skillopt_training_runs_persist_resume_cancel_and_alias_routes() {
        let root =
            std::env::temp_dir().join(format!("coderight-skillopt-route-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&root);
        let store_path = root.join("skillopt-runs.json");
        let _ = std::fs::remove_file(&store_path);

        let state = DaemonState::new("").with_skillopt_store_path(store_path.clone());
        let app = crate::api::build_router(state);
        let body = serde_json::json!({
            "name": "tighten-review-skill",
            "target": {
                "skillId": "review",
                "objective": "Reduce missed regression comments",
                "optimizer": "gpt-5",
                "targetModel": "kimi-k2.6"
            },
            "rejectedEdits": [
                {
                    "artifactId": "candidate-1",
                    "reason": "held-out benchmark regressed",
                    "patchPreview": "replace broad rule"
                }
            ],
            "gates": [
                { "name": "unit route tests", "kind": "hard", "threshold": 1.0 }
            ]
        });
        let (status, created) =
            json_request(app.clone(), "POST", "/skills/training-runs", body).await;
        assert_eq!(status, StatusCode::OK);
        assert!(store_path.exists());
        let run_id = created["id"].as_str().unwrap();

        let (resume_status, resumed) = json_request(
            app.clone(),
            "POST",
            &format!("/skills/training-runs/{run_id}/resume"),
            serde_json::json!({
                "message": "resume from validation checkpoint",
                "checkpoint": {
                    "nextCandidate": "skill_v0002.md",
                    "validationScore": 0.76
                }
            }),
        )
        .await;
        assert_eq!(resume_status, StatusCode::OK);
        assert_eq!(resumed["resumed"], true);
        assert_eq!(resumed["run"]["status"], "running");
        assert_eq!(resumed["run"]["currentEpoch"], 1);
        assert_eq!(
            resumed["run"]["resumeCheckpoint"]["nextCandidate"],
            "skill_v0002.md"
        );

        let (history_status, history) = json_request(
            app.clone(),
            "GET",
            &format!("/runtime/skillopt/runs/{run_id}/history"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(history_status, StatusCode::OK);
        assert_eq!(history["history"].as_array().unwrap().len(), 2);
        assert_eq!(history["history"][1]["event"], "resumed");

        let (edits_status, edits) = json_request(
            app.clone(),
            "GET",
            &format!("/skills/training-runs/{run_id}/rejected-edits"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(edits_status, StatusCode::OK);
        assert_eq!(edits["rejectedEdits"][0]["artifactId"], "candidate-1");

        let (cancel_status, cancelled) = json_request(
            app.clone(),
            "POST",
            &format!("/runtime/skillopt/runs/{run_id}/cancel"),
            serde_json::json!({ "reason": "operator stopped stale run" }),
        )
        .await;
        assert_eq!(cancel_status, StatusCode::OK);
        assert_eq!(cancelled["run"]["status"], "cancelled");
        assert_eq!(cancelled["run"]["history"][2]["event"], "cancelled");

        let reloaded = crate::api::build_router(
            DaemonState::new("").with_skillopt_store_path(store_path.clone()),
        );
        let (reload_status, reload_json) = json_request(
            reloaded,
            "GET",
            &format!("/skills/training-runs/{run_id}"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(reload_status, StatusCode::OK);
        assert_eq!(reload_json["run"]["id"], run_id);
        assert_eq!(reload_json["run"]["status"], "cancelled");
        assert_eq!(
            reload_json["run"]["resumeCheckpoint"]["nextCandidate"],
            "skill_v0002.md"
        );
        assert_eq!(reload_json["storePath"], store_path.display().to_string());

        let _ = std::fs::remove_dir_all(root);
    }
}
