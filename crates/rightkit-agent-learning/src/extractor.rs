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
