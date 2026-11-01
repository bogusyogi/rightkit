//! Extraction of candidate proposals from a finished session.

use crate::types::{Proposal, ProposalKind, ProposalStatus};

/// Summary of a finished session, the input to extraction.
#[derive(Debug, Clone)]
pub struct SessionSummary {
    /// Identifier of the session.
    pub session_id: String,
    /// Whether the session completed successfully.
    pub succeeded: bool,
    /// Number of tool calls made during the session.
    pub tool_calls: u32,
    /// Error messages observed during the session.
    pub errors: Vec<String>,
    /// Notable decisions worth remembering.
    pub notable_decisions: Vec<String>,
}

/// Pulls candidate proposals from a [`SessionSummary`].
#[derive(Debug, Default, Clone)]
pub struct PostSessionLearningExtractor;

impl PostSessionLearningExtractor {
    /// Create a new extractor.
    pub fn new() -> Self {
        Self
    }

    /// Extract candidate proposals from a session summary.
    pub fn extract(&self, summary: &SessionSummary) -> Vec<Proposal> {
        let mut proposals = Vec::new();
        let mut next_id = 0usize;
        let mut mk_id = || {
            let id = format!("prop-{next_id}");
            next_id += 1;
            id
        };

        // One Memory proposal per notable decision.
        for decision in &summary.notable_decisions {
            proposals.push(Proposal {
                id: mk_id(),
                kind: ProposalKind::Memory,
                summary: decision.clone(),
                evidence: format!("session {} decision", summary.session_id),
                score: 0.0,
                status: ProposalStatus::Candidate,
                created_at: String::new(),
                age_sessions: 0,
                rejection_reason: None,
            });
        }

        // One HarnessRule per distinct error (dedup by string, preserve order).
        let mut seen: Vec<&String> = Vec::new();
        for error in &summary.errors {
            if seen.contains(&error) {
                continue;
            }
            seen.push(error);
            proposals.push(Proposal {
                id: mk_id(),
                kind: ProposalKind::HarnessRule,
                summary: format!("avoid: {error}"),
                evidence: error.clone(),
                score: 0.0,
                status: ProposalStatus::Candidate,
                created_at: String::new(),
                age_sessions: 0,
                rejection_reason: None,
            });
        }

        // One Workflow proposal if the session succeeded and was busy.
        if summary.succeeded && summary.tool_calls > 10 {
            proposals.push(Proposal {
                id: mk_id(),
                kind: ProposalKind::Workflow,
                summary: "efficient multi-step workflow".to_string(),
                evidence: format!("{} tool calls", summary.tool_calls),
                score: 0.0,
                status: ProposalStatus::Candidate,
                created_at: String::new(),
                age_sessions: 0,
                rejection_reason: None,
            });
        }

        // Skill synthesis: a successful, genuinely complex run with notable
        // decisions is worth distilling into a *new* reusable skill (the
        // generative direction — success → skill). Advisory only: it surfaces as
        // a candidate for the operator to turn into a real skill, never written
        // automatically.
        if summary.succeeded && summary.tool_calls > 10 && !summary.notable_decisions.is_empty() {
            let approach = summary.notable_decisions.join("; ");
            proposals.push(Proposal {
                id: mk_id(),
                kind: ProposalKind::Skill,
                summary: format!("skill candidate: {}", truncate(&approach, 80)),
                evidence: format!(
                    "synthesized from a successful {}-step run. Approach: {approach}",
                    summary.tool_calls
                ),
                score: 0.0,
                status: ProposalStatus::Candidate,
                created_at: String::new(),
                age_sessions: 0,
                rejection_reason: None,
            });
        }

        proposals
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary() -> SessionSummary {
        SessionSummary {
            session_id: "s1".to_string(),
            succeeded: true,
            tool_calls: 0,
            errors: vec![],
            notable_decisions: vec![],
        }
    }

    #[test]
    fn extract_memory_from_decisions() {
        let mut s = summary();
        s.notable_decisions = vec!["use trait objects".to_string(), "cache results".to_string()];
        let props = PostSessionLearningExtractor::new().extract(&s);
        let mem: Vec<_> = props
            .iter()
            .filter(|p| p.kind == ProposalKind::Memory)
            .collect();
        assert_eq!(mem.len(), 2);
        assert_eq!(mem[0].summary, "use trait objects");
        assert_eq!(mem[0].status, ProposalStatus::Candidate);
        assert_eq!(mem[0].score, 0.0);
        assert_eq!(mem[0].age_sessions, 0);
    }

    #[test]
    fn extract_harness_rule_from_errors() {
        let mut s = summary();
        s.errors = vec!["nil deref".to_string()];
        let props = PostSessionLearningExtractor::new().extract(&s);
        let rule = props
            .iter()
            .find(|p| p.kind == ProposalKind::HarnessRule)
            .unwrap();
        assert_eq!(rule.summary, "avoid: nil deref");
    }

    #[test]
    fn extract_dedups_errors() {
        let mut s = summary();
        s.errors = vec!["boom".to_string(), "boom".to_string(), "bang".to_string()];
        let props = PostSessionLearningExtractor::new().extract(&s);
        let rules: Vec<_> = props
            .iter()
            .filter(|p| p.kind == ProposalKind::HarnessRule)
            .collect();
        assert_eq!(rules.len(), 2);
    }

    #[test]
    fn extract_workflow_when_successful_and_busy() {
        let mut s = summary();
        s.succeeded = true;
        s.tool_calls = 11;
        let props = PostSessionLearningExtractor::new().extract(&s);
        assert!(props.iter().any(|p| p.kind == ProposalKind::Workflow));
    }

    #[test]
    fn extract_no_workflow_when_few_tools() {
        let mut s = summary();
        s.succeeded = true;
        s.tool_calls = 5;
        let props = PostSessionLearningExtractor::new().extract(&s);
        assert!(!props.iter().any(|p| p.kind == ProposalKind::Workflow));
    }

    #[test]
    fn extract_empty_summary_empty() {
        let s = summary();
        let props = PostSessionLearningExtractor::new().extract(&s);
        assert!(props.is_empty());
    }

    #[test]
    fn synthesizes_skill_from_complex_success() {
        let mut s = summary();
        s.succeeded = true;
        s.tool_calls = 14;
        s.notable_decisions = vec!["repro first, then fix".to_string()];
        let props = PostSessionLearningExtractor::new().extract(&s);
        let skill = props
            .iter()
            .find(|p| p.kind == ProposalKind::Skill)
            .unwrap();
        assert!(skill.summary.starts_with("skill candidate:"));
        assert!(skill.evidence.contains("repro first"));
    }

    #[test]
    fn no_skill_without_decisions_or_when_trivial() {
        // Complex but no decisions → no skill.
        let mut s = summary();
        s.succeeded = true;
        s.tool_calls = 20;
        assert!(!PostSessionLearningExtractor::new()
            .extract(&s)
            .iter()
            .any(|p| p.kind == ProposalKind::Skill));
        // Has decisions but trivial (few tool calls) → no skill.
        let mut t = summary();
        t.succeeded = true;
        t.tool_calls = 3;
        t.notable_decisions = vec!["did a thing".to_string()];
        assert!(!PostSessionLearningExtractor::new()
            .extract(&t)
            .iter()
            .any(|p| p.kind == ProposalKind::Skill));
    }
}
