#[test]
fn parses_skill_governance_subcommands() {
    assert!(matches!(
        parse(&["coderight", "skill", "list"]).command,
        Command::Skill {
            command: SkillCommand::List { json: false }
        }
    ));
    assert!(matches!(
        parse(&["coderight", "skill", "list", "--json"]).command,
        Command::Skill {
            command: SkillCommand::List { json: true }
        }
    ));
    assert!(matches!(
        parse(&["coderight", "skill", "proposals"]).command,
        Command::Skill {
            command: SkillCommand::Proposals
        }
    ));

    let cli = parse(&[
        "coderight",
        "skill",
        "propose",
        "--slug",
        "reviewer",
        "--summary",
        "Tighten review behavior",
        "--content-file",
        "reviewer.md",
        "--id",
        "proposal-reviewer",
        "--title",
        "Reviewer",
        "--source-session",
        "sess-1",
        "--source-task",
        "task-1",
    ]);
    match &cli.command {
        Command::Skill {
            command:
                SkillCommand::Propose {
                    slug,
                    summary,
                    content_file,
                    id,
                    title,
                    source_session_id,
                    source_task_id,
                },
        } => {
            assert_eq!(slug, "reviewer");
            assert_eq!(summary, "Tighten review behavior");
            assert_eq!(content_file, "reviewer.md");
            assert_eq!(id.as_deref(), Some("proposal-reviewer"));
            assert_eq!(title.as_deref(), Some("Reviewer"));
            assert_eq!(source_session_id.as_deref(), Some("sess-1"));
            assert_eq!(source_task_id.as_deref(), Some("task-1"));
        }
        other => panic!("expected skill propose, got {other:?}"),
    }

    let cli = parse(&[
        "coderight",
        "skill",
        "benchmark-proposal",
        "proposal-reviewer",
        "--benchmark",
        "review-fixture",
        "--gate-mode",
        "mixed",
        "--score",
        "0.91",
        "--passed",
        "--slug",
        "reviewer",
    ]);
    match &cli.command {
        Command::Skill {
            command:
                SkillCommand::BenchmarkProposal {
                    proposal_id,
                    benchmark_id,
                    gate_mode,
                    score,
                    passed,
                    failed,
                    slug,
                    ..
                },
        } => {
            assert_eq!(proposal_id, "proposal-reviewer");
            assert_eq!(benchmark_id, "review-fixture");
            assert_eq!(gate_mode, "mixed");
            assert_eq!(*score, Some(0.91));
            assert!(*passed);
            assert!(!failed);
            assert_eq!(slug.as_deref(), Some("reviewer"));
        }
        other => panic!("expected skill benchmark-proposal, got {other:?}"),
    }

    let cli = parse(&[
        "coderight",
        "skill",
        "run-benchmark-proposal",
        "proposal-reviewer",
        "--kind",
        "role",
        "--session",
        "s1",
        "--role",
        "coder",
        "--goal",
        "Exercise reviewer fixture",
        "--benchmark",
        "review-fixture",
        "--gate-mode",
        "hard",
        "--threshold",
        "1.0",
        "--repeats",
        "2",
    ]);
    match &cli.command {
        Command::Skill {
            command:
                SkillCommand::RunBenchmarkProposal {
                    proposal_id,
                    kind,
                    session_id,
                    role,
                    goal,
                    benchmark_id,
                    gate_mode,
                    threshold,
                    repeats,
                    ..
                },
        } => {
            assert_eq!(proposal_id, "proposal-reviewer");
            assert_eq!(kind, "role");
            assert_eq!(session_id, "s1");
            assert_eq!(role.as_deref(), Some("coder"));
            assert_eq!(goal.as_deref(), Some("Exercise reviewer fixture"));
            assert_eq!(benchmark_id.as_deref(), Some("review-fixture"));
            assert_eq!(gate_mode.as_deref(), Some("hard"));
            assert_eq!(*threshold, Some(1.0));
            assert_eq!(*repeats, Some(2));
        }
        other => panic!("expected skill run-benchmark-proposal, got {other:?}"),
    }

    let cli = parse(&[
        "coderight",
        "skill",
        "start-training-run",
        "--name",
        "Review Tightening",
        "--skill",
        "reviewer",
        "--objective",
        "Catch unsafe shell suggestions",
        "--optimizer",
        "gpt-5",
        "--target-model",
        "kimi-k2.6",
        "--gate",
        "unit:hard:1.0",
        "--gate",
        "quality:mixed:0.8",
        "--rejected-edit",
        "diff-1:regressed safety:unsafe patch",
    ]);
    match &cli.command {
        Command::Skill {
            command:
                SkillCommand::StartTrainingRun {
                    name,
                    skill_id,
                    objective,
                    optimizer,
                    target_model,
                    gates,
                    rejected_edits,
                },
        } => {
            assert_eq!(name, "Review Tightening");
            assert_eq!(skill_id, "reviewer");
            assert_eq!(objective, "Catch unsafe shell suggestions");
            assert_eq!(optimizer, "gpt-5");
            assert_eq!(target_model.as_deref(), Some("kimi-k2.6"));
            assert_eq!(gates.len(), 2);
            assert_eq!(rejected_edits.len(), 1);
        }
        other => panic!("expected skill start-training-run, got {other:?}"),
    }

    let cli = parse(&[
        "coderight",
        "skill",
        "resume-training-run",
        "skillopt-1",
        "--checkpoint-file",
        "runtime_state.json",
        "--message",
        "continue epoch",
    ]);
    match &cli.command {
        Command::Skill {
            command:
                SkillCommand::ResumeTrainingRun {
                    run_id,
                    checkpoint_file,
                    message,
                },
        } => {
            assert_eq!(run_id, "skillopt-1");
            assert_eq!(checkpoint_file.as_deref(), Some("runtime_state.json"));
            assert_eq!(message.as_deref(), Some("continue epoch"));
        }
        other => panic!("expected skill resume-training-run, got {other:?}"),
    }

    let cli = parse(&[
        "coderight",
        "skill",
        "propose-training-candidate",
        "skillopt-1",
        "--operation",
        "replace",
        "--summary",
        "Restore evidence rule",
        "--instructions",
        "Keep it small",
    ]);
    match &cli.command {
        Command::Skill {
            command:
                SkillCommand::ProposeTrainingCandidate {
                    run_id,
                    operation,
                    summary,
                    instructions,
                },
        } => {
            assert_eq!(run_id, "skillopt-1");
            assert_eq!(operation.as_deref(), Some("replace"));
            assert_eq!(summary.as_deref(), Some("Restore evidence rule"));
            assert_eq!(instructions.as_deref(), Some("Keep it small"));
        }
        other => panic!("expected skill propose-training-candidate, got {other:?}"),
    }

    let cli = parse(&[
        "coderight",
        "skill",
        "execute-training-run",
        "skillopt-1",
        "--proposed-body-file",
        "skill_v0002.md",
        "--validation-score",
        "0.93",
        "--operation",
        "replace",
        "--summary",
        "Tighten evidence rule",
        "--gate-mode",
        "hard",
        "--threshold",
        "0.9",
    ]);
    match &cli.command {
        Command::Skill {
            command:
                SkillCommand::ExecuteTrainingRun {
                    run_id,
                    proposed_body_file,
                    validation_score,
                    operation,
                    summary,
                    gate_mode,
                    threshold,
                },
        } => {
            assert_eq!(run_id, "skillopt-1");
            assert_eq!(proposed_body_file, "skill_v0002.md");
            assert_eq!(*validation_score, 0.93);
            assert_eq!(operation, "replace");
            assert_eq!(summary.as_deref(), Some("Tighten evidence rule"));
            assert_eq!(gate_mode.as_deref(), Some("hard"));
            assert_eq!(*threshold, Some(0.9));
        }
        other => panic!("expected skill execute-training-run, got {other:?}"),
    }

    let cli = parse(&[
        "coderight",
        "skill",
        "cancel-training-run",
        "skillopt-1",
        "--reason",
        "stale",
    ]);
    match &cli.command {
        Command::Skill {
            command: SkillCommand::CancelTrainingRun { run_id, reason },
        } => {
            assert_eq!(run_id, "skillopt-1");
            assert_eq!(reason.as_deref(), Some("stale"));
        }
        other => panic!("expected skill cancel-training-run, got {other:?}"),
    }

    assert!(matches!(
        parse(&["coderight", "skill", "training-run-history", "skillopt-1"]).command,
        Command::Skill {
            command: SkillCommand::TrainingRunHistory { .. }
        }
    ));
    assert!(matches!(
        parse(&[
            "coderight",
            "skill",
            "training-run-rejected-edits",
            "skillopt-1"
        ])
        .command,
        Command::Skill {
            command: SkillCommand::TrainingRunRejectedEdits { .. }
        }
    ));

    let cli = parse(&[
        "coderight",
        "skill",
        "activate",
        "reviewer",
        "--operator-override",
        "--reason",
        "manual review",
    ]);
    match &cli.command {
        Command::Skill {
            command:
                SkillCommand::Activate {
                    slug,
                    operator_override,
                    reason,
                },
        } => {
            assert_eq!(slug, "reviewer");
            assert!(*operator_override);
            assert_eq!(reason.as_deref(), Some("manual review"));
        }
        other => panic!("expected skill activate, got {other:?}"),
    }
}
