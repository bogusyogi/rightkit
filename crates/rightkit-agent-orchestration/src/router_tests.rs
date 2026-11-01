mod tests {
    use super::super::*;
    use crate::registry::AgentDescriptor;
    use rightkit_agent_engine_protocol::TaskState;

    fn agent(name: &str, role: &str, caps: &[&str]) -> AgentDescriptor {
        let mut a = AgentDescriptor::new(name, role, "test/model");
        a.capabilities = caps.iter().map(|s| s.to_string()).collect();
        a
    }

    fn task(id: &str, role: &str, category: &str) -> TaskNode {
        task_with_duration(id, role, category, "")
    }

    fn task_with_duration(id: &str, role: &str, category: &str, duration: &str) -> TaskNode {
        TaskNode {
            id: id.into(),
            title: id.into(),
            deps: vec![],
            done_when: String::new(),
            done_when_predicates: vec![],
            state: TaskState::Open,
            category: category.into(),
            estimated_duration: duration.into(),
            assigned_role: role.into(),
            escalation_path: String::new(),
            open_questions: vec![],
            created_at: "t".into(),
            updated_at: "t".into(),
        }
    }

    fn registry() -> AgentRegistry {
        let mut r = AgentRegistry::new();
        r.register(agent("impl", "implementer", &["coding"]));
        r.register(agent("rev", "reviewer", &["review"]));
        r.register(agent("res", "researcher", &["search", "docs"]));
        r
    }

    #[test]
    fn route_by_role() {
        let r = registry();
        let router = AgentRouter::new();
        let t = task("t1", "reviewer", "anything");
        assert_eq!(router.route(&r, &t).unwrap().name, "rev");
    }

    #[test]
    fn route_by_capability_fallback() {
        let r = registry();
        let router = AgentRouter::new();
        // Role does not match any agent, but category "docs" is a capability of res.
        let t = task("t1", "no-such-role", "docs");
        assert_eq!(router.route(&r, &t).unwrap().name, "res");
    }

    #[test]
    fn route_none_when_no_match() {
        let r = registry();
        let router = AgentRouter::new();
        let t = task("t1", "no-such-role", "no-such-capability");
        assert!(router.route(&r, &t).is_none());
    }

    #[test]
    fn builtin_product_category_routes_to_product_analyst() {
        let r = AgentRegistry::with_builtins();
        let router = AgentRouter::new();
        let t = task("t1", "", "product");

        assert_eq!(router.route(&r, &t).unwrap().name, "product-analyst");
    }

    #[test]
    fn builtin_product_lead_requires_explicit_role() {
        let r = AgentRegistry::with_builtins();
        let router = AgentRouter::new();
        let t = task("t1", "product-lead", "product");

        assert_eq!(router.route(&r, &t).unwrap().name, "product-lead");
    }

    #[test]
    fn builtin_research_routes_to_researcher_not_auditor() {
        let r = AgentRegistry::with_builtins();
        let router = AgentRouter::new();

        assert_eq!(
            router.route(&r, &task("t1", "", "research")).unwrap().name,
            "researcher"
        );
        assert_eq!(
            router
                .route(&r, &task("t2", "", "ux-research"))
                .unwrap()
                .name,
            "researcher"
        );
    }

    #[test]
    fn builtin_claim_and_evidence_audits_route_to_auditor() {
        let r = AgentRegistry::with_builtins();
        let router = AgentRouter::new();

        assert_eq!(
            router
                .route(&r, &task("t1", "", "claim-audit"))
                .unwrap()
                .name,
            "auditor"
        );
        assert_eq!(
            router
                .route(&r, &task("t2", "", "evidence-audit"))
                .unwrap()
                .name,
            "auditor"
        );
    }

    #[test]
    fn builtin_code_review_routes_to_auditor_not_reviewer_panel() {
        let r = AgentRegistry::with_builtins();
        let router = AgentRouter::new();
        let decision = router
            .route_with_trace(&r, &task("t1", "", "code-review"))
            .unwrap();

        assert_eq!(decision.agent_name, "auditor");
        assert!(decision.jury_panel.is_none());
        assert!(decision.council_panel.is_none());
    }

    #[test]
    fn builtin_kimi_audit_stack_routes_to_kimi_auditor() {
        let r = AgentRegistry::with_builtins();
        let router = AgentRouter::new();
        let decision = router
            .route_with_trace(&r, &task("t1", "", "audit-stack-kimi"))
            .unwrap();

        assert_eq!(decision.agent_name, "auditor-kimi");
        assert_eq!(decision.model_id, "moonshotai/kimi-k2.6");
        assert!(decision.jury_panel.is_none());
        assert!(decision.council_panel.is_none());
    }

    #[test]
    fn builtin_plan_review_routes_to_reviewer_panel() {
        let r = AgentRegistry::with_builtins();
        let router = AgentRouter::new();
        let decision = router
            .route_with_trace(&r, &task("t1", "", "plan-review"))
            .unwrap();

        assert_eq!(decision.agent_name, "challenger");
        assert_eq!(decision.jury_panel.as_ref().unwrap().id, "reviewer-jury");
        assert_eq!(
            decision.council_panel.as_ref().unwrap().id,
            "reviewer-council"
        );
    }

    #[test]
    fn builtin_product_design_audit_routes_to_visual_review_panel() {
        let r = AgentRegistry::with_builtins();
        let router = AgentRouter::new();
        let decision = router
            .route_with_trace(&r, &task("t1", "", "product-design-audit"))
            .unwrap();

        assert_eq!(decision.agent_name, "lens");
        assert_eq!(
            decision.jury_panel.as_ref().unwrap().id,
            "visual-review-jury"
        );
        assert_eq!(
            decision.council_panel.as_ref().unwrap().id,
            "visual-review-council"
        );
    }

    #[test]
    fn builtin_code_duration_routes_short_to_coder_and_long_to_engineer() {
        let r = AgentRegistry::with_builtins();
        let router = AgentRouter::new();
        let short = task_with_duration("t1", "", "code", "short");
        let long = task_with_duration("t2", "", "code", "long");

        assert_eq!(router.route(&r, &short).unwrap().name, "coder");
        assert_eq!(router.route(&r, &long).unwrap().name, "engineer");
    }

    #[test]
    fn route_with_trace_explains_category_duration_match() {
        let r = AgentRegistry::with_builtins();
        let router = AgentRouter::new();
        let t = task_with_duration("t1", "", "code", "very-long");
        let decision = router.route_with_trace(&r, &t).unwrap();

        assert_eq!(decision.agent_name, "engineer");
        assert_eq!(decision.model_id, "xiaomi/mimo-v2.5-pro");
        assert_eq!(decision.matched_by, "category_duration");
        assert!(decision.rationale.contains("duration 'very-long'"));
    }

    #[test]
    fn route_with_trace_attaches_reviewer_jury_and_council() {
        let r = AgentRegistry::with_builtins();
        let router = AgentRouter::new();
        let t = task("t1", "", "review");
        let decision = router.route_with_trace(&r, &t).unwrap();

        assert_eq!(decision.agent_name, "challenger");
        assert_eq!(decision.jury_panel.as_ref().unwrap().id, "reviewer-jury");
        assert_eq!(
            decision.jury_panel.as_ref().unwrap().model_ids(),
            vec!["qwen/qwen3.7-max", "moonshotai/kimi-k2.6", "z-ai/glm-5.2"]
        );
        assert_eq!(
            decision.council_panel.as_ref().unwrap().id,
            "reviewer-council"
        );
    }

    #[test]
    fn route_with_trace_attaches_visual_jury_and_council() {
        let r = AgentRegistry::with_builtins();
        let router = AgentRouter::new();
        let t = task("t1", "", "visual-qa");
        let decision = router.route_with_trace(&r, &t).unwrap();

        assert_eq!(decision.agent_name, "lens");
        assert_eq!(
            decision.jury_panel.as_ref().unwrap().id,
            "visual-review-jury"
        );
        assert_eq!(
            decision.jury_panel.as_ref().unwrap().model_ids(),
            vec![
                "qwen/qwen3.7-plus",
                "minimax/minimax-m3",
                "qwen/qwen3-vl-235b-a22b-instruct"
            ]
        );
        assert_eq!(
            decision.council_panel.as_ref().unwrap().id,
            "visual-review-council"
        );
    }

    #[test]
    fn versioned_role_pack_assets_parse_and_validate_against_live_catalog() {
        let catalog = rightkit_llm::agent_providers::model_metadata::table();
        for role in ["orchestrator", "coder", "verifier"] {
            let pack = builtin_role_pack(role).unwrap();
            assert_eq!(pack.schema_version, 1);
            assert_eq!(pack.role, role);
            pack.validate(catalog).unwrap();
            assert!(pack.retry_limit <= 1);
            assert!(pack.verifier_receipt().independent);
        }
    }

    #[test]
    fn role_pack_unknown_model_fails_visibly_before_execution() {
        let mut pack = builtin_role_pack("coder").unwrap();
        pack.primary = "unknown/not-catalogued".into();
        let error = pack
            .validate(rightkit_llm::agent_providers::model_metadata::table())
            .unwrap_err();
        assert!(
            matches!(error, RolePackError::UnknownModel(model) if model == "unknown/not-catalogued")
        );
    }

    #[test]
    fn same_provider_verifier_has_explicit_degradation_receipt() {
        let mut pack = builtin_role_pack("coder").unwrap();
        pack.verifier = "xiaomi/mimo-v2.5".into();
        let receipt = pack.verifier_receipt();
        assert!(!receipt.independent);
        assert!(receipt.degradation_reason.is_some());
    }
}
