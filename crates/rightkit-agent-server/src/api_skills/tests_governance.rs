#[tokio::test]
async fn skill_governance_routes_reject_lower_trust_sources() {
    let skill_dir = temp_skill_dir("routes");
    let app = crate::api::build_router(DaemonState::new("").with_skills_dir(skill_dir.clone()));

    let cases = [
        (
            Method::POST,
            "/skills/install",
            serde_json::json!({
                "slug": "blocked",
                "content": "Do not install from lower-trust context."
            }),
        ),
        (
            Method::POST,
            "/skills/proposals",
            serde_json::json!({
                "slug": "blocked",
                "summary": "blocked proposal",
                "content": "proposed skill body"
            }),
        ),
        (
            Method::POST,
            "/skills/proposals/proposal-blocked/quarantine",
            serde_json::json!({"reason": "blocked"}),
        ),
        (
            Method::POST,
            "/skills/proposals/proposal-blocked/benchmark",
            serde_json::json!({"benchmarkId": "manual-gate", "passed": true}),
        ),
        (
            Method::POST,
            "/skills/proposals/proposal-blocked/run-benchmark",
            serde_json::json!({
                "kind": "role",
                "sessionId": "session-blocked",
                "role": "reviewer",
                "model": "gpt-5",
                "goal": "exercise skill"
            }),
        ),
        (
            Method::PUT,
            "/skills/blocked",
            serde_json::json!({"content": "updated body"}),
        ),
        (Method::DELETE, "/skills/blocked", serde_json::json!({})),
        (
            Method::POST,
            "/skills/blocked/quarantine",
            serde_json::json!({"reason": "blocked"}),
        ),
        (
            Method::POST,
            "/skills/blocked/activate",
            serde_json::json!({"reason": "blocked", "operatorOverride": true}),
        ),
        (
            Method::POST,
            "/skills/blocked/retire",
            serde_json::json!({"reason": "blocked"}),
        ),
    ];

    for (method, uri, body) in cases {
        let status = lower_trust_skill_request_status(app.clone(), method, uri, body).await;
        assert_eq!(status, axum::http::StatusCode::FORBIDDEN, "{uri}");
    }
    assert!(
        !skill_dir.join("blocked.md").exists(),
        "lower-trust skill install/save must fail before writing"
    );
    assert!(
        !skill_dir.join(".coderight-skill-governance.json").exists(),
        "lower-trust skill proposal routes must fail before writing governance state"
    );

    let _ = std::fs::remove_dir_all(skill_dir);
}

#[tokio::test]
async fn skill_proposal_activation_requires_quarantine_and_passing_benchmark() {
    let skill_dir = temp_skill_dir("activation-gate");
    let app = crate::api::build_router(DaemonState::new("").with_skills_dir(skill_dir.clone()));

    let (status, _) = trusted_skill_request(
        app.clone(),
        Method::POST,
        "/skills/proposals",
        serde_json::json!({
            "id": "proposal-governed-skill",
            "slug": "governed-skill",
            "summary": "A governed test skill",
            "content": "Use governed behavior."
        }),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);

    let (status, _) = trusted_skill_request(
        app.clone(),
        Method::POST,
        "/skills/proposals/proposal-governed-skill/benchmark",
        serde_json::json!({
            "benchmarkId": "premature-benchmark",
            "passed": true
        }),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::PRECONDITION_FAILED);

    let (status, quarantine_body) = trusted_skill_request(
        app.clone(),
        Method::POST,
        "/skills/proposals/proposal-governed-skill/quarantine",
        serde_json::json!({"reason": "reviewed proposal"}),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(quarantine_body["proposal"]["status"], "quarantined");
    assert_eq!(quarantine_body["skill"]["enabled"], false);
    assert_eq!(quarantine_body["skill"]["lifecycle"], "quarantined");

    let (status, _) = trusted_skill_request(
        app.clone(),
        Method::POST,
        "/skills/proposals/proposal-governed-skill/benchmark",
        serde_json::json!({
            "slug": "another-skill",
            "benchmarkId": "wrong-slug",
            "passed": true
        }),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);

    let (status, failed_benchmark) = trusted_skill_request(
        app.clone(),
        Method::POST,
        "/skills/proposals/proposal-governed-skill/benchmark",
        serde_json::json!({
            "benchmarkId": "failing-gate",
            "passed": false,
            "reason": "failed regression gate"
        }),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(failed_benchmark["proposal"]["status"], "quarantined");
    assert_eq!(failed_benchmark["benchmarkVerdict"]["passed"], false);

    let (status, _) = trusted_skill_request(
        app.clone(),
        Method::POST,
        "/skills/governed-skill/activate",
        serde_json::json!({"reason": "try before passing benchmark"}),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::PRECONDITION_FAILED);

    let (status, passed_benchmark) = trusted_skill_request(
        app.clone(),
        Method::POST,
        "/skills/proposals/proposal-governed-skill/benchmark",
        serde_json::json!({
            "benchmarkId": "passing-gate",
            "passed": true,
            "reason": "passed regression gate"
        }),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(passed_benchmark["proposal"]["status"], "benchmarked");
    assert_eq!(passed_benchmark["benchmarkVerdict"]["passed"], true);

    let (status, activation_body) = trusted_skill_request(
        app.clone(),
        Method::POST,
        "/skills/governed-skill/activate",
        serde_json::json!({"reason": "passed benchmark evidence"}),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(activation_body["skill"]["enabled"], true);
    assert_eq!(activation_body["skill"]["lifecycle"], "active");

    let governance_path = skill_dir.join(".coderight-skill-governance.json");
    let store = load_skill_governance_store(&governance_path);
    assert!(store
        .audit_log
        .iter()
        .any(|entry| entry.action == "quarantine"));
    assert!(store
        .audit_log
        .iter()
        .any(|entry| entry.action == "benchmark"));
    assert!(store
        .audit_log
        .iter()
        .any(|entry| entry.action == "activate"));
    assert_eq!(
        store
            .proposals
            .iter()
            .find(|proposal| proposal.id == "proposal-governed-skill")
            .map(|proposal| proposal.status),
        Some(SkillProposalStatus::Active)
    );

    let skill_audit_path = skill_dir.join(".coderight-skill-audit-log.json");
    let audit: Vec<SkillAuditEntry> =
        serde_json::from_str(&std::fs::read_to_string(skill_audit_path).unwrap()).unwrap();
    assert!(audit.iter().any(|entry| entry.action == "activate"
        && entry.lifecycle == SkillLifecycle::Active
        && !entry.operator_override));

    let _ = std::fs::remove_dir_all(skill_dir);
}
