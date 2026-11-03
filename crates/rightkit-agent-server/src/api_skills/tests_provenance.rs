#[tokio::test]
async fn skill_provenance_review_normalizes_github_source_and_flags_review_gates() {
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let app = crate::api::build_router(DaemonState::new(""));
    let content = r#"---
name: external-behavior
description: External behavioral pack
enabled: true
category: behavioral
required_environment_variables: [EXTERNAL_TOKEN]
---

Use careful review behavior.
"#;

    let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/skills/provenance-review")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "sourceUrl": "https://github.com/acme/skills/blob/main/external-behavior/SKILL.md",
                            "content": content
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let review: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(review["ok"], true);
    assert_eq!(review["manifestName"], "external-behavior");
    assert_eq!(review["recommendedLifecycle"], "quarantined");
    assert!(review["contentHash"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    assert_eq!(
        review["normalizedSourceUrl"],
        "https://raw.githubusercontent.com/acme/skills/main/external-behavior/SKILL.md"
    );
    let findings = review["findings"].as_array().unwrap();
    assert!(findings.iter().any(|finding| {
        finding["code"] == "external-pack-should-quarantine" && finding["severity"] == "warning"
    }));
    assert!(findings
        .iter()
        .any(|finding| finding["code"] == "missing-benchmark-ids"));
    assert!(findings
        .iter()
        .any(|finding| finding["code"] == "requires-environment"));
    assert!(findings
        .iter()
        .any(|finding| finding["code"] == "behavioral-skill"));
    assert!(!serde_json::to_string(&review).unwrap().contains("secret"));
}
