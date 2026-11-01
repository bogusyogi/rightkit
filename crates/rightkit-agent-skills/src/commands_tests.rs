mod tests {
    use super::super::*;

    #[test]
    fn parses_slash_command() {
        let cmd = SlashCommand::parse("/help foo bar").unwrap();
        assert_eq!(cmd.name, "help");
        assert_eq!(cmd.args, vec!["foo", "bar"]);
    }

    #[test]
    fn returns_none_for_non_slash_input() {
        assert!(SlashCommand::parse("hello world").is_none());
        assert!(SlashCommand::parse("").is_none());
    }

    #[test]
    fn empty_args_command() {
        let cmd = SlashCommand::parse("/clear").unwrap();
        assert_eq!(cmd.name, "clear");
        assert!(cmd.args.is_empty());
    }

    #[test]
    fn registry_executes_help() {
        let registry = CommandRegistry::new();
        let result = registry.execute("help", &[]).unwrap();
        assert!(result.success);
        assert!(result.output.contains("/help"));
        assert!(result.output.contains("/audit"));
        assert!(result.output.contains("/blueprint"));
        assert!(result.output.contains("/architect"));
        assert!(result.output.contains("/commit"));
        assert!(result.output.contains("/dream now"));
    }

    #[test]
    fn registry_clear_returns_clear_action() {
        let registry = CommandRegistry::new();
        let result = registry.execute("clear", &[]).unwrap();
        assert_eq!(result.action, CommandAction::ClearChat);
    }

    #[test]
    fn registry_review_starts_auditor_code_review_goal() {
        let registry = CommandRegistry::new();
        let result = registry
            .execute(
                "review",
                &[
                    "focus".into(),
                    "on".into(),
                    "Windows".into(),
                    "tests".into(),
                ],
            )
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("read-only auditor review"));
        let CommandAction::ConductGoal(prompt) = result.action else {
            panic!("expected conduct_goal action");
        };
        assert!(prompt.contains("categorized as `code-review`"));
        assert!(prompt.contains("routes this to `auditor`"));
        assert!(prompt.contains("not the plan-review jury/council"));
        assert!(prompt.contains("Findings should lead"));
        assert!(prompt.contains("Additional focus: focus on Windows tests"));
    }

    #[test]
    fn registry_audit_starts_whole_repo_auditor_goal() {
        let registry = CommandRegistry::new();
        let result = registry
            .execute("audit", &["security".into(), "surfaces".into()])
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("whole-repo auditor pass"));
        let CommandAction::ConductGoal(prompt) = result.action else {
            panic!("expected conduct_goal action");
        };
        assert!(prompt.contains("whole-repo auditor flow"));
        assert!(prompt.contains("categorized as `audit`"));
        assert!(prompt.contains("existing `auditor` role"));
        assert!(prompt.contains("not the plan-review jury/council"));
        assert!(prompt.contains("doc drift"));
        assert!(prompt.contains("architecture contradictions"));
        assert!(prompt.contains("security smells"));
        assert!(prompt.contains("skipped, never as clean"));
        assert!(prompt.contains("Additional focus: security surfaces"));
    }

    #[test]
    fn registry_blueprint_starts_research_architecture_goal() {
        let registry = CommandRegistry::new();
        let result = registry
            .execute("blueprint", &["engine".into(), "map".into()])
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("blueprint mapping"));
        let CommandAction::ConductGoal(prompt) = result.action else {
            panic!("expected conduct_goal action");
        };
        assert!(prompt.contains("blueprint-map flow"));
        assert!(prompt.contains("categorized as `research` and `architecture`"));
        assert!(prompt.contains("existing `researcher` and `architect` roles"));
        assert!(prompt.contains("conductor synthesizing"));
        assert!(prompt.contains("critical files"));
        assert!(prompt.contains(".rightkit/understanding/"));
        assert!(prompt.contains("Additional focus: engine map"));
    }

    #[test]
    fn registry_architect_starts_adr_tdd_challenger_goal() {
        let registry = CommandRegistry::new();
        let result = registry
            .execute("architect", &["new".into(), "cache".into(), "layer".into()])
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("architect method"));
        assert!(result.output.contains("ADR"));
        assert!(result.output.contains("challenger gate"));
        let CommandAction::ConductGoal(prompt) = result.action else {
            panic!("expected conduct_goal action");
        };
        assert!(prompt.contains("categorized as `architecture`"));
        assert!(prompt.contains("existing `architect` role"));
        assert!(prompt.contains("challenger as the adversarial gate"));
        assert!(prompt.contains("Phase 1"));
        assert!(prompt.contains("Phase 2"));
        assert!(prompt.contains("Phase 3"));
        assert!(prompt.contains("Phase 4"));
        assert!(prompt.contains("ADR"));
        assert!(prompt.contains("2-3 concrete options"));
        assert!(prompt.contains("Context"));
        assert!(prompt.contains("Consequences"));
        assert!(prompt.contains("TDD"));
        assert!(prompt.contains("file-by-file"));
        assert!(prompt.contains("Critical Files"));
        assert!(prompt.contains("challenger reviews the ADR"));
        assert!(prompt.contains("Additional focus: new cache layer"));
    }

    #[test]
    fn registry_architect_no_args_still_invokes_conduct_goal() {
        let registry = CommandRegistry::new();
        let result = registry.execute("architect", &[]).unwrap();

        assert!(result.success);
        let CommandAction::ConductGoal(prompt) = result.action else {
            panic!("expected conduct_goal action");
        };
        assert!(prompt.contains("categorized as `architecture`"));
        assert!(prompt.contains("challenger"));
        assert!(!prompt.contains("Additional focus:"));
    }

    #[test]
    fn registry_commit_starts_diff_scoped_auditor_commit_push_goal() {
        let registry = CommandRegistry::new();
        let result = registry
            .execute(
                "commit",
                &[
                    "focus".into(),
                    "on".into(),
                    "binary".into(),
                    "guards".into(),
                ],
            )
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("diff-scoped auditor pass"));
        let CommandAction::ConductGoal(prompt) = result.action else {
            panic!("expected conduct_goal action");
        };
        // Roster-resolution parity with /audit: routes to the existing
        // `auditor` role, not the plan-review jury/council or an
        // implementation role.
        assert!(prompt.contains("categorized as `commit`"));
        assert!(prompt.contains("existing `auditor` role"));
        assert!(prompt.contains("not the plan-review jury/council"));
        // WS-6 contract: diff-scoped audit lenses, then commit+push gate.
        assert!(prompt.contains("diff-scoped audit lenses"));
        assert!(prompt.contains("commit+push gate"));
        // Forbidden route: must not shell out to external binaries.
        assert!(prompt.contains("do NOT shell out to external binaries"));
        // Phase-2 commit+push goes through `shell`. This used to require the
        // prompt to name `git_commit` / `git_push` and to forbid `shell` — but
        // no such tools are registered (only the read-only `git_diff` and
        // `git_read`), so the goal was directing the model at tools that do not
        // exist while banning the one path that does. The gate is the
        // permission engine: `git commit`/`git push` are off the AutoSafe
        // allowlist, so each is surfaced for approval and ledgered.
        assert!(prompt.contains("Run the git operations through `shell`"));
        assert!(prompt.contains("not on the AutoSafe allowlist"));
        assert!(
            !prompt.contains("`git_commit`"),
            "no such tool is registered"
        );
        assert!(!prompt.contains("`git_push`"), "no such tool is registered");
        // Findings-led output format, mirroring /audit and /review.
        assert!(prompt.contains("Findings should lead"));
        // Focus arg flows through to the prompt.
        assert!(prompt.contains("Additional focus: focus on binary guards"));
    }

    #[test]
    fn registry_commit_no_args_still_invokes_conduct_goal_with_auditor_route() {
        let registry = CommandRegistry::new();
        let result = registry.execute("commit", &[]).unwrap();

        assert!(result.success);
        // ConductGoal wiring must always be reached — even without a focus.
        let CommandAction::ConductGoal(prompt) = result.action else {
            panic!("expected conduct_goal action");
        };
        assert!(prompt.contains("categorized as `commit`"));
        assert!(prompt.contains("existing `auditor` role"));
        assert!(prompt.contains("commit+push gate"));
        // No focus arg ⇒ the tail must not carry a stray "Additional focus:" line.
        assert!(!prompt.contains("Additional focus:"));
    }

    #[test]
    fn registry_dream_now_starts_memory_consolidation_action() {
        let registry = CommandRegistry::new();
        let result = registry.execute("dream", &["now".into()]).unwrap();
        assert!(result.success);
        assert!(result.output.contains("memory dream consolidation"));
        assert_eq!(result.action, CommandAction::DreamNow("today".into()));
    }

    #[test]
    fn registry_lists_all_builtins() {
        let registry = CommandRegistry::new();
        let info = registry.list();
        let names: Vec<_> = info.iter().map(|i| i.name).collect();
        assert!(names.contains(&"help"));
        assert!(names.contains(&"clear"));
        assert!(names.contains(&"compact"));
        assert!(names.contains(&"sessions"));
    }

    #[test]
    fn registry_includes_daily_driver_commands() {
        let registry = CommandRegistry::new();
        let info = registry.list();
        let names: Vec<_> = info.iter().map(|i| i.name).collect();
        for expected in [
            "status",
            "caps",
            "doctor",
            "install",
            "diff",
            "review",
            "audit",
            "blueprint",
            "architect",
            "commit",
            "permissions",
            "mcp",
            "plugin",
            "teams",
            "messages",
            "schedules",
            "skills",
            "tasks",
            "benchmarks",
            "terminal-bench",
            "context",
            "summary",
            "export",
            "handoff",
        ] {
            assert!(names.contains(&expected), "missing /{expected}");
        }
    }

    #[test]
    fn registry_includes_handoff_alias_commands() {
        let registry = CommandRegistry::new();
        let info = registry.list();
        let names: Vec<_> = info.iter().map(|i| i.name).collect();
        for expected in ["bridge", "ide", "desktop", "mobile", "teleport"] {
            assert!(names.contains(&expected), "missing /{expected}");
        }
    }

    #[test]
    fn unknown_command_returns_none() {
        let registry = CommandRegistry::new();
        assert!(registry.execute("unknown_xyz", &[]).is_none());
    }

    fn audit_goal(args: &[&str]) -> String {
        let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        let registry = CommandRegistry::new();
        match registry
            .execute("audit", &args)
            .expect("audit exists")
            .action
        {
            CommandAction::ConductGoal(goal) => goal,
            other => panic!("expected a conductor goal, got {other:?}"),
        }
    }

    #[test]
    fn plain_audit_stays_read_only() {
        let goal = audit_goal(&[]);
        assert!(goal.contains("Stay read-only"));
    }

    /// `/audit fix` used to append "fix" as a focus line *inside* the read-only
    /// goal, so the model was told to stay read-only and to fix things in the
    /// same breath. It must now authorize edits instead.
    #[test]
    fn audit_fix_authorizes_edits_instead_of_contradicting_itself() {
        let goal = audit_goal(&["fix"]);
        assert!(
            !goal.contains("Stay read-only"),
            "fix mode must not be read-only"
        );
        assert!(
            !goal.contains("Additional focus: fix"),
            "`fix` is a mode, not a focus"
        );
        assert!(goal.contains("You ARE permitted to modify files"));
        assert!(goal.contains("never edit or delete a test to make it pass"));
        assert!(goal.contains("Do NOT commit or push"));
    }

    /// Observed on a real run: the fixer added two files to `.gitignore` that
    /// git already tracked, so the edit read as a fix, passed every check that
    /// could be run, and changed nothing — on a security note and customer
    /// data. Landing an edit is not the same as achieving its effect.
    #[test]
    fn audit_fix_requires_the_fix_to_demonstrably_take_effect() {
        let goal = audit_goal(&["fix"]);
        assert!(goal.contains("achieves its stated EFFECT, not merely that the edit landed"));
        assert!(goal.contains("git rm --cached"));
        assert!(goal.contains("State the check you ran and its result for every fix"));
    }

    /// A leading `fix` switches modes and the rest stays a focus, so
    /// `/audit fix the login flow` means "fix, scoped to the login flow".
    /// Anything else is an ordinary read-only audit with a focus.
    #[test]
    fn leading_fix_switches_mode_and_keeps_the_rest_as_focus() {
        let scoped = audit_goal(&["fix", "the", "login", "flow"]);
        assert!(!scoped.contains("Stay read-only"));
        assert!(scoped.contains("You ARE permitted to modify files"));
        assert!(scoped.contains("Additional focus: the login flow"));

        let unrelated = audit_goal(&["security"]);
        assert!(unrelated.contains("Stay read-only"));
        assert!(unrelated.contains("Additional focus: security"));
    }
}

mod alias_tests {
    use super::super::*;

    #[test]
    fn slash_aliases_resolve_to_canonical_entries() {
        for (typed, canonical) in [
            ("/exit", "/quit"),
            ("exit", "/quit"),
            ("/account", "/providers"),
            ("account", "/providers"),
            ("/new", "/clear"),
            ("/cls", "/clear"),
            ("/?", "/help"),
        ] {
            assert_eq!(
                resolve_query(typed).map(|e| e.name),
                Some(canonical),
                "{typed}"
            );
        }
    }

    #[test]
    fn login_is_not_a_command() {
        assert!(resolve_query("/login").is_none());
    }

    #[test]
    fn quit_and_providers_descriptions() {
        assert_eq!(resolve_query("/quit").unwrap().summary, "Quit Agent");
        assert!(resolve_query("/providers")
            .unwrap()
            .summary
            .starts_with("Providers & account"));
    }
}
