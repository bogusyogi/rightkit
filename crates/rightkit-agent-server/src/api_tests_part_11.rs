#[tokio::test]
async fn plugin_marketplace_sync_requires_trust_and_catalog_checksum() {
    let root = unique_temp_dir("plugins-marketplace-sync");
    let plugin_dir = root.join("installed");
    let source_dir = root.join("sources");
    write_executable_test_plugin(
        &source_dir,
        "sync-fixture",
        false,
        serde_json::json!(["sync_tool"]),
        serde_json::json!(["sync"]),
    );
    let source_path = source_dir.join("sync-fixture");
    let plugin_checksum = plugin_source_sha256(&source_path).unwrap();
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(source_path.join("plugin.json")).unwrap())
            .unwrap();
    let remote_catalog = serde_json::to_vec_pretty(&serde_json::json!({
        "signedBy": "catalog-test",
        "plugins": [
            {
                "manifest": manifest,
                "downloads": 22,
                "rating": 4.6,
                "sourceUrl": "file://local-sync-fixture",
                "sourcePath": source_path,
                "checksumSha256": plugin_checksum,
                "signedBy": "catalog-test"
            }
        ]
    }))
    .unwrap();
    let catalog_checksum = rightkit_fs::digest::sha256_bytes(&remote_catalog);
    let remote_catalog_path = root.join("remote-marketplace.json");
    std::fs::write(&remote_catalog_path, &remote_catalog).unwrap();
    let marketplace_path = root.join("local-marketplace.json");
    let catalog_url = format!(
        "file://{}",
        remote_catalog_path.to_string_lossy().replace('\\', "/")
    );

    let app = build_router(
        DaemonState::new("")
            .with_plugin_dir(plugin_dir)
            .with_plugin_marketplace_path(marketplace_path.clone()),
    );

    let untrusted = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/plugins/marketplace/sync")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "catalogUrl": catalog_url,
                        "expectedSha256": catalog_checksum,
                        "trustAccepted": false,
                        "allowFileUrls": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(untrusted.status(), StatusCode::BAD_REQUEST);

    let wrong_checksum = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/plugins/marketplace/sync")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "catalogUrl": catalog_url,
                        "expectedSha256": "bad",
                        "trustAccepted": true,
                        "allowFileUrls": true,
                        "fileUrlTrustAccepted": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(wrong_checksum.status(), StatusCode::BAD_REQUEST);

    let synced = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/plugins/marketplace/sync")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "catalogUrl": catalog_url,
                        "expectedSha256": catalog_checksum,
                        "trustAccepted": true,
                        "allowFileUrls": true,
                        "fileUrlTrustAccepted": true,
                        "requireSigned": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(synced.status(), StatusCode::OK);
    let body = synced.into_body().collect().await.unwrap().to_bytes();
    let synced: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(synced["synced"], true);
    assert_eq!(synced["checksumSha256"], catalog_checksum);
    assert_eq!(synced["pluginCount"], 1);

    let list_resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/plugins/marketplace?query=sync")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list_resp.status(), StatusCode::OK);
    let body = list_resp.into_body().collect().await.unwrap().to_bytes();
    let listed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(listed["plugins"][0]["id"], "sync-fixture");
    assert_eq!(listed["plugins"][0]["checksumSha256"], plugin_checksum);

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn plugin_marketplace_installs_checksum_pinned_package_source() {
    let root = unique_temp_dir("plugins-marketplace-package");
    let plugin_dir = root.join("installed");
    let source_dir = root.join("sources");
    write_executable_test_plugin(
        &source_dir,
        "package-fixture",
        false,
        serde_json::json!(["package_tool"]),
        serde_json::json!(["package"]),
    );
    let source_path = source_dir.join("package-fixture");
    let source_checksum = plugin_source_sha256(&source_path).unwrap();
    let package_path = root.join("packages").join("package-fixture.tar.gz");
    write_plugin_package(&source_path, &package_path);
    let package_bytes = std::fs::read(&package_path).unwrap();
    let package_checksum = rightkit_fs::digest::sha256_bytes(&package_bytes);
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(source_path.join("plugin.json")).unwrap())
            .unwrap();
    let marketplace_path = root.join("marketplace.json");
    std::fs::write(
            &marketplace_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "plugins": [
                    {
                        "manifest": manifest,
                        "downloads": 8,
                        "rating": 4.4,
                        "sourceUrl": format!("file://{}", package_path.to_string_lossy().replace('\\', "/")),
                        "checksumSha256": source_checksum,
                        "packageSha256": package_checksum
                    }
                ]
            }))
            .unwrap(),
        )
        .unwrap();

    let app = build_router(
        DaemonState::new("")
            .with_plugin_dir(plugin_dir.clone())
            .with_plugin_marketplace_path(marketplace_path),
    );

    let file_url_not_acknowledged = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/plugins/marketplace/package-fixture/install")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "trustAccepted": true,
                        "expectedSha256": package_checksum,
                        "allowFileUrls": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(file_url_not_acknowledged.status(), StatusCode::BAD_REQUEST);

    let install_resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/plugins/marketplace/package-fixture/install")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "trustAccepted": true,
                        "expectedSha256": package_checksum,
                        "allowFileUrls": true,
                        "fileUrlTrustAccepted": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(install_resp.status(), StatusCode::OK);
    let body = install_resp.into_body().collect().await.unwrap().to_bytes();
    let installed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(installed["plugin"]["id"], "package-fixture");
    assert_eq!(installed["checksumSha256"], source_checksum);
    assert_eq!(installed["packageSha256"], package_checksum);
    assert!(plugin_dir
        .join("package-fixture")
        .join("plugin.py")
        .exists());

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn plugin_marketplace_slash_command_reports_catalog_entries() {
    let root = unique_temp_dir("plugins-marketplace-slash");
    let plugin_dir = root.join("installed");
    let source_dir = root.join("sources");
    write_executable_test_plugin(
        &source_dir,
        "market-slash",
        false,
        serde_json::json!(["market_tool"]),
        serde_json::json!(["market"]),
    );
    let source_path = source_dir.join("market-slash");
    let checksum = plugin_source_sha256(&source_path).unwrap();
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(source_path.join("plugin.json")).unwrap())
            .unwrap();
    let marketplace_path = root.join("marketplace.json");
    std::fs::write(
        &marketplace_path,
        serde_json::to_string_pretty(&serde_json::json!({
            "plugins": [
                {
                    "manifest": manifest,
                    "downloads": 5,
                    "rating": 4.2,
                    "sourceUrl": "file://market-slash",
                    "sourcePath": source_path,
                    "checksumSha256": checksum,
                    "signedBy": "local-test"
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let app = build_router(
        DaemonState::new("")
            .with_plugin_dir(plugin_dir)
            .with_plugin_marketplace_path(marketplace_path),
    );
    let result = execute_slash_command(app, "/plugin marketplace slash").await;
    assert_eq!(result["success"], true);
    let output = result["output"].as_str().unwrap();
    assert!(output.contains("market-slash"));
    assert!(output.contains(&checksum));
    assert!(output.contains("expectedSha256"));

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn plugin_commands_are_listed_and_execute_via_entry() {
    let plugin_dir = unique_temp_dir("plugins-command");
    write_executable_test_plugin(
        &plugin_dir,
        "fixture",
        true,
        serde_json::json!([]),
        serde_json::json!(["fixture"]),
    );

    let app = build_router(DaemonState::new("").with_plugin_dir(plugin_dir.clone()));
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/commands")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let commands: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert!(commands.iter().any(|c| c["name"] == "fixture"));

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/commands/execute")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"input":"/fixture hello"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let result: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(result["success"], true);
    assert_eq!(result["output"], "command:fixture");

    let _ = std::fs::remove_dir_all(plugin_dir);
}
