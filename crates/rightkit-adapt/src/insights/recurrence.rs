//! Deterministic recurrence: episode → issue formation (canon §6.3).
//!
//! Issues form ONLY from recurring episodes of the same family + signature.
//! Hybrid model-proposed merges are proposals until deterministically
//! verified; they NEVER auto-admit and never overwrite a deterministic
//! grouping. Issue state transitions are explicit and validated.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::{InsightIssueV1, IssueState, TranscriptEventV1};
use crate::insights::detectors::run_all_detectors;
use crate::model_boundary::ModelProposalError;

/// Grouping key: family + signature. Deterministic and order-independent.
pub fn grouping_key(family: &str, signature: &str) -> String {
    format!("{family}\u{1f}{signature}")
}

/// Form issues from episodes. Recurring (>= `min_recurrence` episodes with
/// distinct evidence) groups become issues in `Observed`; singles stay
/// episodes and produce no issue.
pub fn form_issues(
    episodes: &[crate::insights::FailureEpisodeV1],
    min_recurrence: u32,
) -> Vec<InsightIssueV1> {
    let mut groups: BTreeMap<String, Vec<&crate::insights::FailureEpisodeV1>> = BTreeMap::new();
    for ep in episodes {
        groups
            .entry(grouping_key(&ep.family, &ep.signature))
            .or_default()
            .push(ep);
    }
    let mut out = Vec::new();
    for (_key, members) in groups {
        let mut unique_by_id = BTreeMap::new();
        for member in members {
            unique_by_id
                .entry(member.episode_id.as_str())
                .or_insert(member);
        }
        let members: Vec<_> = unique_by_id.into_values().collect();
        let count = members.len() as u32;
        if count < min_recurrence.max(2) {
            continue;
        }
        let first = members[0];
        let sessions: Vec<String> = {
            let mut s: Vec<String> = members
                .iter()
                .flat_map(|m| m.sessions.iter().map(|session| session.trim().to_owned()))
                .filter(|session| !session.is_empty())
                .collect();
            s.sort();
            s.dedup();
            s
        };
        if sessions.len() < 2 {
            continue;
        }
        let ids: Vec<String> = members.iter().map(|m| m.episode_id.clone()).collect();
        let timestamps: Vec<String> = members.iter().filter_map(|m| m.timestamp.clone()).collect();
        out.push(InsightIssueV1 {
            schema_version: crate::insights::INSIGHT_ISSUE_SCHEMA.to_string(),
            issue_id: crate::canonical::derive_issue_id(&first.family, &first.signature),
            family: first.family.clone(),
            recurrence_signature: first.signature.clone(),
            canonical_description: first.observed_failure.clone(),
            applicability: applicability_of(&members),
            episode_ids: ids,
            recurrence_count: count,
            distinct_sessions: sessions.len() as u32,
            first_seen: timestamps.iter().min().cloned(),
            last_seen: timestamps.iter().max().cloned(),
            confidence: members.iter().map(|m| m.confidence).fold(0.0_f64, f64::min),
            state: IssueState::Observed,
            candidate_mechanisms: members
                .iter()
                .filter(|m| !m.likely_mechanism.is_empty())
                .map(|m| m.likely_mechanism.clone())
                .collect(),
            mitigation_links: vec![],
            recurrence_after_mitigation: 0,
            honesty_limit: crate::insights::HONESTY_LIMIT.to_string(),
        });
    }
    out
}

fn applicability_of(members: &[&crate::insights::FailureEpisodeV1]) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    let hosts: Vec<String> = members
        .iter()
        .flat_map(|e| e.hosts.iter().cloned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if hosts.len() == 1 {
        m.insert("host".to_string(), hosts[0].clone());
    }
    m
}

/// Convenience pipeline over raw events.
pub fn mine_issues(events: &[TranscriptEventV1], min_recurrence: u32) -> Vec<InsightIssueV1> {
    form_issues(&run_all_detectors(events), min_recurrence)
}

/// A hybrid model-proposed merge of episodes into an existing issue (or a new
/// one). This is a PROPOSAL object only; nothing here writes durable truth.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridMergeProposal {
    pub proposer_id: String,
    pub target_issue_id: Option<String>,
    /// Episode IDs the model claims belong together.
    pub episode_ids: Vec<String>,
    pub rationale: String,
}

impl HybridMergeProposal {
    /// Deterministic verification: every referenced episode must exist and
    /// share one family+signature key. A verified proposal is still only a
    /// validated plan — Cortex admission stays a separate gate.
    pub fn verify(
        &self,
        episodes: &[crate::insights::FailureEpisodeV1],
        issues: &[InsightIssueV1],
    ) -> Result<Vec<String>, ModelProposalError> {
        let by_id: BTreeMap<&str, &crate::insights::FailureEpisodeV1> = episodes
            .iter()
            .map(|e| (e.episode_id.as_str(), e))
            .collect();
        if self.episode_ids.is_empty() {
            return Err(ModelProposalError::UnboundEvidence);
        }
        let mut keys = std::collections::BTreeSet::new();
        for id in &self.episode_ids {
            let Some(ep) = by_id.get(id.as_str()) else {
                return Err(ModelProposalError::UnboundEvidence);
            };
            keys.insert(grouping_key(&ep.family, &ep.signature));
        }
        if keys.len() != 1 {
            return Err(ModelProposalError::ScopeBeyondEvidence);
        }
        if let Some(target) = &self.target_issue_id {
            if !issues.iter().any(|i| i.issue_id == *target) {
                return Err(ModelProposalError::UnboundEvidence);
            }
            if issues
                .iter()
                .any(|i| i.issue_id == *target && i.state == IssueState::Dismissed)
            {
                return Err(ModelProposalError::AuthorityEscalationAttempted);
            }
        }
        Ok(self.episode_ids.clone())
    }
}

/// Apply a validated issue-state transition. Returns the updated issue or an
/// error naming the illegal transition. Never bypasses `can_transition_to`.
pub fn transition_issue(
    issue: &InsightIssueV1,
    target: IssueState,
) -> Result<InsightIssueV1, String> {
    if !issue.state.can_transition_to(target) {
        return Err(format!(
            "illegal issue transition {:?} -> {:?}",
            issue.state, target
        ));
    }
    let mut next = issue.clone();
    next.state = target;
    Ok(next)
}

/// Record recurrence after mitigation; drives reopen logic in `outcomes`.
pub fn record_post_mitigation_recurrence(
    mut issue: InsightIssueV1,
) -> Result<InsightIssueV1, String> {
    if issue.state != IssueState::Mitigated {
        return Err("recurrence-after-mitigation requires Mitigated state".into());
    }
    issue.recurrence_after_mitigation += 1;
    transition_issue(&issue, IssueState::Reopened)
}

/// A host outcome joined to one exact mitigation/version.  Exposure is kept
/// on each observation so a zero-opportunity window cannot look successful.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MitigationOutcomeV1 {
    pub outcome_id: String,
    pub issue_id: String,
    pub mitigation_proposal_id: String,
    pub mitigation_version: String,
    pub baseline_version: String,
    pub exposure: crate::outcomes::Exposure,
    pub raw: crate::outcomes::RawOutcome,
    pub applicability: crate::attribution::EvaluatorApplicability,
    #[serde(default)]
    pub observed_at: Option<String>,
}

impl MitigationOutcomeV1 {
    pub fn is_usable_exposure(&self) -> bool {
        self.exposure.opportunities > 0 && self.exposure.baseline > 0
    }

    pub fn is_reopen_signal(&self) -> bool {
        self.applicability == crate::attribution::EvaluatorApplicability::Applicable
            && self.raw == crate::outcomes::RawOutcome::RecurredSameSignature
            && self.is_usable_exposure()
    }
}

/// Idempotent, order-independent recurrence projection.  Replayed outcome
/// IDs are no-ops; conflicting reuse of an ID is refused.  Latest outcome is
/// selected by host timestamp then ID, not arrival order.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecurrenceOutcomeLedgerV1 {
    outcomes: BTreeMap<String, MitigationOutcomeV1>,
}

impl RecurrenceOutcomeLedgerV1 {
    pub fn record(&mut self, outcome: MitigationOutcomeV1) -> Result<(), String> {
        if outcome.outcome_id.trim().is_empty()
            || outcome.issue_id.trim().is_empty()
            || outcome.mitigation_proposal_id.trim().is_empty()
            || outcome.mitigation_version.trim().is_empty()
            || outcome.baseline_version.trim().is_empty()
        {
            return Err("mitigation outcome identity is incomplete".into());
        }
        if let Some(existing) = self.outcomes.get(&outcome.outcome_id) {
            return if existing == &outcome {
                Ok(())
            } else {
                Err("outcome replay conflicts with prior identity".into())
            };
        }
        self.outcomes.insert(outcome.outcome_id.clone(), outcome);
        Ok(())
    }

    pub fn replay<I>(&mut self, outcomes: I) -> Result<(), String>
    where
        I: IntoIterator<Item = MitigationOutcomeV1>,
    {
        for outcome in outcomes {
            self.record(outcome)?;
        }
        Ok(())
    }

    pub fn outcomes_for(
        &self,
        issue_id: &str,
        mitigation_version: &str,
    ) -> Vec<&MitigationOutcomeV1> {
        let mut result: Vec<_> = self
            .outcomes
            .values()
            .filter(|outcome| {
                outcome.issue_id == issue_id && outcome.mitigation_version == mitigation_version
            })
            .collect();
        result.sort_by(|left, right| {
            left.observed_at
                .cmp(&right.observed_at)
                .then_with(|| left.outcome_id.cmp(&right.outcome_id))
        });
        result
    }

    pub fn outcomes_for_baseline(
        &self,
        issue_id: &str,
        mitigation_version: &str,
        baseline_version: &str,
    ) -> Vec<&MitigationOutcomeV1> {
        self.outcomes_for(issue_id, mitigation_version)
            .into_iter()
            .filter(|outcome| outcome.baseline_version == baseline_version)
            .collect()
    }

    /// Reopen iff the latest applicable, sufficiently exposed outcome for the
    /// exact mitigation version is same-signature recurrence. Incomplete,
    /// not-applicable, and insufficient observations never become failures.
    pub fn should_reopen(&self, issue_id: &str, mitigation_version: &str) -> bool {
        self.outcomes_for(issue_id, mitigation_version)
            .into_iter()
            .next_back()
            .is_some_and(|outcome| outcome.is_reopen_signal())
    }

    pub fn should_reopen_for_baseline(
        &self,
        issue_id: &str,
        mitigation_version: &str,
        baseline_version: &str,
    ) -> bool {
        self.outcomes_for_baseline(issue_id, mitigation_version, baseline_version)
            .into_iter()
            .next_back()
            .is_some_and(|outcome| outcome.is_reopen_signal())
    }

    pub fn recurrence_count(&self, issue_id: &str, mitigation_version: &str) -> u32 {
        self.outcomes_for(issue_id, mitigation_version)
            .into_iter()
            .filter(|outcome| outcome.is_reopen_signal())
            .count() as u32
    }
}

/// Apply latest outcome semantics to an issue without mutating the ledger.
pub fn apply_mitigation_outcome(
    issue: InsightIssueV1,
    ledger: &RecurrenceOutcomeLedgerV1,
    mitigation_version: &str,
) -> Result<InsightIssueV1, String> {
    if !ledger.should_reopen(&issue.issue_id, mitigation_version) {
        return Ok(issue);
    }
    if issue.state != IssueState::Mitigated {
        return Err("recurrence outcome requires Mitigated state".into());
    }
    let mut next = issue;
    next.recurrence_after_mitigation = ledger.recurrence_count(&next.issue_id, mitigation_version);
    transition_issue(&next, IssueState::Reopened)
}
