#[tokio::test]
async fn constitution_decision_blocks_implement_until_evidence_backed_override() {
    let root = std::env::temp_dir().join(format!(
        "coderight-spec-constitution-{}-{}",
        std::process::id(),
        now_millis()
    ));
    let constitution_dir = root.join(".coderight");
    std::fs::create_dir_all(&constitution_dir).unwrap();
    let constitution_path = constitution_dir.join("constitution.md");
    std::fs::write(
        &constitution_path,
        "All user-facing changes require verification evidence.",
    )
    .unwrap();
    let workspace = root.to_string_lossy().to_string();
    let app = crate::api::build_router(DaemonState::new(""));
    let create_body = serde_json::json!({
        "workspaceRoot": workspace,
        "title": "Auth",
        "description": "Add login support",
        "functionalRequirements": [{"id":"FR-001","text":"Users can log in"}],
        "successCriteria": [{"id":"SC-001","text":"Login test passes"}]
    });
    let create_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/specs")
                .header("content-type", "application/json")
                .body(Body::from(create_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create_resp.status(), StatusCode::CREATED);
    let body = create_resp.into_body().collect().await.unwrap().to_bytes();
    let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let spec_id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["constitutionSources"].as_array().unwrap().len(), 1);

    let plan_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/specs/{spec_id}/plan"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"includeVerification":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(plan_resp.status(), StatusCode::OK);

    let violation_body = serde_json::json!({
        "id": "cd-login-tests",
        "rule": "All user-facing changes require verification evidence.",
        "verdict": "violation",
        "rationale": "Operator review says required verification is missing.",
        "evidenceIds": []
    });
    let violation_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/specs/{spec_id}/constitution/decisions"))
                .header("content-type", "application/json")
                .body(Body::from(violation_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(violation_resp.status(), StatusCode::CREATED);

    let blocked_impl = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/specs/{spec_id}/implement"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"dryRun":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(blocked_impl.status(), StatusCode::CONFLICT);

    let override_body = serde_json::json!({
        "id": "cd-login-tests",
        "rule": "All user-facing changes require verification evidence.",
        "verdict": "overridden",
        "rationale": "Risk accepted after reviewing EV-SC-001.",
        "evidenceIds": ["EV-SC-001"]
    });
    let override_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/specs/{spec_id}/constitution/decisions"))
                .header("content-type", "application/json")
                .body(Body::from(override_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(override_resp.status(), StatusCode::CREATED);

    let list_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/specs/{spec_id}/constitution/decisions"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list_resp.status(), StatusCode::OK);
    let body = list_resp.into_body().collect().await.unwrap().to_bytes();
    let decisions: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(decisions.as_array().unwrap().len(), 1);
    assert_eq!(decisions[0]["verdict"], "overridden");
    assert_eq!(decisions[0]["evidenceIds"][0], "EV-SC-001");

    let trace_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/specs/{spec_id}/trace"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(trace_resp.status(), StatusCode::OK);
    let body = trace_resp.into_body().collect().await.unwrap().to_bytes();
    let trace: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        trace["evidence"].as_array().unwrap().iter().any(|row| {
            row["id"] == "constitution:cd-login-tests"
                && row["kind"] == "constitution_decision"
                && row["sessionId"] == format!("spec:{spec_id}")
                && row["taskId"] == "T002"
                && row["evidenceId"] == "EV-SC-001"
                && row["producer"] == "constitution_review"
                && row["provenance"] == "spec-constitution-decision"
                && row["verdictPassed"] == true
                && row["shippable"] == true
        }),
        "trace should include hash-bound constitution decision evidence"
    );

    let allowed_impl = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/specs/{spec_id}/implement"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"dryRun":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(allowed_impl.status(), StatusCode::OK);
    let body = allowed_impl.into_body().collect().await.unwrap().to_bytes();
    let implement: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(implement["dryRun"], true);
    assert_eq!(implement["analysis"]["status"], "pass");

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn analyzer_route_flags_requirement_without_task() {
    let state = DaemonState::new("");
    let app = crate::api::build_router(state);
    let create_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/specs")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"workspaceRoot":"D:/work","title":"Auth","description":"Add login support","functionalRequirements":[{"id":"FR-001","text":"Users can log in"}],"successCriteria":[{"id":"SC-001","text":"Login test passes"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
    let body = create_resp.into_body().collect().await.unwrap().to_bytes();
    let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let spec_id = created["id"].as_str().unwrap();

    let analyze_resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/specs/{spec_id}/analyze"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(analyze_resp.status(), StatusCode::OK);
    let body = analyze_resp.into_body().collect().await.unwrap().to_bytes();
    let run: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(run["status"], "block");
    assert!(run["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|finding| { finding["kind"] == "requirement_without_task" }));
}

#[tokio::test]
async fn specs_route_persists_specs_tasks_and_analysis() {
    let root = std::env::temp_dir().join(format!(
        "coderight-spec-store-{}-{}",
        std::process::id(),
        now_millis()
    ));
    let store_path = root.join("feature-specs.json");
    let state = DaemonState::new("").with_spec_store_path(store_path.clone());
    let app = crate::api::build_router(state);
    let create_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/specs")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"workspaceRoot":"D:/work","title":"Auth","description":"Add login support","functionalRequirements":[{"id":"FR-001","text":"Users can log in"}],"successCriteria":[{"id":"SC-001","text":"Login test passes"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
    assert_eq!(create_resp.status(), StatusCode::CREATED);
    let body = create_resp.into_body().collect().await.unwrap().to_bytes();
    let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let spec_id = created["id"].as_str().unwrap().to_string();

    let add_tasks_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/specs/{spec_id}/tasks"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"tasks":[{"requirementIds":["FR-001"],"successCriterionIds":["SC-001"],"description":"Implement login","evidenceIds":["test:login"]}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
    assert_eq!(add_tasks_resp.status(), StatusCode::OK);

    let analyze_resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/specs/{spec_id}/analyze"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(analyze_resp.status(), StatusCode::OK);
    assert!(store_path.exists());

    let reloaded =
        crate::api::build_router(DaemonState::new("").with_spec_store_path(store_path.clone()));
    let get_resp = reloaded
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/specs/{spec_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get_resp.status(), StatusCode::OK);
    let body = get_resp.into_body().collect().await.unwrap().to_bytes();
    let envelope: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(envelope["spec"]["id"].as_str(), Some(spec_id.as_str()));
    assert_eq!(envelope["tasks"][0]["requirementIds"][0], "FR-001");

    let trace_resp = reloaded
        .oneshot(
            Request::builder()
                .uri(format!("/specs/{spec_id}/trace"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(trace_resp.status(), StatusCode::OK);
    let body = trace_resp.into_body().collect().await.unwrap().to_bytes();
    let trace: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        trace["latestAnalysis"]["specId"].as_str(),
        Some(spec_id.as_str())
    );

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn patch_spec_rejects_done_when_analysis_blocks_completion() {
    let app = crate::api::build_router(DaemonState::new(""));
    let create_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/specs")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"workspaceRoot":"D:/work","title":"Auth","description":"Add login support","functionalRequirements":[{"id":"FR-001","text":"Users can log in"}],"successCriteria":[{"id":"SC-001","text":"Login test passes"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
    assert_eq!(create_resp.status(), StatusCode::CREATED);
    let body = create_resp.into_body().collect().await.unwrap().to_bytes();
    let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let spec_id = created["id"].as_str().unwrap();

    let patch_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/specs/{spec_id}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"status":"done"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(patch_resp.status(), StatusCode::CONFLICT);
    let body = patch_resp.into_body().collect().await.unwrap().to_bytes();
    let error: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(error["error"]
        .as_str()
        .unwrap()
        .contains("spec cannot be marked done"));

    let get_resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/specs/{spec_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get_resp.status(), StatusCode::OK);
    let body = get_resp.into_body().collect().await.unwrap().to_bytes();
    let envelope: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(envelope["spec"]["status"], "draft");
}
