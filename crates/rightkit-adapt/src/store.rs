//! In-memory proposal store and lifecycle management.

use crate::error::LearningError;
use crate::types::{Proposal, ProposalStatus};

/// Minimum evaluated score required before a proposal may be promoted. Matches
/// `EvaluationConfig::default().promote_threshold`.
const PROMOTE_THRESHOLD: f64 = 0.6;

/// An in-memory collection of proposals with lifecycle operations.
#[derive(Debug, Default, Clone)]
pub struct LearningProposalStore {
    proposals: Vec<Proposal>,
}

impl LearningProposalStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self {
            proposals: Vec::new(),
        }
    }

    /// Add a proposal to the store.
    pub fn add(&mut self, proposal: Proposal) {
        self.proposals.push(proposal);
    }

    /// Look up a proposal by id.
    pub fn get(&self, id: &str) -> Option<&Proposal> {
        self.proposals.iter().find(|p| p.id == id)
    }

    /// Number of proposals stored.
    pub fn len(&self) -> usize {
        self.proposals.len()
    }

    /// Whether the store holds no proposals (clippy companion to `len`).
    pub fn is_empty(&self) -> bool {
        self.proposals.is_empty()
    }

    /// All proposals, by reference.
    pub fn all(&self) -> Vec<&Proposal> {
        self.proposals.iter().collect()
    }

    /// All proposals with the given status.
    pub fn by_status(&self, status: ProposalStatus) -> Vec<&Proposal> {
        self.proposals
            .iter()
            .filter(|p| p.status == status)
            .collect()
    }

    fn set_status(&mut self, id: &str, status: ProposalStatus) -> Result<(), LearningError> {
        match self.proposals.iter_mut().find(|p| p.id == id) {
            Some(p) => {
                p.status = status;
                Ok(())
            }
            None => Err(LearningError::NotFound(id.to_string())),
        }
    }

    /// Mark a proposal as `Promoted`, but only if it has been evaluated and
    /// cleared the promotion threshold.
    pub fn promote(&mut self, id: &str) -> Result<(), LearningError> {
        let p = self
            .proposals
            .iter()
            .find(|p| p.id == id)
            .ok_or_else(|| LearningError::NotFound(id.to_string()))?;
        if p.status != ProposalStatus::Evaluated || p.score < PROMOTE_THRESHOLD {
            return Err(LearningError::NotEligible(format!(
                "proposal {id} is not promotable (status={:?}, score={:.3}); evaluate it first and clear the {PROMOTE_THRESHOLD} threshold",
                p.status, p.score
            )));
        }
        self.set_status(id, ProposalStatus::Promoted)
    }

    /// Mark a proposal as `Rejected`, recording why.
    pub fn reject(&mut self, id: &str, reason: Option<String>) -> Result<(), LearningError> {
        match self.proposals.iter_mut().find(|p| p.id == id) {
            Some(p) => {
                p.status = ProposalStatus::Rejected;
                p.rejection_reason = reason;
                Ok(())
            }
            None => Err(LearningError::NotFound(id.to_string())),
        }
    }

    /// Increment `age_sessions` on every non-terminal proposal.
    pub fn age_all(&mut self) {
        for p in &mut self.proposals {
            if matches!(
                p.status,
                ProposalStatus::Candidate | ProposalStatus::Evaluated
            ) {
                p.age_sessions += 1;
            }
        }
    }

    /// Mark non-terminal proposals older than `max_age` as `Expired`.
    pub fn forget_expired(&mut self, max_age: u32) -> usize {
        let mut count = 0;
        for p in &mut self.proposals {
            let eligible = matches!(
                p.status,
                ProposalStatus::Candidate | ProposalStatus::Evaluated
            );
            if eligible && p.age_sessions > max_age {
                p.status = ProposalStatus::Expired;
                count += 1;
            }
        }
        count
    }
}
