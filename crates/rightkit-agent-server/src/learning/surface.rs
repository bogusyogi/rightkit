include!("harness.rs");
include!("daily_analysis.rs");
include!("analyzer_source.rs");

#[cfg(test)]
include!("tests.rs");

/// Additional coverage for `harness.rs`, `daily_analysis.rs`, and
/// `analyzer_source.rs` pure helpers that are not already exercised by
/// `tests.rs` (module name kept distinct from `tests` since both are
/// `include!`d into this same translation unit).
#[cfg(test)]
mod owned_file_unit_tests {
    use super::*;

    // -- daily_analysis.rs: stable_slug --------------------------------

    #[test]
    fn stable_slug_normalizes_and_collapses_punctuation() {
        assert_eq!(stable_slug("Cargo Test Failed!!"), "cargo-test-failed");
        assert_eq!(stable_slug("  leading/trailing  "), "leading-trailing");
    }

    #[test]
    fn stable_slug_falls_back_to_unknown_for_empty_input() {
        assert_eq!(stable_slug(""), "unknown");
        assert_eq!(stable_slug("   "), "unknown");
        // Punctuation-only input has no alphanumeric parts either.
        assert_eq!(stable_slug("!!!---???"), "unknown");
    }

    #[test]
    fn stable_slug_bounds_length_and_word_count() {
        let many_words = (0..20)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let slug = stable_slug(&many_words);
        assert!(slug.len() <= 80);
        // Only the first 8 whitespace-delimited parts survive.
        assert_eq!(slug.split('-').count(), 8);
    }

    // -- daily_analysis.rs: review_candidate_to_proposal ---------------

    fn candidate(session_id: &str, kind: &str) -> ReviewCandidate {
        ReviewCandidate {
            session_id: session_id.into(),
            reason: "flagged for review".into(),
            suggested_proposal_kind: kind.into(),
            trajectory_excerpt: String::new(),
        }
    }

    #[test]
    fn review_candidate_to_proposal_maps_known_and_unknown_kinds() {
        let harness_proposal = review_candidate_to_proposal(&candidate("s1", "HarnessRule"));
        assert!(matches!(harness_proposal.kind, ProposalKind::HarnessRule));
        assert_eq!(harness_proposal.id, "daily-analyzer-s1");
        assert_eq!(harness_proposal.status, ProposalStatus::Candidate);
        assert_eq!(harness_proposal.score, 0.5);
        assert!(harness_proposal.summary.contains("s1"));
        assert!(harness_proposal.summary.contains("flagged for review"));
        assert!(harness_proposal.evidence.is_empty());

        let workflow_proposal = review_candidate_to_proposal(&candidate("s2", "Workflow"));
        assert!(matches!(workflow_proposal.kind, ProposalKind::Workflow));

        // Unrecognized kinds fall back to Memory.
        let fallback_proposal = review_candidate_to_proposal(&candidate("s3", "anything-else"));
        assert!(matches!(fallback_proposal.kind, ProposalKind::Memory));
    }

    // -- harness.rs: self_review_draft_to_proposal ---------------------

    #[test]
    fn self_review_draft_to_proposal_maps_every_kind_variant() {
        fn draft(kind: SelfReviewProposalKind) -> SelfReviewProposalDraft {
            SelfReviewProposalDraft {
                id: "id-1".into(),
                kind,
                summary: "summary".into(),
                evidence: "evidence".into(),
            }
        }

        let cases = [
            (SelfReviewProposalKind::Memory, ProposalKind::Memory),
            (
                SelfReviewProposalKind::HarnessRule,
                ProposalKind::HarnessRule,
            ),
            (SelfReviewProposalKind::Workflow, ProposalKind::Workflow),
            (SelfReviewProposalKind::SkillTweak, ProposalKind::SkillTweak),
            (SelfReviewProposalKind::Skill, ProposalKind::Skill),
        ];
        for (input_kind, expected_kind) in cases {
            let proposal = self_review_draft_to_proposal(draft(input_kind));
            assert_eq!(proposal.kind, expected_kind);
            assert_eq!(proposal.status, ProposalStatus::Candidate);
            assert_eq!(proposal.score, 0.0);
            assert_eq!(proposal.id, "id-1");
            assert_eq!(proposal.summary, "summary");
            assert_eq!(proposal.evidence, "evidence");
        }
    }

    // -- harness.rs: extract_self_review_drafts ------------------------

    fn review_input(assistant_text: &str) -> SelfReviewInput {
        SelfReviewInput {
            session_id: "s-extract".into(),
            model: "haiku".into(),
            allowed_tools: vec!["learning_propose".into()],
            shell_allowed: false,
            stable_system_prompt_bytes: Vec::new(),
            stable_system_cache_key: None,
            transcript: Vec::new(),
            assistant_text: assistant_text.into(),
        }
    }

    #[test]
    fn extract_self_review_drafts_recognizes_all_marker_prefixes() {
        let input = review_input(
            "Remember: prefer the narrow fix over a rewrite\n\
             Learned: shell scripts need quoting on windows\n\
             Correction: the daemon port was wrong in the doc\n\
             This line has no marker and is ignored.",
        );
        let drafts = extract_self_review_drafts(&input);
        assert_eq!(drafts.len(), 3);
        assert!(drafts
            .iter()
            .any(|d| d.summary == "prefer the narrow fix over a rewrite"));
        assert!(drafts
            .iter()
            .any(|d| d.summary == "shell scripts need quoting on windows"));
        assert!(drafts
            .iter()
            .any(|d| d.summary == "the daemon port was wrong in the doc"));
        for d in &drafts {
            assert!(matches!(d.kind, SelfReviewProposalKind::Memory));
            assert!(d.evidence.contains("model=haiku"));
            assert!(d.evidence.contains("cache_key=no-cache-key"));
        }
    }

    #[test]
    fn extract_self_review_drafts_filters_short_summaries_and_empty_text() {
        assert!(extract_self_review_drafts(&review_input("")).is_empty());
        // "ok" is below the 8-character minimum after the marker.
        assert!(extract_self_review_drafts(&review_input("Remember: ok")).is_empty());
        assert!(extract_self_review_drafts(&review_input("no marker here at all")).is_empty());
    }

    // -- harness.rs: stable_review_proposal_id -------------------------

    #[test]
    fn stable_review_proposal_id_is_deterministic_and_input_sensitive() {
        let id_a = stable_review_proposal_id("session-1", "same summary");
        let id_b = stable_review_proposal_id("session-1", "same summary");
        assert_eq!(id_a, id_b, "same inputs must hash identically");
        assert!(id_a.starts_with("self-review-session-1-"));

        let id_c = stable_review_proposal_id("session-1", "different summary");
        assert_ne!(id_a, id_c, "different summaries must not collide");

        let id_d = stable_review_proposal_id("session-2", "same summary");
        assert_ne!(id_a, id_d, "different sessions must not collide");
    }

    // -- analyzer_source.rs: extract_text -------------------------------

    #[test]
    fn extract_text_falls_back_across_pointers_and_trims_empties() {
        let value = serde_json::json!({
            "first": "   ",
            "second": "  actual text  ",
        });
        assert_eq!(
            extract_text(&value, &["/missing", "/first", "/second"]),
            Some("actual text".to_string())
        );
        assert_eq!(extract_text(&value, &["/missing"]), None);
        // Whitespace-only text at a pointer is treated as absent, not "".
        assert_eq!(extract_text(&value, &["/first"]), None);
    }
}
