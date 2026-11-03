#[tokio::test]
async fn skills_install_rejects_path_traversal_slug() {
    let root = unique_temp_dir("skills-install-traversal");
    let skills_dir = root.join("skills");
    let app = build_router(DaemonState::new("").with_skills_dir(skills_dir.clone()));

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/skills/install")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"slug":"../escape","description":"bad","content":"body"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(!root.join("escape.md").exists());
    assert!(!skills_dir.join("..").exists());

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn skill_runtime_context_lists_enabled_skills_without_eager_body_load() {
    let root = unique_temp_dir("skill-runtime-catalog");
    let skills_dir = root.join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
            skills_dir.join("reviewer.md"),
            "---\nname: reviewer\ndescription: Review code changes.\nexecutionMode: review\nallowedTools: [\"read_file\", \"git_diff\"]\n---\n\n# Reviewer\n\nReview the diff carefully.",
        )
        .unwrap();

    let context = crate::api_skills::skill_runtime_context(
        std::slice::from_ref(&skills_dir),
        &root.to_string_lossy(),
        None,
    )
    .expect("expected skill context");

    assert!(context.contains("Enabled local skills"));
    assert!(context.contains("- reviewer: Review code changes."));
    assert!(context.contains("executionMode=review"));
    assert!(context.contains("allowedTools=read_file,git_diff"));
    assert!(context.contains("invoke_skill"));
    assert!(!context.contains("Review the diff carefully."));

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn skill_runtime_context_omits_model_invocation_disabled_skills() {
    let root = unique_temp_dir("skill-runtime-no-model-invoke");
    let skills_dir = root.join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
            skills_dir.join("deploy.md"),
            "---\nname: deploy\ndescription: Deploy production.\ndisable-model-invocation: true\n---\n\n# Deploy\n\nRun deployment.",
        )
        .unwrap();
    std::fs::write(
        skills_dir.join("reviewer.md"),
        "---\nname: reviewer\ndescription: Review code changes.\n---\n\n# Reviewer\n\nReview code.",
    )
    .unwrap();

    let context = crate::api_skills::skill_runtime_context(
        std::slice::from_ref(&skills_dir),
        &root.to_string_lossy(),
        None,
    )
    .expect("expected skill context");

    assert!(context.contains("reviewer"));
    assert!(!context.contains("deploy"));

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
    let deploy = v["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|skill| skill["slug"] == "deploy")
        .expect("deploy skill should remain visible in management inventory");
    assert_eq!(deploy["disableModelInvocation"], true);

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn skill_runtime_context_autoloads_session_start_skill_body() {
    let root = unique_temp_dir("skill-runtime-session-start");
    let skills_dir = root.join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
            skills_dir.join("security.md"),
            "+++\nname = \"security\"\ndescription = \"Review security-sensitive changes.\"\ntrigger = \"event == \\\"SessionStart\\\" and workspace_root contains \\\"skill-runtime-session-start\\\"\"\nenabled = true\n+++\n\n# Security\n\nCheck auth, filesystem access, and secrets.",
        )
        .unwrap();
    std::fs::write(
            skills_dir.join("disabled.md"),
            "+++\nname = \"disabled\"\ndescription = \"Should not load.\"\ntrigger = \"event == \\\"SessionStart\\\"\"\nenabled = false\n+++\n\n# Disabled",
        )
        .unwrap();

    let context = crate::api_skills::skill_runtime_context(
        std::slice::from_ref(&skills_dir),
        &root.to_string_lossy(),
        None,
    )
    .expect("expected skill context");

    assert!(context.contains("Auto-loaded skill instructions"));
    assert!(context.contains("## security"));
    assert!(context.contains("Check auth, filesystem access, and secrets."));
    assert!(!context.contains("Should not load."));

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn quarantined_skill_stays_visible_but_never_autoloads_or_leaks_env_metadata() {
    let root = unique_temp_dir("skill-runtime-quarantine");
    let skills_dir = root.join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
            skills_dir.join("quarantined.md"),
            "---\nname: quarantined\ndescription: Proposed skill under review.\nlifecycle: quarantined\nrequired_environment_variables: [SECRET_SKILL_TOKEN]\nmetadata:\n  coderight:\n    category: behavioral\n    gate_mode: mixed\n    benchmark_ids: [behavior-fixture]\n---\n\n# Quarantined\n\nDo not expose until approved.",
        )
        .unwrap();
    std::fs::write(
            skills_dir.join("active.md"),
            "+++\nname = \"active\"\ndescription = \"Active session-start skill.\"\ntrigger = \"event == \\\"SessionStart\\\"\"\nenabled = true\n+++\n\n# Active\n\nLoad this.",
        )
        .unwrap();

    let context = crate::api_skills::skill_runtime_context(
        std::slice::from_ref(&skills_dir),
        &root.to_string_lossy(),
        None,
    )
    .expect("expected active skill context");

    assert!(context.contains("## active"));
    assert!(!context.contains("## quarantined"));
    assert!(!context.contains("Do not expose until approved."));
    assert!(!context.contains("SECRET_SKILL_TOKEN"));

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
    let quarantined = v["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|skill| skill["slug"] == "quarantined")
        .expect("quarantined skill should remain visible for governance UI");
    assert_eq!(quarantined["lifecycle"], "quarantined");
    assert_eq!(quarantined["active"], false);
    assert_eq!(
        quarantined["requiredEnvironmentVariables"],
        serde_json::json!(["SECRET_SKILL_TOKEN"])
    );
    assert_eq!(
        quarantined["metadata"]["coderight"]["benchmarkIds"],
        serde_json::json!(["behavior-fixture"])
    );

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn skill_activation_requires_benchmark_or_operator_override_and_writes_audit_log() {
    let root = unique_temp_dir("skill-runtime-activation-gate");
    let skills_dir = root.join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
            skills_dir.join("candidate.md"),
            "---\nname: candidate\ndescription: Candidate skill.\nlifecycle: quarantined\nenabled: false\n---\n\n# Candidate\n\nUse carefully.",
        )
        .unwrap();

    let app = build_router(DaemonState::new("").with_skills_dir(skills_dir.clone()));
    let blocked = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/skills/candidate/activate")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"reason":"no benchmark yet"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::PRECONDITION_FAILED);

    let activated = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/skills/candidate/activate")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"reason":"operator accepted local risk","operatorOverride":true}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(activated.status(), StatusCode::OK);
    let body = activated.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["skill"]["lifecycle"], "active");
    assert_eq!(v["skill"]["enabled"], true);
    assert_eq!(v["skill"]["active"], true);

    let audit_path = skills_dir.join(".coderight-skill-audit-log.json");
    let audit: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(audit_path).unwrap()).unwrap();
    assert_eq!(audit[0]["slug"], "candidate");
    assert_eq!(audit[0]["action"], "activate");
    assert_eq!(audit[0]["operatorOverride"], true);

    let _ = std::fs::remove_dir_all(root);
}
