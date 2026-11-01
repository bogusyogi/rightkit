#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_engine_core::llm::LlmResponse;
    use std::sync::Mutex;

    fn registry() -> AgentRegistry {
        AgentRegistry::with_builtins()
    }

    #[derive(Default)]
    struct CapturingPlannerLlm {
        system_prompts: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl LlmClient for CapturingPlannerLlm {
        async fn complete(&self, messages: &[TranscriptMessage]) -> Result<LlmResponse, String> {
            let system = messages
                .iter()
                .find_map(|message| match message {
                    TranscriptMessage::System { content, .. } => Some(content.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            self.system_prompts.lock().unwrap().push(system);
            Ok(LlmResponse::text(
                r#"{"tasks":[{"id":"t1","title":"Review changes","role":"code-reviewer","deps":[],"done_when":"reviewed"},{"id":"t2","title":"Verify","role":"signoff","deps":["t1"],"done_when":"verified"}]}"#,
            ))
        }
    }

    #[test]
    fn parse_plan_extracts_tasks() {
        let raw = r#"Here is the plan:
        ```json
        {"tasks":[
          {"id":"t1","title":"Implement feature","role":"coder","deps":[],"done_when":"builds"},
          {"id":"t2","title":"Verify","role":"signoff","deps":["t1"],"done_when":"passes"}
        ]}
        ```"#;
        let tasks = parse_plan(raw, &registry()).unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].id, "t1");
        assert_eq!(tasks[0].assigned_role, "coder");
        assert_eq!(tasks[1].deps, vec!["t1".to_string()]);
    }

    #[test]
    fn parse_plan_defaults_missing_fields() {
        let raw = r#"{"tasks":[{"title":"only a title"}]}"#;
        let tasks = parse_plan(raw, &registry()).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "t1"); // synthesized
        assert!(tasks[0].deps.is_empty());
    }

    #[test]
    fn parse_plan_rejects_non_json() {
        assert!(parse_plan("no json here", &registry()).is_err());
    }

    #[test]
    fn parse_plan_rejects_empty_tasks() {
        assert!(parse_plan(r#"{"tasks":[]}"#, &registry()).is_err());
    }

    #[test]
    fn parse_plan_rejects_duplicate_ids() {
        let raw = r#"{"tasks":[{"id":"x","role":"coder"},{"id":"x","role":"coder"}]}"#;
        assert!(parse_plan(raw, &registry()).is_err());
    }

    #[test]
    fn detects_repeating_action_sequence_mop() {
        let output = "\
Tool: read_file src/lib.rs
Tool: read_file src/lib.rs
Tool: read_file src/lib.rs
Tool: read_file src/lib.rs
";
        assert!(detect_mop_in_output(output));
    }

    #[test]
    fn normal_action_sequence_is_not_mop() {
        let output = "\
Tool: read_file src/lib.rs
Tool: edit_file src/lib.rs
running cargo test
verify tests passed
";
        assert!(!detect_mop_in_output(output));
    }

    #[test]
    fn planner_prompt_lists_roles() {
        let p = planner_system_prompt(&registry(), &[]);
        assert!(p.contains("coder"));
        assert!(p.contains("signoff"));
        assert!(p.contains("\"tasks\""));
    }

    #[test]
    fn planner_prompt_lists_team_templates() {
        let teams = vec![rightkit_agent_tools::coding_tools::TeamDefinition {
            name: "review-squad".to_string(),
            description: Some("audit changes before merge".to_string()),
            members: vec!["code-reviewer".to_string(), "qa-analyst".to_string()],
            max_workers: Some(2),
            budget_usd: Some(0.75),
        }];

        let p = planner_system_prompt(&registry(), &teams);

        assert!(p.contains("Team templates"));
        assert!(p.contains("review-squad"));
        assert!(p.contains("audit changes before merge"));
        assert!(p.contains("code-reviewer, qa-analyst"));
        assert!(p.contains("maxWorkers: 2"));
        assert!(p.contains("budgetUsd: 0.75"));
        assert!(p.contains("team templates are not roles"));
    }

    #[test]
    fn named_team_preset_becomes_execution_constraints() {
        let teams = vec![rightkit_agent_tools::coding_tools::TeamDefinition {
            name: "review-squad".to_string(),
            description: Some("audit changes before merge".to_string()),
            members: vec!["code-reviewer".to_string(), "qa-analyst".to_string()],
            max_workers: Some(2),
            budget_usd: Some(0.75),
        }];

        let preset =
            matching_team_execution_preset("Use review-squad to inspect this branch", &teams)
                .unwrap();

        assert_eq!(preset.team_name, "review-squad");
        assert_eq!(preset.max_workers, Some(2));
        assert_eq!(preset.budget_usd, Some(0.75));
    }

    #[test]
    fn unnamed_team_does_not_constrain_execution() {
        let teams = vec![rightkit_agent_tools::coding_tools::TeamDefinition {
            name: "review-squad".to_string(),
            description: Some("audit changes before merge".to_string()),
            members: vec!["code-reviewer".to_string()],
            max_workers: Some(1),
            budget_usd: Some(0.10),
        }];

        assert!(matching_team_execution_preset("Fix the parser bug", &teams).is_none());
    }

    #[test]
    fn effective_team_limits_are_stricter_than_global_limits() {
        let preset = TeamExecutionPreset {
            team_name: "review-squad".to_string(),
            max_workers: Some(2),
            budget_usd: Some(0.75),
        };

        assert_eq!(effective_max_workers(8, Some(&preset)), 2);
        assert_eq!(effective_max_workers(0, Some(&preset)), 2);
        assert!((effective_budget_ceiling(2.0, Some(&preset)) - 0.75).abs() < f64::EPSILON);
        assert!((effective_budget_ceiling(0.0, Some(&preset)) - 0.75).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn plan_goal_sends_team_templates_to_planner_llm() {
        let llm = CapturingPlannerLlm::default();
        let teams = vec![rightkit_agent_tools::coding_tools::TeamDefinition {
            name: "review-squad".to_string(),
            description: Some("audit changes before merge".to_string()),
            members: vec!["code-reviewer".to_string(), "qa-analyst".to_string()],
            max_workers: Some(2),
            budget_usd: Some(0.75),
        }];

        let tasks = plan_goal(
            &llm,
            "Use review-squad to inspect this branch",
            None,
            &registry(),
            &teams,
        )
        .await
        .unwrap();

        assert_eq!(tasks.len(), 2);
        let prompts = llm.system_prompts.lock().unwrap();
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0].contains("review-squad"));
        assert!(prompts[0].contains("team templates are not roles"));
        assert!(prompts[0].contains("inspect the affected current files before any edit task"));
        assert!(prompts[0].contains("capture the baseline behavior before editing"));
    }

    #[test]
    fn parse_verdict_reads_explicit_verdict_line() {
        assert!(parse_verdict("VERDICT: PASS\nlooks good").0);
        assert!(parse_verdict("VERDICT: APPROVE\nCONFIDENCE: 0.9").0);
        assert!(!parse_verdict("VERDICT: FAIL\nmissing file").0);
        assert!(!parse_verdict("VERDICT: REJECT\nnope").0);
    }

    #[test]
    fn parse_verdict_is_conservative_when_ambiguous() {
        // No clear verdict → not approved (never grant completion by default).
        assert!(!parse_verdict("I think it might be okay maybe").0);
        assert!(!parse_verdict("").0);
    }

    #[test]
    fn parse_verdict_reads_confidence() {
        let (_, c) = parse_verdict("VERDICT: APPROVE\nCONFIDENCE: 0.42\nbecause");
        assert!((c - 0.42).abs() < 1e-9);
        // Default when absent.
        let (_, d) = parse_verdict("VERDICT: APPROVE");
        assert!((d - 0.8).abs() < 1e-9);
    }

    #[test]
    fn parse_verdict_reject_beats_pass_keyword() {
        // A reject verdict that mentions "pass" elsewhere still rejects.
        let (a, _) = parse_verdict("VERDICT: REJECT\nit does not pass the check");
        assert!(!a);
    }

    #[test]
    fn content_hash_is_stable_and_differs() {
        assert_eq!(content_hash("abc"), content_hash("abc"));
        assert_ne!(content_hash("abc"), content_hash("abd"));
        assert_eq!(content_hash("abc").len(), 16);
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hello…");
    }

    #[test]
    fn conductor_prefers_live_runtime_model_when_allowed() {
        let model = resolve_conductor_runtime_model(
            "moonshotai/kimi-k2.6",
            "z-ai/glm-5.2",
            Some("fallback/model"),
        );
        assert_eq!(model, "moonshotai/kimi-k2.6");
    }

    #[test]
    fn conductor_accepts_minimax_as_a_live_runtime_model() {
        let model = resolve_conductor_runtime_model(
            "minimax/minimax-m3",
            "z-ai/glm-5.2",
            Some("fallback/model"),
        );
        assert_eq!(model, "minimax/minimax-m3");
    }

    #[test]
    fn conductor_falls_back_to_configured_model_when_live_model_is_not_allowed() {
        let model =
            resolve_conductor_runtime_model("openai/gpt-5", "z-ai/glm-5.2", Some("fallback/model"));
        assert_eq!(model, "z-ai/glm-5.2");
    }

    #[test]
    fn conductor_uses_engine_default_when_no_runtime_or_config_model_exists() {
        let model = resolve_conductor_runtime_model("", "", Some("fallback/model"));
        assert_eq!(model, "fallback/model");
    }
}
