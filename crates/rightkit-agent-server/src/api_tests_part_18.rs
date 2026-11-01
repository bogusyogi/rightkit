#[tokio::test]
async fn skill_proposal_store_quarantines_benchmarks_and_gates_activation() {
    let root = unique_temp_dir("skill-proposal-governance");
    let skills_dir = root.join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();

    let app = build_router(DaemonState::new("").with_skills_dir(skills_dir.clone()));
    let created = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/skills/proposals")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r##"{
                          "id":"prop-reviewer",
                          "slug":"reviewer",
                          "title":"Reviewer skill",
                          "summary":"Review code carefully.",
                          "content":"---\nname: reviewer\ndescription: Review code carefully.\nlifecycle: proposed\nenabled: false\nmetadata:\n  coderight:\n    category: tool\n    gate_mode: hard\n    benchmark_ids: [review-fixture]\n---\n\n# Reviewer\n\nInspect diffs before approval.",
                          "sourceSessionId":"s1",
                          "sourceTaskId":"task1"
                        }"##,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let body = created.into_body().collect().await.unwrap().to_bytes();
    let created_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(created_json["proposal"]["status"], "proposed");
    assert!(created_json["proposal"]["contentHash"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));

    let quarantined = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/skills/proposals/prop-reviewer/quarantine")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"reason":"needs fixture"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(quarantined.status(), StatusCode::OK);
    let body = quarantined.into_body().collect().await.unwrap().to_bytes();
    let quarantined_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(quarantined_json["proposal"]["status"], "quarantined");
    assert_eq!(quarantined_json["skill"]["lifecycle"], "quarantined");
    assert_eq!(quarantined_json["skill"]["active"], false);
    assert!(skills_dir.join("reviewer.md").is_file());

    let blocked = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/skills/reviewer/activate")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"reason":"before benchmark"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::PRECONDITION_FAILED);

    let runner_benchmark = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/skills/proposals/prop-reviewer/run-benchmark")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{
                          "kind":"role",
                          "sessionId":"s1",
                          "role":"coder",
                          "goal":"Exercise reviewer skill fixture",
                          "benchmarkId":"review-fixture-runner",
                          "gateMode":"hard",
                          "repeats":1
                        }"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(runner_benchmark.status(), StatusCode::OK);
    let body = runner_benchmark
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    let runner_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(runner_json["ran"], true);
    assert_eq!(runner_json["runnerPassed"], false);
    assert_eq!(
        runner_json["record"]["benchmarkVerdict"]["benchmarkId"],
        "review-fixture-runner"
    );
    assert_eq!(runner_json["record"]["benchmarkVerdict"]["passed"], false);
    assert!(runner_json["record"]["benchmarkVerdict"]["verdictHash"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));

    let benchmarked = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/skills/proposals/prop-reviewer/benchmark")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"benchmarkId":"review-fixture","gateMode":"hard","scoreContinuous":1.0,"passed":true}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
    assert_eq!(benchmarked.status(), StatusCode::OK);
    let body = benchmarked.into_body().collect().await.unwrap().to_bytes();
    let benchmarked_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(benchmarked_json["proposal"]["status"], "benchmarked");
    assert_eq!(benchmarked_json["benchmarkVerdict"]["passed"], true);
    assert!(benchmarked_json["benchmarkVerdict"]["verdictHash"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));

    let activated = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/skills/reviewer/activate")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"reason":"fixture passed"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(activated.status(), StatusCode::OK);
    let body = activated.into_body().collect().await.unwrap().to_bytes();
    let activated_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(activated_json["skill"]["lifecycle"], "active");
    assert_eq!(activated_json["skill"]["active"], true);

    let listed = app
        .oneshot(
            Request::builder()
                .uri("/skills/proposals")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let body = listed.into_body().collect().await.unwrap().to_bytes();
    let listed_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(listed_json["proposals"][0]["status"], "active");
    assert_eq!(listed_json["versions"].as_array().unwrap().len(), 1);
    let verdicts = listed_json["benchmarkVerdicts"].as_array().unwrap();
    assert!(verdicts
        .iter()
        .any(|verdict| verdict["benchmarkId"] == "review-fixture-runner"
            && verdict["passed"] == false));
    assert!(verdicts
        .iter()
        .any(|verdict| verdict["benchmarkId"] == "review-fixture" && verdict["passed"] == true));
    assert!(listed_json["auditLog"].as_array().unwrap().len() >= 4);

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn skill_runtime_context_injects_behavioral_skills_by_priority() {
    let root = unique_temp_dir("skill-runtime-behavioral");
    let skills_dir = root.join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
            skills_dir.join("second.md"),
            "---\nname: second\ndescription: Second behavioral pack.\ncategory: behavioral\npriority: 20\ndisable-model-invocation: true\n---\n\n# Second\n\nApply after baseline.",
        )
        .unwrap();
    std::fs::write(
            skills_dir.join("first.md"),
            "---\nname: first\ndescription: First behavioral pack.\ncategory: behavioral\npriority: 10\n---\n\n# First\n\nThink before coding.",
        )
        .unwrap();
    std::fs::write(
        skills_dir.join("tool-skill.md"),
        "---\nname: tool-skill\ndescription: Tool-invoked skill.\n---\n\n# Tool\n\nUse on request.",
    )
    .unwrap();

    let context = crate::api_skills::skill_runtime_context(
        std::slice::from_ref(&skills_dir),
        &root.to_string_lossy(),
        None,
    )
    .expect("expected skill context");

    assert!(context.contains("Active behavioral skill instructions"));
    assert!(context.contains("## first"));
    assert!(context.contains("Think before coding."));
    assert!(context.contains("## second"));
    assert!(context.contains("Apply after baseline."));
    assert!(
        context.find("## first").unwrap() < context.find("## second").unwrap(),
        "{context}"
    );
    assert!(context.contains("- tool-skill: Tool-invoked skill."));
    assert!(!context.contains("- first: First behavioral pack."));

    let app = build_router(DaemonState::new("").with_skills_dir(skills_dir.clone()));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/skills")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let first = v["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|skill| skill["slug"] == "first")
        .expect("behavioral skill should be in inventory");
    assert_eq!(first["category"], "behavioral");
    assert_eq!(first["priority"], 10);

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn skill_runtime_context_filters_behavioral_skills_for_toolset_defaults() {
    let root = unique_temp_dir("skill-runtime-toolset-behavioral");
    let skills_dir = root.join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
            skills_dir.join("baseline.md"),
            "---\nname: baseline\ndescription: Baseline behavior.\ncategory: behavioral\npriority: 10\n---\n\n# Baseline\n\nUse baseline.",
        )
        .unwrap();
    std::fs::write(
            skills_dir.join("experimental.md"),
            "---\nname: experimental\ndescription: Experimental behavior.\ncategory: behavioral\npriority: 1\n---\n\n# Experimental\n\nDo not load by default.",
        )
        .unwrap();

    let active = vec!["baseline".to_string()];
    let context = crate::api_skills::skill_runtime_context_with_behavioral(
        std::slice::from_ref(&skills_dir),
        &root.to_string_lossy(),
        None,
        Some(&active),
    )
    .expect("expected skill context");

    assert!(context.contains("## baseline"));
    assert!(context.contains("Use baseline."));
    assert!(!context.contains("## experimental"));
    assert!(!context.contains("Do not load by default."));

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn skill_runtime_context_loads_explicitly_activated_tool_skill_body() {
    let root = unique_temp_dir("skill-runtime-explicit-tool");
    let skills_dir = root.join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
            skills_dir.join("reviewer.md"),
            "---\nname: reviewer\ndescription: Review scheduled work.\n---\n\n# Reviewer\n\nInspect scheduled output carefully.",
        )
        .unwrap();

    let active = vec!["reviewer".to_string()];
    let context = crate::api_skills::skill_runtime_context_with_behavioral(
        std::slice::from_ref(&skills_dir),
        &root.to_string_lossy(),
        None,
        Some(&active),
    )
    .expect("expected skill context");

    assert!(context.contains("Auto-loaded skill instructions"));
    assert!(context.contains("Inspect scheduled output carefully."));

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn compose_session_system_prompt_always_injects_coding_defaults() {
    // Every session — even with no instructions/skills/role — gets the
    // always-on minimalism + brevity posture (the inbuilt ponytail+brief hook).
    let prompt = compose_session_system_prompt(None, None, None, None, None, None, None, None)
        .expect("coding defaults make the prompt non-empty even when all inputs are empty");
    assert!(prompt.contains("<coding_defaults>"));
    assert!(prompt.contains("MINIMALISM"));
    assert!(prompt.contains("BREVITY"));
}
