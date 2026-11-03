mod tests {
    use super::*;
    use rightkit_memory::backend::MemoryBackend as _;

    fn summary(id: &str, succeeded: bool) -> SessionSummary {
        SessionSummary {
            session_id: id.into(),
            succeeded,
            tool_calls: 12,
            errors: vec!["boom".into()],
            notable_decisions: vec!["chose approach A".into()],
        }
    }

    #[test]
    fn record_run_tallies_role_stats() {
        let h = LearningHarness::new();
        h.record_run(
            &summary("s1", true),
            &[("coder".into(), true), ("signoff".into(), false)],
        );
        h.record_run(&summary("s2", true), &[("coder".into(), true)]);

        let metrics = h.metrics_json();
        let coder = metrics.iter().find(|m| m["role"] == "coder").unwrap();
        assert_eq!(coder["runs"], 2);
        assert_eq!(coder["successes"], 2);
        assert_eq!(coder["successRate"], 1.0);
        let signoff = metrics.iter().find(|m| m["role"] == "signoff").unwrap();
        assert_eq!(signoff["successRate"], 0.0);
    }

    #[test]
    fn record_run_stores_proposals() {
        let h = LearningHarness::new();
        h.record_run(&summary("s1", true), &[("coder".into(), true)]);
        let props = h.proposals_json();
        // 1 memory (decision) + 1 harness rule (error) + 1 workflow (busy+success)
        // + 1 synthesized skill (complex success with a notable decision).
        assert_eq!(props.len(), 4);
    }

    #[test]
    fn proposal_ids_do_not_collide_across_runs() {
        let h = LearningHarness::new();
        h.record_run(&summary("s1", true), &[]);
        h.record_run(&summary("s2", true), &[]);
        let props = h.proposals_json();
        let ids: std::collections::HashSet<_> = props
            .iter()
            .map(|p| p["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids.len(), props.len(), "all proposal ids must be unique");
    }

    #[test]
    fn lessons_returns_promoted_harness_rules() {
        let h = LearningHarness::new();
        h.record_run(&summary("s1", true), &[("coder".into(), true)]);
        let lessons = h.lessons();
        // The error "boom" becomes a promoted harness rule (base 0.65 > 0.6).
        assert!(
            lessons.iter().any(|l| l.contains("avoid: boom")),
            "lessons: {lessons:?}"
        );
    }

    #[test]
    fn lessons_are_capped() {
        let h = LearningHarness::new();
        for i in 0..30 {
            let mut s = summary(&format!("s{i}"), true);
            s.errors = vec![format!("err-{i}")];
            s.notable_decisions = vec![];
            h.record_run(&s, &[]);
        }
        assert!(h.lessons().len() <= MAX_INJECTED_LESSONS);
    }

    #[test]
    fn persists_and_reloads_across_instances() {
        let path = std::env::temp_dir().join(format!("cr-learn-test-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let db = EvolutionDb::open(&path).unwrap();
            let h = LearningHarness::open(db);
            h.record_run(&summary("s1", true), &[("coder".into(), true)]);
        }
        let db = EvolutionDb::open(&path).unwrap();
        let reloaded = LearningHarness::open(db);
        assert!(
            !reloaded.proposals_json().is_empty(),
            "proposals should survive a reload"
        );
        assert!(
            reloaded.metrics_json().iter().any(|m| m["role"] == "coder"),
            "role stats should survive a reload"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn metrics_sorted_worst_first() {
        let h = LearningHarness::new();
        h.record_run(
            &summary("s1", true),
            &[("good".into(), true), ("bad".into(), false)],
        );
        let metrics = h.metrics_json();
        assert_eq!(
            metrics[0]["role"], "bad",
            "worst success rate should sort first"
        );
    }

    #[test]
    fn self_review_fork_writes_candidate_proposal_to_quarantine() {
        let h = LearningHarness::new();
        h.review_and_propose(SelfReviewInput {
            session_id: "s-review".into(),
            model: "haiku".into(),
            allowed_tools: vec![
                "read_memory".into(),
                "write_memory".into(),
                "learning_propose".into(),
                "remember_consolidated".into(),
            ],
            shell_allowed: false,
            stable_system_prompt_bytes: b"stable system\n".to_vec(),
            stable_system_cache_key: Some("cache:prompt:abc".into()),
            transcript: Vec::new(),
            assistant_text: "Remember: RightKit self-review proposals must remain quarantined."
                .into(),
        });

        let props = h.proposals_json();
        assert_eq!(props.len(), 1);
        assert_eq!(props[0]["kind"], "Memory");
        assert_eq!(props[0]["status"], "Candidate");
        assert_eq!(props[0]["score"], 0.0);
        assert!(props[0]["id"]
            .as_str()
            .unwrap()
            .starts_with("self-review-s-review-"));
        assert!(props[0]["evidence"]
            .as_str()
            .unwrap()
            .contains("cache:prompt:abc"));
        assert!(h.lessons().is_empty(), "candidate proposals are not live");
    }

    #[test]
    fn self_review_fork_rejects_unrestricted_inputs() {
        let h = LearningHarness::new();
        h.review_and_propose(SelfReviewInput {
            session_id: "s-review".into(),
            model: "haiku".into(),
            allowed_tools: vec!["learning_propose".into()],
            shell_allowed: true,
            stable_system_prompt_bytes: Vec::new(),
            stable_system_cache_key: None,
            transcript: Vec::new(),
            assistant_text: "Remember: should not be stored.".into(),
        });
        h.review_and_propose(SelfReviewInput {
            session_id: "s-review".into(),
            model: "other".into(),
            allowed_tools: vec!["learning_propose".into()],
            shell_allowed: false,
            stable_system_prompt_bytes: Vec::new(),
            stable_system_cache_key: None,
            transcript: Vec::new(),
            assistant_text: "Remember: should not be stored either.".into(),
        });

        assert!(h.proposals_json().is_empty());
    }

    #[test]
    fn self_review_fork_rejects_extra_tool_even_without_shell_flag() {
        let h = LearningHarness::new();
        h.review_and_propose(SelfReviewInput {
            session_id: "s-review-extra".into(),
            model: "haiku".into(),
            allowed_tools: vec!["learning_propose".into(), "shell".into()],
            shell_allowed: false,
            stable_system_prompt_bytes: Vec::new(),
            stable_system_cache_key: None,
            transcript: Vec::new(),
            assistant_text: "Remember: should remain outside quarantine.".into(),
        });

        assert!(h.proposals_json().is_empty());
    }

    fn candidate(session_id: &str, kind: &str) -> ReviewCandidate {
        ReviewCandidate {
            session_id: session_id.into(),
            reason: "flagged for review".into(),
            suggested_proposal_kind: kind.into(),
            trajectory_excerpt: String::new(),
        }
    }

    #[test]
    fn daily_analysis_mints_proposals_for_review_candidates() {
        let h = LearningHarness::new();
        let report = DailyReport {
            review_candidates: vec![
                candidate("s1", "HarnessRule"),
                candidate("s2", "Workflow"),
                candidate("s3", "anything-else"),
            ],
            ..DailyReport::default()
        };
        mint_review_proposals(&h, &report);

        let props = h.proposals_json();
        assert_eq!(props.len(), 3, "one proposal per review candidate");
        let by_id = |id: &str| {
            props
                .iter()
                .find(|p| p["id"] == format!("daily-analyzer-{id}"))
                .cloned()
                .unwrap_or_else(|| panic!("missing proposal for {id}"))
        };
        assert_eq!(by_id("s1")["kind"], "HarnessRule");
        assert_eq!(by_id("s2")["kind"], "Workflow");
        // Unknown kinds fall back to Memory.
        assert_eq!(by_id("s3")["kind"], "Memory");
        // Minted proposals start as candidates (await approval), never auto-promoted.
        assert_eq!(by_id("s1")["status"], "Candidate");
    }

    #[test]
    fn daily_analysis_is_idempotent() {
        let h = LearningHarness::new();
        let report = DailyReport {
            review_candidates: vec![candidate("s1", "HarnessRule")],
            ..DailyReport::default()
        };
        mint_review_proposals(&h, &report);
        mint_review_proposals(&h, &report);
        assert_eq!(
            h.proposals_json().len(),
            1,
            "re-running analysis must not duplicate the proposal for a session"
        );
    }

    #[tokio::test]
    async fn daily_analysis_consolidates_memory_candidates_and_repeated_failures() {
        let memory = crate::test_support::FakeMemoryBackend::new();
        let report = DailyReport {
            review_candidates: vec![
                candidate("s-memory", "Memory"),
                candidate("s-workflow", "Workflow"),
                candidate("s-harness", "HarnessRule"),
            ],
            repeated_failures: vec![("cargo test failed".to_string(), 3)],
            ..DailyReport::default()
        };

        let written = consolidate_daily_memory(memory.as_ref(), &report)
            .await
            .expect("fake backend accepts consolidation");

        assert_eq!(written, 2);
        assert_eq!(memory.len(), 2);
        let ctx = rightkit_memory::backend::CallContext::unbounded();
        let memory_hits = memory
            .search(&ctx, "session memory flagged review", 10)
            .await
            .unwrap();
        assert!(
            memory_hits
                .iter()
                .any(|entry| entry.id == "daily-analyzer-memory-s-memory"
                    && entry.content.contains("s-memory")),
            "memory hits: {memory_hits:?}"
        );
        let failure_hits = memory.search(&ctx, "cargo test failed", 10).await.unwrap();
        assert!(
            failure_hits.iter().any(|entry| entry
                .id
                .starts_with("daily-analyzer-repeated-failure-0-cargo-test-failed")),
            "failure hits: {failure_hits:?}"
        );
    }

    #[tokio::test]
    async fn daily_memory_consolidation_is_idempotent() {
        let memory = crate::test_support::FakeMemoryBackend::new();
        let report = DailyReport {
            review_candidates: vec![candidate("s-memory", "Memory")],
            repeated_failures: vec![("same error".to_string(), 2)],
            ..DailyReport::default()
        };

        consolidate_daily_memory(memory.as_ref(), &report)
            .await
            .unwrap();
        consolidate_daily_memory(memory.as_ref(), &report)
            .await
            .unwrap();

        assert_eq!(memory.len(), 2);
    }

    fn event(kind: &str, payload: serde_json::Value) -> rightkit_agent_engine_storage::EventRow {
        rightkit_agent_engine_storage::EventRow {
            session_id: "s-events".to_string(),
            seq: 1,
            kind: kind.to_string(),
            payload_json: payload.to_string(),
            created_at: crate::api::now_iso(),
        }
    }

    #[test]
    fn session_summary_uses_persisted_event_signals() {
        let events = vec![
            event(
                "toolCallStarted",
                serde_json::json!({
                    "$kind": "toolCallStarted",
                    "call": {"name": "shell"}
                }),
            ),
            event(
                "toolResultAdded",
                serde_json::json!({
                    "$kind": "toolResultAdded",
                    "result": {
                        "$kind": "tool",
                        "summary": "cargo test failed",
                        "isError": true
                    }
                }),
            ),
            event(
                "verificationFinished",
                serde_json::json!({
                    "$kind": "verificationFinished",
                    "summary": "verification failed",
                    "passed": false,
                    "issues": [{"message": "missing assertion"}]
                }),
            ),
            event(
                "humanSteeringInjected",
                serde_json::json!({
                    "$kind": "humanSteeringInjected",
                    "contentPreview": "prefer the narrow fix"
                }),
            ),
            event(
                "toolApprovalResolved",
                serde_json::json!({
                    "$kind": "toolApprovalResolved",
                    "toolCallId": "tc-1",
                    "approved": true
                }),
            ),
        ];

        let summary = session_summary_from_events("s-events", "running", &events);

        assert!(!summary.succeeded);
        assert_eq!(summary.tool_calls, 1);
        assert!(
            summary
                .errors
                .iter()
                .any(|error| error == "cargo test failed"),
            "errors: {:?}",
            summary.errors
        );
        assert!(
            summary
                .errors
                .iter()
                .any(|error| error == "missing assertion"),
            "errors: {:?}",
            summary.errors
        );
        assert!(
            summary
                .notable_decisions
                .iter()
                .any(|decision| decision.contains("prefer the narrow fix")),
            "decisions: {:?}",
            summary.notable_decisions
        );
        assert!(
            summary
                .notable_decisions
                .iter()
                .any(|decision| decision.contains("tc-1")),
            "decisions: {:?}",
            summary.notable_decisions
        );
    }

    #[test]
    fn execution_observations_are_validated_as_non_authoritative_learning_evidence() {
        let valid = rightkit_agent_engine_runtime_middleware::RuntimeObservation {
            id: "execution:tool-1".to_string(),
            span_id: "tool-1".to_string(),
            session_id: "s-observation".to_string(),
            severity: "error".to_string(),
            message: "PreToolCall: failed".to_string(),
            payload: serde_json::json!({
                "schemaVersion": "coderight.execution-observation.v1",
                "eventId": "tool-1",
                "sessionId": "s-observation",
                "causalParentId": "session-1",
                "phase": "preToolCall",
                "status": "failed",
                "createdAt": "2026-08-26T00:00:01Z",
                "attributes": {"tool": "shell"},
            }),
            created_at: "2026-08-26T00:00:01Z".to_string(),
        };
        let invalid = rightkit_agent_engine_runtime_middleware::RuntimeObservation {
            payload: serde_json::json!({ "schemaVersion": "other" }),
            ..valid.clone()
        };

        let evidence = execution_observation_evidence([&valid, &invalid].into_iter());
        assert_eq!(
            evidence,
            vec!["execution observation: phase=preToolCall, status=unsuccessful, parent=session-1"]
        );
        assert!(!evidence[0].contains("shell"));
        assert!(!evidence[0].contains("failed"));
    }
}
