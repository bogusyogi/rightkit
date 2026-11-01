mod tests {
    use super::super::*;

    #[test]
    fn reviewer_jury_is_verdict_bearing_top_three() {
        let panel = jury_for_category("review").unwrap();

        assert_eq!(panel.id, "reviewer-jury");
        assert_eq!(panel.kind, PanelKind::Jury);
        assert_eq!(panel.authority, PanelAuthority::Verdict);
        assert_eq!(panel.quorum, 2);
        assert_eq!(
            panel.model_ids(),
            vec!["qwen/qwen3.7-max", "moonshotai/kimi-k2.6", "z-ai/glm-5.2"]
        );
        assert!(panel.can_decide());
        assert_eq!(
            panel.member_ids(),
            vec!["plan-gap-finder", "architecture-risk", "execution-skeptic"]
        );
    }

    #[test]
    fn reviewer_council_is_advisory_only() {
        let panel = council_for_category("plan-review").unwrap();

        assert_eq!(panel.id, "reviewer-council");
        assert_eq!(panel.kind, PanelKind::Council);
        assert_eq!(panel.authority, PanelAuthority::Advisory);
        assert_eq!(
            panel.model_ids(),
            vec!["moonshotai/kimi-k2.6", "qwen/qwen3.7-max", "z-ai/glm-5.2"]
        );
        assert!(!panel.can_decide());
    }

    #[test]
    fn code_review_is_not_a_reviewer_panel_category() {
        assert!(jury_for_category("code-review").is_none());
        assert!(council_for_category("code-review").is_none());
    }

    #[test]
    fn visual_review_jury_uses_visual_top_three() {
        let panel = jury_for_category("visual-qa").unwrap();

        assert_eq!(panel.id, "visual-review-jury");
        assert_eq!(
            panel.model_ids(),
            vec![
                "qwen/qwen3.7-plus",
                "minimax/minimax-m3",
                "qwen/qwen3-vl-235b-a22b-instruct"
            ]
        );
    }

    #[test]
    fn visual_review_council_is_advisory_only() {
        let panel = council_for_category("screenshot").unwrap();

        assert_eq!(panel.id, "visual-review-council");
        assert_eq!(panel.authority, PanelAuthority::Advisory);
        assert_eq!(
            panel.model_ids(),
            vec![
                "qwen/qwen3.7-plus",
                "qwen/qwen3.7-plus",
                "minimax/minimax-m3"
            ]
        );
        assert!(!panel.can_decide());
    }

    #[test]
    fn parses_panel_verdicts() {
        assert_eq!(
            parse_panel_verdict("VERDICT: APPROVE\nCONFIDENCE: 0.82").0,
            Some(true)
        );
        assert_eq!(
            parse_panel_verdict("VERDICT: REJECT\nCONFIDENCE: 0.91").0,
            Some(false)
        );
        assert_eq!(parse_panel_verdict("needs work").0, None);
    }

    #[tokio::test]
    async fn executes_jury_panel_and_aggregates_majority() {
        use crate::coding::failure::FailureKind;
        use async_trait::async_trait;

        struct VerdictWorker;

        #[async_trait]
        impl Worker for VerdictWorker {
            async fn run(&self, task: &WorkerTask) -> WorkerResult {
                let reject = task.task_id.contains("execution-skeptic");
                WorkerResult {
                    task_id: task.task_id.clone(),
                    agent: task.agent.clone(),
                    success: true,
                    output: if reject {
                        "VERDICT: REJECT\nCONFIDENCE: 0.70\nrisk".into()
                    } else {
                        "VERDICT: APPROVE\nCONFIDENCE: 0.90\nok".into()
                    },
                    input_tokens: 0,
                    mop_fired: false,
                    failure_kind: if reject {
                        Some(FailureKind::Unknown)
                    } else {
                        None
                    },
                    agent_version: None,
                    rollback_step: None,
                    outcome: crate::coding::conductor::WorkerOutcome::Complete,
                }
            }
        }

        let panel = jury_for_category("review").unwrap();
        let result = execute_panel(&panel, "task-1", "artifact", Arc::new(VerdictWorker)).await;

        assert_eq!(result.member_results.len(), 3);
        assert_eq!(result.approval_count(), 2);
        assert_eq!(result.rejection_count(), 1);
        assert!(matches!(
            result.decision,
            Some(JuryDecision::Approved { .. })
        ));
        assert!(result.success);
    }

    #[tokio::test]
    async fn split_jury_runs_deliberation_ballot_and_uses_second_verdict() {
        use crate::coding::failure::FailureKind;
        use async_trait::async_trait;
        use std::sync::Mutex;

        struct DeliberationWorker {
            calls: Mutex<Vec<(String, String)>>,
        }

        impl DeliberationWorker {
            fn new() -> Self {
                Self {
                    calls: Mutex::new(Vec::new()),
                }
            }

            fn calls(&self) -> Vec<(String, String)> {
                self.calls.lock().unwrap().clone()
            }
        }

        #[async_trait]
        impl Worker for DeliberationWorker {
            async fn run(&self, task: &WorkerTask) -> WorkerResult {
                self.calls
                    .lock()
                    .unwrap()
                    .push((task.task_id.clone(), task.payload.clone()));
                let second_ballot = task.payload.contains("Second ballot after split vote");
                let member_id = task.task_id.rsplit(':').next().unwrap_or_default();

                if second_ballot {
                    assert!(task.payload.contains("plan-gap-finder: REJECT"));
                    assert!(task.payload.contains("confidence 0.70"));
                    assert!(task.payload.contains("gap blocks launch"));
                    assert!(task.payload.contains("architecture-risk: REJECT"));
                    assert!(task.payload.contains("execution-skeptic: APPROVE"));
                }

                let output = match (second_ballot, member_id) {
                    (false, "plan-gap-finder") => {
                        "VERDICT: REJECT\nCONFIDENCE: 0.70\ngap blocks launch"
                    }
                    (false, "architecture-risk") => {
                        "VERDICT: REJECT\nCONFIDENCE: 0.80\nboundary risk is unresolved"
                    }
                    (false, "execution-skeptic") => {
                        "VERDICT: APPROVE\nCONFIDENCE: 0.60\nsequence is workable"
                    }
                    (true, "execution-skeptic") => {
                        "VERDICT: REJECT\nCONFIDENCE: 0.72\nkeep one blocker open"
                    }
                    (true, _) => "VERDICT: APPROVE\nCONFIDENCE: 0.84\npeer evidence resolves risk",
                    (false, _) => "VERDICT: REJECT\nCONFIDENCE: 0.50\nunexpected juror",
                };

                WorkerResult {
                    task_id: task.task_id.clone(),
                    agent: task.agent.clone(),
                    success: true,
                    output: output.into(),
                    input_tokens: 0,
                    mop_fired: false,
                    failure_kind: if output.contains("REJECT") {
                        Some(FailureKind::Unknown)
                    } else {
                        None
                    },
                    agent_version: None,
                    rollback_step: None,
                    outcome: crate::coding::conductor::WorkerOutcome::Complete,
                }
            }
        }

        let worker = Arc::new(DeliberationWorker::new());
        let panel = jury_for_category("review").unwrap();
        let result = execute_panel(&panel, "task-1", "artifact", worker.clone()).await;

        assert_eq!(result.ballot_rounds, 2);
        assert_eq!(result.member_results.len(), 3);
        assert_eq!(worker.calls().len(), 6);
        assert!(matches!(
            result.decision,
            Some(JuryDecision::Approved { .. })
        ));
        assert!(result.success);
    }

    #[tokio::test]
    async fn unanimous_jury_skips_deliberation_ballot() {
        use async_trait::async_trait;
        use std::sync::Mutex;

        struct UnanimousWorker {
            calls: Mutex<Vec<String>>,
        }

        #[async_trait]
        impl Worker for UnanimousWorker {
            async fn run(&self, task: &WorkerTask) -> WorkerResult {
                self.calls.lock().unwrap().push(task.payload.clone());
                WorkerResult {
                    task_id: task.task_id.clone(),
                    agent: task.agent.clone(),
                    success: true,
                    output: "VERDICT: APPROVE\nCONFIDENCE: 0.91\nship it".into(),
                    input_tokens: 0,
                    mop_fired: false,
                    failure_kind: None,
                    agent_version: None,
                    rollback_step: None,
                    outcome: crate::coding::conductor::WorkerOutcome::Complete,
                }
            }
        }

        let worker = Arc::new(UnanimousWorker {
            calls: Mutex::new(Vec::new()),
        });
        let panel = jury_for_category("review").unwrap();
        let result = execute_panel(&panel, "task-1", "artifact", worker.clone()).await;

        let calls = worker.calls.lock().unwrap();
        assert_eq!(result.ballot_rounds, 1);
        assert_eq!(calls.len(), 3);
        assert!(calls
            .iter()
            .all(|payload| !payload.contains("Second ballot after split vote")));
        assert!(matches!(
            result.decision,
            Some(JuryDecision::Approved { .. })
        ));
        assert!(result.success);
    }
}
