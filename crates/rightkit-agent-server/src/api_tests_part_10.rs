#[tokio::test]
async fn plugin_enable_disable_routes_persist_manifest_state() {
    let plugin_dir = unique_temp_dir("plugins-toggle");
    write_test_plugin(&plugin_dir, "fixture", true);

    let app = build_router(DaemonState::new("").with_plugin_dir(plugin_dir.clone()));
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/plugins/fixture/disable")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let manifest_after_disable: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(plugin_dir.join("fixture").join("plugin.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest_after_disable["enabled"], false);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/plugins/fixture/enable")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let manifest_after_enable: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(plugin_dir.join("fixture").join("plugin.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest_after_enable["enabled"], true);

    let _ = std::fs::remove_dir_all(plugin_dir);
}

#[tokio::test]
async fn plugin_install_route_requires_explicit_trust() {
    let plugin_dir = unique_temp_dir("plugins-install-requires-trust");
    let source_dir = unique_temp_dir("plugins-source-requires-trust");
    write_executable_test_plugin(
        &source_dir,
        "trusted-fixture",
        false,
        serde_json::json!(["trusted_tool"]),
        serde_json::json!([]),
    );

    let source_path = source_dir.join("trusted-fixture");
    let app = build_router(DaemonState::new("").with_plugin_dir(plugin_dir.clone()));
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/plugins/install")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "sourcePath": source_path,
                        "trustAccepted": false,
                        "overwrite": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(!plugin_dir.join("trusted-fixture").exists());

    let _ = std::fs::remove_dir_all(plugin_dir);
    let _ = std::fs::remove_dir_all(source_dir);
}

#[tokio::test]
async fn plugin_install_route_copies_trusted_local_source_and_lists_it() {
    let plugin_dir = unique_temp_dir("plugins-install");
    let source_dir = unique_temp_dir("plugins-source");
    write_executable_test_plugin(
        &source_dir,
        "trusted-fixture",
        false,
        serde_json::json!(["trusted_tool"]),
        serde_json::json!(["trusted"]),
    );

    let source_path = source_dir.join("trusted-fixture");
    let app = build_router(DaemonState::new("").with_plugin_dir(plugin_dir.clone()));
    let install_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/plugins/install")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "sourcePath": source_path,
                        "trustAccepted": true,
                        "overwrite": false
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
    assert_eq!(installed["installed"], true);
    assert_eq!(installed["overwritten"], false);
    assert_eq!(installed["plugin"]["id"], "trusted-fixture");
    assert!(plugin_dir
        .join("trusted-fixture")
        .join("plugin.py")
        .exists());

    let list_resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/plugins")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list_resp.status(), StatusCode::OK);
    let body = list_resp.into_body().collect().await.unwrap().to_bytes();
    let listed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(listed["plugins"][0]["id"], "trusted-fixture");
    assert_eq!(listed["plugins"][0]["providesTools"][0], "trusted_tool");

    let _ = std::fs::remove_dir_all(plugin_dir);
    let _ = std::fs::remove_dir_all(source_dir);
}

#[tokio::test]
async fn plugin_marketplace_lists_and_installs_trusted_checksum_entry() {
    let root = unique_temp_dir("plugins-marketplace");
    let plugin_dir = root.join("installed");
    let source_dir = root.join("sources");
    write_executable_test_plugin(
        &source_dir,
        "market-fixture",
        false,
        serde_json::json!(["market_tool"]),
        serde_json::json!(["market"]),
    );
    let source_path = source_dir.join("market-fixture");
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
                    "downloads": 12,
                    "rating": 4.8,
                    "sourceUrl": "file://local-market-fixture",
                    "sourcePath": source_path,
                    "checksumSha256": checksum
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let app = build_router(
        DaemonState::new("")
            .with_plugin_dir(plugin_dir.clone())
            .with_plugin_marketplace_path(marketplace_path.clone()),
    );
    let list_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/plugins/marketplace?query=market")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list_resp.status(), StatusCode::OK);
    let body = list_resp.into_body().collect().await.unwrap().to_bytes();
    let listed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        listed["marketplacePath"].as_str().unwrap(),
        marketplace_path.to_string_lossy()
    );
    assert_eq!(listed["plugins"][0]["id"], "market-fixture");
    assert_eq!(listed["plugins"][0]["checksumSha256"], checksum);

    let untrusted = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/plugins/marketplace/market-fixture/install")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "trustAccepted": false,
                        "expectedSha256": checksum
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
                .uri("/plugins/marketplace/market-fixture/install")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "trustAccepted": true,
                        "expectedSha256": "bad"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(wrong_checksum.status(), StatusCode::BAD_REQUEST);

    let install_resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/plugins/marketplace/market-fixture/install")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "trustAccepted": true,
                        "expectedSha256": checksum
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
    assert_eq!(installed["installed"], true);
    assert_eq!(installed["plugin"]["id"], "market-fixture");
    assert_eq!(installed["checksumSha256"], checksum);
    assert!(plugin_dir.join("market-fixture").join("plugin.py").exists());

    let _ = std::fs::remove_dir_all(root);
}
