#[tokio::test]
async fn slash_commands_report_real_permissions_and_skills() {
    let root = unique_temp_dir("slash-permissions-skills");
    let skills_dir = root.join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
        skills_dir.join("reviewer.md"),
        "# Reviewer\n\nReview changes.",
    )
    .unwrap();

    let state = DaemonState::new("");
    state.create_session("s1", None, "/tmp", "Primary", None);
    let app = build_router(state.clone());

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/s1/permission-mode")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"mode":"auto-safe"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/s1/permission-rules")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"pattern":"Bash(git push*)","effect":"deny"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let permissions = execute_slash_command(app.clone(), "/permissions s1").await;
    assert_eq!(permissions["success"], true);
    let permissions_output = permissions["output"].as_str().unwrap();
    assert!(
        permissions_output.contains("AutoSafe"),
        "{permissions_output}"
    );
    assert!(
        permissions_output.contains("Bash(git push*)"),
        "{permissions_output}"
    );
    assert!(permissions_output.contains("deny"), "{permissions_output}");
    assert!(
        permissions_output.contains("Rule examples:"),
        "{permissions_output}"
    );
    assert!(
        permissions_output.contains("read_file(**/.env*)"),
        "{permissions_output}"
    );

    let active_permissions =
        execute_slash_command_for_session(app.clone(), "/permissions", "s1").await;
    assert_eq!(active_permissions["success"], true);
    let active_permissions_output = active_permissions["output"].as_str().unwrap();
    assert!(
        active_permissions_output.contains("**Permissions:** s1"),
        "{active_permissions_output}"
    );
    assert!(
        active_permissions_output.contains("AutoSafe"),
        "{active_permissions_output}"
    );
    assert!(
        active_permissions_output.contains("Bash(git push*)"),
        "{active_permissions_output}"
    );

    let skills_app = build_router(DaemonState::new("").with_skills_dir(skills_dir.clone()));
    let skills = execute_slash_command(skills_app, "/skills").await;
    assert_eq!(skills["success"], true);
    let skills_output = skills["output"].as_str().unwrap();
    assert!(skills_output.contains("reviewer"));
    assert!(skills_output.contains(&skills_dir.display().to_string()));

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn skills_routes_list_read_and_install_managed_skills() {
    let root = unique_temp_dir("skills-managed-routes");
    let skills_dir = root.join("skills");
    let reviewer_dir = skills_dir.join("reviewer");
    std::fs::create_dir_all(&reviewer_dir).unwrap();
    std::fs::write(
            reviewer_dir.join("SKILL.md"),
            "---\nname: reviewer\ndescription: Review code changes for correctness.\nexecutionMode: review\nallowedTools: [\"read_file\", \"git_diff\"]\ndisallowedTools: [\"write_file\"]\neffectPrompt: Inspect first.\n---\n\n# Reviewer\n\nReview the diff.",
        )
        .unwrap();
    let state = DaemonState::new("").with_skills_dir(skills_dir.clone());
    state.create_session("s1", None, &root.to_string_lossy(), "Skill benchmark", None);
    let app = build_router(state);

    let resp = app
        .clone()
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
    assert_eq!(v["skillDirs"][0], skills_dir.to_string_lossy().to_string());
    assert_eq!(v["skills"][0]["slug"], "reviewer");
    assert_eq!(
        v["skills"][0]["description"],
        "Review code changes for correctness."
    );
    assert_eq!(v["skills"][0]["kind"], "directory");
    assert_eq!(v["skills"][0]["category"], "tool");
    assert_eq!(v["skills"][0]["priority"], 100);
    assert_eq!(v["skills"][0]["enabled"], true);
    assert_eq!(v["skills"][0]["disableModelInvocation"], false);
    assert_eq!(v["skills"][0]["effects"]["executionMode"], "review");
    assert_eq!(v["skills"][0]["effects"]["allowedTools"][0], "read_file");
    assert_eq!(
        v["skills"][0]["effects"]["disallowedTools"][0],
        "write_file"
    );
    assert_eq!(v["skills"][0]["effects"]["prompt"], "Inspect first.");

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/skills/reviewer")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v["content"].as_str().unwrap().contains("# Reviewer"));
    assert_eq!(v["skill"]["slug"], "reviewer");
    assert_eq!(v["skill"]["effects"]["executionMode"], "review");

    let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/skills/install")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r##"{"slug":"security-check","description":"Check security-sensitive code paths.","trigger":"event == \"PreToolUse\"","content":"# Security\n\nCheck auth and filesystem access."}"##,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["installed"], true);
    assert_eq!(v["skill"]["slug"], "security-check");
    assert_eq!(v["skill"]["trigger"], "event == \"PreToolUse\"");
    assert!(skills_dir.join("security-check.md").is_file());

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/skills/security-check")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        v["skill"]["description"],
        "Check security-sensitive code paths."
    );
    assert!(v["content"]
        .as_str()
        .unwrap()
        .contains("Check auth and filesystem access."));

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn skills_routes_update_toggle_and_delete_managed_skill() {
    let root = unique_temp_dir("skills-edit-routes");
    let skills_dir = root.join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    let skill_path = skills_dir.join("reviewer.md");
    std::fs::write(
            &skill_path,
            "+++\nname = \"reviewer\"\ndescription = \"Review code.\"\nenabled = true\ntags = []\n+++\n\n# Reviewer\n\nReview the diff.",
        )
        .unwrap();
    let app = build_router(DaemonState::new("").with_skills_dir(skills_dir.clone()));

    let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/skills/reviewer")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "content": "+++\nname = \"reviewer\"\ndescription = \"Review code and tests.\"\nenabled = true\ntags = [\"review\"]\n+++\n\n# Reviewer\n\nCheck tests too.",
                            "enabled": false
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["skill"]["slug"], "reviewer");
    assert_eq!(v["skill"]["enabled"], false);
    assert_eq!(v["skill"]["description"], "Review code and tests.");
    let saved = std::fs::read_to_string(&skill_path).unwrap();
    assert!(saved.contains("enabled = false"));
    assert!(saved.contains("Check tests too."));

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/skills/reviewer")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["skill"]["enabled"], false);

    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/skills/reviewer")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(!skill_path.exists());

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn lsp_routes_save_and_report_workspace_config() {
    let root = unique_temp_dir("lsp-routes");
    std::fs::create_dir_all(&root).unwrap();
    let app = build_router(DaemonState::new(""));

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/lsp")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "workspaceRoot": root,
                        "servers": [{
                            "language": "rust",
                            "extensions": ["rs"],
                            "serverPath": "custom-rust-analyzer",
                            "serverArgs": ["--stdio"]
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["servers"][0]["language"], "rust");
    assert_eq!(v["servers"][0]["source"], "workspace-config");
    assert_eq!(v["servers"][0]["configuredPath"], "custom-rust-analyzer");
    assert!(v["servers"][0]["effectivePath"].is_null());

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/lsp?workspaceRoot={}",
                    root.to_string_lossy().replace('\\', "/")
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let expected_config_path = root
        .join(".coderight")
        .join("lsp.json")
        .to_string_lossy()
        .replace('\\', "/")
        .to_string();
    assert_eq!(
        v["configPath"].as_str().map(|path| path.replace('\\', "/")),
        Some(expected_config_path)
    );
    assert_eq!(v["servers"][0]["serverArgs"][0], "--stdio");

    let _ = std::fs::remove_dir_all(root);
}
