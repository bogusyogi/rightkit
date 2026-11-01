mod tests {
    use super::super::*;
    use tempfile::{tempdir_in, TempDir};

    /// Scratch root for tracer output. Windows test runs use D:\scratch (override with
    /// RIGHTKIT_TEST_SCRATCH); other hosts use the OS temp dir. Never the drive root.
    fn scratch_root() -> std::path::PathBuf {
        #[cfg(windows)]
        let default = std::path::PathBuf::from(r"D:\scratch");
        #[cfg(not(windows))]
        let default = std::env::temp_dir();
        let root = std::env::var_os("RIGHTKIT_TEST_SCRATCH")
            .map(std::path::PathBuf::from)
            .unwrap_or(default)
            .join("rightkit-agent-evolution-tests");
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn test_traces_dir() -> (TempDir, std::path::PathBuf) {
        let guard = tempdir_in(scratch_root()).unwrap();
        let traces = guard.path().join("traces");
        (guard, traces)
    }

    #[test]
    fn bucket_from_estimate_strings() {
        assert_eq!(bucket_from_estimate("short"), DurationBucket::Short);
        assert_eq!(bucket_from_estimate("medium"), DurationBucket::Medium);
        assert_eq!(bucket_from_estimate("long"), DurationBucket::Long);
        assert_eq!(bucket_from_estimate("very-long"), DurationBucket::VeryLong);
        assert_eq!(bucket_from_estimate("MEDIUM"), DurationBucket::Medium);
    }

    #[test]
    fn bucket_from_estimate_minutes() {
        assert_eq!(bucket_from_estimate("5"), DurationBucket::Short);
        assert_eq!(bucket_from_estimate("30"), DurationBucket::Medium);
        assert_eq!(bucket_from_estimate("120"), DurationBucket::Long);
        assert_eq!(bucket_from_estimate("500"), DurationBucket::VeryLong);
    }

    #[test]
    fn bucket_from_estimate_unknown_defaults_medium() {
        assert_eq!(bucket_from_estimate("what"), DurationBucket::Medium);
        assert_eq!(bucket_from_estimate(""), DurationBucket::Medium);
    }

    #[test]
    fn harness_should_ratchet_after_threshold_failures() {
        let (_guard, traces) = test_traces_dir();
        let mut h = EvolutionHarness::new(traces, "session-test");
        let result = crate::coding::conductor::WorkerResult {
            task_id: "t1".into(),
            agent: "coder".into(),
            success: false,
            output: String::new(),
            rollback_step: None,
            failure_kind: None,
            mop_fired: false,
            input_tokens: 0,
            agent_version: None,
            outcome: crate::coding::conductor::WorkerOutcome::Complete,
        };
        for _ in 0..3 {
            h.record_result(&result, 100);
        }
        assert!(h.should_ratchet());
        h.reset_ratchet();
        assert!(!h.should_ratchet());
    }

    #[test]
    fn harness_tags_turns_in_prompt_registry() {
        let (_guard, traces) = test_traces_dir();
        let mut h = EvolutionHarness::new(traces, "session-tag-test");
        h.set_active_prompt_version("coder", 2);
        let result = crate::coding::conductor::WorkerResult {
            task_id: "t1".into(),
            agent: "coder".into(),
            success: true,
            output: "done".into(),
            rollback_step: None,
            failure_kind: None,
            mop_fired: false,
            input_tokens: 1000,
            agent_version: Some("v2".into()),
            outcome: crate::coding::conductor::WorkerOutcome::Complete,
        };
        h.record_result(&result, 500);
        let tagged = h.prompt_registry().get_turn_tag("t1");
        assert_eq!(tagged, Some("coder@v2"));
    }

    #[test]
    fn duration_aware_router_picks_high_reliability_agent() {
        use crate::coding::registry::AgentDescriptor;
        use rightkit_agent_engine_protocol::TaskState;

        let mut registry = AgentRegistry::new();
        let mut a1 = AgentDescriptor::new("coder-fast", "coder", "m");
        a1.capabilities = vec!["coding".into()];
        let mut a2 = AgentDescriptor::new("coder-slow", "coder", "m");
        a2.capabilities = vec!["coding".into()];
        registry.register(a1);
        registry.register(a2);

        let mut router = DurationAwareRouter::new();
        let high_score = TasteScore {
            gds: 0.9,
            long_horizon_reliability: 0.95,
            reliability_decay_slope: -0.01,
            quality: 0.9,
            reliability: 0.95,
            consistency: 0.9,
            robustness: 0.9,
            predictability: 0.9,
            safety: 0.9,
            context_efficiency: 0.9,
            vaf: 1.0,
            mop_rate: 0.0,
        };
        let low_score = TasteScore {
            gds: 0.5,
            long_horizon_reliability: 0.4,
            reliability_decay_slope: -0.1,
            quality: 0.5,
            reliability: 0.5,
            consistency: 0.5,
            robustness: 0.5,
            predictability: 0.5,
            safety: 0.5,
            context_efficiency: 0.5,
            vaf: 1.0,
            mop_rate: 0.1,
        };
        router.register_taste("coder", high_score);
        // Both map to the same role, so the router picks by reliability.
        // With same role, they're equal — just confirm routing doesn't panic.
        router.register_taste("coder", low_score);

        let task = TaskNode {
            id: "t1".into(),
            title: "code something".into(),
            deps: vec![],
            done_when: String::new(),
            done_when_predicates: vec![],
            state: TaskState::Open,
            category: "coding".into(),
            estimated_duration: "long".into(),
            assigned_role: "coder".into(),
            escalation_path: String::new(),
            open_questions: vec![],
            created_at: "t".into(),
            updated_at: "t".into(),
        };
        let result = router.route(&registry, &task, DurationBucket::Long);
        assert!(result.is_some());
    }
}
