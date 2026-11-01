#[cfg(test)]
mod tests_instincts {
    use super::*;

    fn learning_proposal(
        id: &str,
        kind: rightkit_adapt::ProposalKind,
        summary: &str,
    ) -> rightkit_adapt::Proposal {
        rightkit_adapt::Proposal {
            id: id.to_string(),
            kind,
            summary: summary.to_string(),
            evidence: "test evidence".to_string(),
            score: 0.9,
            status: rightkit_adapt::ProposalStatus::Promoted,
            created_at: "2026-06-13".to_string(),
            age_sessions: 0,
            rejection_reason: None,
        }
    }

    #[test]
    fn approved_memory_or_harness_rule_proposal_becomes_instinct() {
        let proposal = learning_proposal(
            "daily-analyzer/session 1",
            rightkit_adapt::ProposalKind::HarnessRule,
            "avoid repeated cargo failures",
        );

        let instinct = proposal_to_instinct(&proposal).expect("instinct");

        assert_eq!(instinct.id, "learning-daily-analyzer-session-1");
        assert_eq!(instinct.rule, "avoid repeated cargo failures");
        assert_eq!(instinct.observed_at_sessions, vec![proposal.id]);
        assert!(!instinct.deprecated);
    }

    #[test]
    fn approved_skill_or_workflow_proposal_does_not_become_instinct() {
        let workflow = learning_proposal(
            "wf-1",
            rightkit_adapt::ProposalKind::Workflow,
            "workflow proposal",
        );
        let skill = learning_proposal(
            "skill-1",
            rightkit_adapt::ProposalKind::Skill,
            "skill proposal",
        );

        assert!(proposal_to_instinct(&workflow).is_none());
        assert!(proposal_to_instinct(&skill).is_none());
    }

    #[test]
    fn persist_instinct_store_roundtrips_json() {
        let root = std::env::temp_dir().join(format!(
            "coderight-instinct-writeback-{}",
            std::process::id()
        ));
        let path = root.join("instincts.json");
        let _ = std::fs::remove_dir_all(&root);

        let proposal = learning_proposal(
            "proposal/with spaces",
            rightkit_adapt::ProposalKind::Memory,
            "remember the selected architecture",
        );
        let mut store = rightkit_adapt::InstinctStore::new();
        store.add(proposal_to_instinct(&proposal).unwrap());

        persist_instinct_store(&store, &path).unwrap();
        let reloaded =
            rightkit_adapt::InstinctStore::load_from_json(&path.to_string_lossy()).unwrap();

        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded.all()[0].rule, "remember the selected architecture");

        let _ = std::fs::remove_dir_all(&root);
    }
}
