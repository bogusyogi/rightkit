#[test]
fn skills_openapi_components_cover_skill_governance_shapes() {
    let components = skills_openapi_components();
    let expected = [
        "SkillItem",
        "SkillLifecycle",
        "SkillGateMode",
        "SkillListResponse",
        "InstallSkillRequest",
        "InstallSkillResponse",
        "SkillProvenanceReviewRequest",
        "SkillProvenanceFinding",
        "SkillProvenanceReviewResponse",
        "SkillProposalRecord",
        "SkillVersionRecord",
        "SkillBenchmarkVerdict",
        "SkillGovernanceAuditEntry",
        "SkillGovernanceResponse",
        "CreateSkillProposalRequest",
        "CreateSkillProposalResponse",
        "SkillProposalQuarantineRequest",
        "SkillProposalQuarantineResponse",
        "SkillBenchmarkRequest",
        "SkillBenchmarkResponse",
        "SkillProposalBenchmarkRunRequest",
        "SkillProposalBenchmarkRunResponse",
        "SkillLifecycleRequest",
        "SkillLifecycleResponse",
    ];

    for name in expected {
        assert!(components.contains_key(name), "missing component {name}");
    }

    let helper_refs = [
        skill_list_response_schema(),
        install_skill_request_schema(),
        install_skill_response_schema(),
        skill_provenance_review_request_schema(),
        skill_provenance_review_response_schema(),
        skill_governance_response_schema(),
        create_skill_proposal_request_schema(),
        create_skill_proposal_response_schema(),
        skill_proposal_quarantine_request_schema(),
        skill_proposal_quarantine_response_schema(),
        skill_benchmark_request_schema(),
        skill_benchmark_response_schema(),
        skill_proposal_benchmark_run_request_schema(),
        skill_proposal_benchmark_run_response_schema(),
        skill_lifecycle_request_schema(),
        skill_lifecycle_response_schema(),
    ];

    for schema in helper_refs {
        assert!(
            components.contains_key(ref_target_name(&schema)),
            "helper ref does not point at a skills component: {schema:?}"
        );
    }

    assert_eq!(
        components["SkillListResponse"]["properties"]["skills"]["items"]["$ref"],
        "#/components/schemas/SkillItem"
    );
    assert_eq!(
        components["SkillProposalBenchmarkRunResponse"]["properties"]["record"]["$ref"],
        "#/components/schemas/SkillBenchmarkResponse"
    );
    assert_eq!(
        components["SkillLifecycleResponse"]["properties"]["skill"]["$ref"],
        "#/components/schemas/SkillItem"
    );
}

#[test]
fn skills_openapi_helpers_cover_daemon_skill_routes() {
    let mut components = skills_openapi_components();
    components.extend(crate::api_runtime::runtime_openapi_components());
    let expected_paths = [
        "/skills",
        "/skills/install",
        "/skills/provenance-review",
        "/skills/proposals",
        "/skills/proposals/{id}/quarantine",
        "/skills/proposals/{id}/benchmark",
        "/skills/proposals/{id}/run-benchmark",
        "/skills/training-runs",
        "/skills/training-runs/{id}",
        "/skills/training-runs/{id}/resume",
        "/skills/training-runs/{id}/execute",
        "/skills/training-runs/{id}/propose-candidate",
        "/skills/training-runs/{id}/cancel",
        "/skills/training-runs/{id}/history",
        "/skills/training-runs/{id}/rejected-edits",
        "/skills/{slug}/quarantine",
        "/skills/{slug}/activate",
        "/skills/{slug}/retire",
    ];

    let endpoints: Vec<_> = crate::api_contract::DAEMON_API_ENDPOINTS
        .iter()
        .filter(|endpoint| expected_paths.contains(&endpoint.path))
        .collect();
    assert_eq!(endpoints.len(), expected_paths.len() + 2);

    for endpoint in endpoints {
        let method = endpoint.method.to_ascii_lowercase();
        let response = skills_openapi_response_schema(&method, endpoint.path)
            .unwrap_or_else(|| panic!("missing response schema for {}", endpoint.path));
        assert!(
            components.contains_key(ref_target_name(&response)),
            "missing response component for {} {}",
            endpoint.method,
            endpoint.path
        );

        let request = skills_openapi_request_schema(&method, endpoint.path);
        if method == "post" {
            let request =
                request.unwrap_or_else(|| panic!("missing request schema for {}", endpoint.path));
            assert!(
                components.contains_key(ref_target_name(&request)),
                "missing request component for {} {}",
                endpoint.method,
                endpoint.path
            );
        } else {
            assert!(
                request.is_none(),
                "unexpected request schema for {} {}",
                endpoint.method,
                endpoint.path
            );
        }
    }
}
