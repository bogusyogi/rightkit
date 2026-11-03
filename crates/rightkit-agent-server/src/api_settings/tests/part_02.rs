#[tokio::test]
async fn settings_toolsets_save_list_and_delete_workspace_profile() {
    let _guard = env_lock();
    let prior_home = std::env::var_os("RIGHTKIT_SUITE_ROOT");
    let home = temp_path("home");
    let workspace = temp_path("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    unsafe {
        rightkit_app_settings::agent_config::test_isolation::set_process_suite_root(&home);
    }

    let app = crate::api::build_router(DaemonState::new(""));
    let workspace_query = query_encode(&workspace.display().to_string());
    let save_uri = format!("/settings/toolsets/remote-safe?workspaceRoot={workspace_query}");
    let save_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(save_uri)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": "Remote safe",
                        "description": "Remote profile",
                        "allowedTools": ["read_file", "search"],
                        "disallowedTools": ["shell"],
                        "readOnly": true,
                        "defaultBehavioralSkills": ["cr-baseline-behavior"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(save_resp.status(), StatusCode::OK);

    let list_uri = format!("/settings/toolsets?workspaceRoot={workspace_query}");
    let list_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(list_uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list_resp.status(), StatusCode::OK);
    let body = list_resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["configured"][0]["id"], "remote-safe");
    assert_eq!(json["configured"][0]["allowedTools"][0], "read_file");
    assert_eq!(json["configured"][0]["readOnly"], true);

    let session_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "id": "s-settings-toolset",
                        "workspaceRoot": workspace.display().to_string(),
                        "toolset": "remote-safe",
                        "allowedTools": ["read_file", "shell"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(session_resp.status(), StatusCode::OK);
    let body = session_resp.into_body().collect().await.unwrap().to_bytes();
    let session: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(session["toolset"], "remote-safe");
    assert_eq!(session["allowedTools"], serde_json::json!(["read_file"]));

    let delete_uri = format!("/settings/toolsets/remote-safe?workspaceRoot={workspace_query}");
    let delete_resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(delete_uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete_resp.status(), StatusCode::OK);
    let config = std::fs::read_to_string(workspace.join(".coderight").join("config.toml"))
        .expect("workspace config");
    assert!(!config.contains("[toolsets.remote-safe]"));

    unsafe {
        match prior_home {
            Some(value) => std::env::set_var("RIGHTKIT_SUITE_ROOT", value),
            None => std::env::remove_var("RIGHTKIT_SUITE_ROOT"),
        }
    }
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&workspace);
}
