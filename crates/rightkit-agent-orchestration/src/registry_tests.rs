mod tests {
    use super::super::*;

    fn agent(name: &str, model: &str) -> AgentDescriptor {
        AgentDescriptor::new(name, name, model)
    }

    #[test]
    fn register_and_get() {
        let mut r = AgentRegistry::new();
        r.register(agent("alice", "kimi"));
        assert_eq!(r.get("alice").unwrap().model_id, "kimi");
        assert!(r.get("bob").is_none());
    }

    #[test]
    fn builtins_load() {
        let r = AgentRegistry::with_builtins();
        assert!(
            r.len() >= 17,
            "expected ≥17 built-in agents, got {}",
            r.len()
        );
        let fc = r.get("coder").expect("fast-code-worker must be registered");
        assert_eq!(fc.escalation_tier, 1);
        assert!(!fc.model_id.is_empty());
        let boss = r.get("conductor").expect("orchestrator must be registered");
        assert_eq!(boss.escalation_tier, 3);
    }

    #[test]
    fn override_builtin_model() {
        let mut r = AgentRegistry::with_builtins();
        let mut custom = r.get("coder").unwrap().clone();
        custom.model_id = "custom/my-model".into();
        r.register(custom);
        assert_eq!(r.get("coder").unwrap().model_id, "custom/my-model");
    }

    #[test]
    fn empty() {
        let r = AgentRegistry::new();
        assert!(r.is_empty());
    }

    #[test]
    fn builtins_include_new_roles() {
        let r = AgentRegistry::with_builtins();
        for role in &[
            "product-analyst",
            "product-lead",
            "auditor",
            "auditor-kimi",
            "auditor-worker-kimi",
            "software-architect",
            "code-reviewer",
            "security-engineer",
            "technical-writer",
            "frontend-worker",
            "backend-architect",
            "devops-worker",
            "qa-analyst",
            "vision-juror",
        ] {
            assert!(
                r.get(role).is_some(),
                "expected new role '{role}' to be registered"
            );
        }
    }

    #[test]
    fn builtins_total_count() {
        let r = AgentRegistry::with_builtins();
        assert!(
            r.len() >= 21,
            "expected ≥21 built-in agents after audit-stack retirement (2026-06-20), got {}",
            r.len()
        );
    }

    #[test]
    fn final_roster_primary_models_are_locked() {
        let r = AgentRegistry::with_builtins();
        let expected = [
            ("conductor", "z-ai/glm-5.2"),
            ("architect", "moonshotai/kimi-k2.6"),
            ("coder", "xiaomi/mimo-v2.5"),
            ("engineer", "xiaomi/mimo-v2.5-pro"),
            ("ui-coder", "minimax/minimax-m3"),
            ("frontend-worker", "minimax/minimax-m3"),
            ("product-analyst", "minimax/minimax-m3"),
            ("product-lead", "moonshotai/kimi-k2.6"),
            ("researcher", "minimax/minimax-m3"),
            ("challenger", "moonshotai/kimi-k2.6"),
            ("auditor", "deepseek/deepseek-v4-pro"),
            ("auditor-kimi", "moonshotai/kimi-k2.6"),
            ("auditor-worker-kimi", "moonshotai/kimi-k2.5"),
            ("code-reviewer", "qwen/qwen3.7-max"),
            ("lens", "qwen/qwen3.7-plus"),
            ("vision-juror", "qwen/qwen3-vl-235b-a22b-instruct"),
            ("watchdog", "deepseek/deepseek-v4-flash"),
            ("writer", "moonshotai/kimi-k2.6"),
        ];

        for (role, model) in expected {
            assert_eq!(
                r.get(role)
                    .unwrap_or_else(|| panic!("{role} missing"))
                    .model_id,
                model
            );
        }
    }

    #[test]
    fn router_has_no_arm_for_retired_audit_stacks() {
        // Read router.rs source and assert the retired stacks are not routed.
        // Only auditor-kimi and the generic auditor arms are permitted.
        let router_src = include_str!("router.rs");
        let retired = [
            "audit-qwen",
            "audit-mimo",
            "audit-minimax",
            "audit-glm",
            "audit-worker-deepseek",
            "audit-worker-qwen",
            "audit-worker-mimo",
            "audit-worker-minimax",
            "audit-worker-glm",
            "auditor-worker-deepseek",
            "auditor-qwen",
            "auditor-worker-qwen",
            "auditor-mimo",
            "auditor-worker-mimo",
            "auditor-minimax",
            "auditor-worker-minimax",
            "auditor-glm",
            "auditor-worker-glm",
        ];
        for token in &retired {
            assert!(
                !router_src.contains(token),
                "router.rs contains retired audit-stack token {:?} — remove it before shipping",
                token
            );
        }
    }

    #[test]
    fn clarification_query_planner_to_coder() {
        let q = AgentRegistry::clarification_query_for("planner", "coder");
        assert!(q.is_some());
        assert!(q.unwrap().contains("acceptance criteria"));
    }

    #[test]
    fn clarification_query_architect_to_engineer() {
        let q = AgentRegistry::clarification_query_for("architect", "engineer");
        assert!(q.is_some());
        assert!(q.unwrap().contains("design spec"));
    }

    #[test]
    fn clarification_query_security_to_coder() {
        let q = AgentRegistry::clarification_query_for("security-engineer", "coder");
        assert!(q.is_some());
        assert!(q.unwrap().contains("mitigations"));
    }

    #[test]
    fn clarification_query_unknown_pair_returns_none() {
        let q = AgentRegistry::clarification_query_for("writer", "memorizer");
        assert!(q.is_none());
    }

    #[test]
    fn clarification_query_case_insensitive() {
        let lower = AgentRegistry::clarification_query_for("planner", "coder");
        let mixed = AgentRegistry::clarification_query_for("Planner", "Coder");
        assert_eq!(lower, mixed);
    }

    #[test]
    fn clarification_query_signoff_to_any() {
        let q = AgentRegistry::clarification_query_for("signoff", "coder");
        assert!(q.is_some());
        assert!(q.unwrap().contains("acceptance criteria"));
    }

    #[test]
    fn clarification_query_product_analyst_to_product_lead() {
        let q = AgentRegistry::clarification_query_for("product-analyst", "product-lead");
        assert!(q.is_some());
        assert!(q.unwrap().contains("final approval"));
    }
}
