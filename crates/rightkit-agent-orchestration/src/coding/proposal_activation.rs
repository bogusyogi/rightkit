//! Proposal activation — promotes advisory ImprovementProposals from the taste engine
//! into actionable active proposals that influence routing and configuration.
//!
//! A proposal moves through states: Advisory → Approved → Active (influences routing)
//! or rejected. Only active proposals affect orchestration decisions.

use std::collections::HashMap;

/// State of a proposal in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalStatus {
    /// Initial state: the proposal is a suggestion from the taste engine, not yet approved.
    Advisory,
    /// Human or automated approval gate has signed off; ready to activate.
    Approved,
    /// Actively influencing routing/configuration decisions.
    Active,
    /// Rejected and will not be activated.
    Rejected,
}

/// An actionable proposal with a full state machine.
#[derive(Debug, Clone)]
pub struct ActivatableProposal {
    /// Unique proposal identifier.
    pub id: String,
    /// The kind of improvement (e.g., "ModelSwitch", "PromptStabilization").
    pub kind: String,
    /// JSON payload describing the proposal (model to switch to, prompt changes, etc).
    pub payload: String,
    /// Current state of the proposal.
    pub status: ProposalStatus,
    /// The role being improved (e.g., "coder", "conductor").
    pub role: String,
    /// Confidence score (0.0-1.0) from the taste engine.
    pub confidence: f64,
}

/// Manages a registry of activatable proposals and their state transitions.
pub struct ProposalActivator {
    /// All proposals keyed by id.
    proposals: HashMap<String, ActivatableProposal>,
}

impl Default for ProposalActivator {
    fn default() -> Self {
        Self::new()
    }
}

impl ProposalActivator {
    pub fn new() -> Self {
        Self {
            proposals: HashMap::new(),
        }
    }

    /// Register a new proposal in advisory state.
    pub fn register_advisory(
        &mut self,
        id: impl Into<String>,
        kind: impl Into<String>,
        role: impl Into<String>,
        payload: impl Into<String>,
        confidence: f64,
    ) -> &mut ActivatableProposal {
        let id = id.into();
        self.proposals.insert(
            id.clone(),
            ActivatableProposal {
                id: id.clone(),
                kind: kind.into(),
                role: role.into(),
                payload: payload.into(),
                status: ProposalStatus::Advisory,
                confidence,
            },
        );
        self.proposals.get_mut(&id).unwrap()
    }

    /// Transition a proposal to Approved state.
    /// Returns true if the transition was allowed, false if the proposal was not found or already rejected.
    pub fn approve(&mut self, id: &str) -> bool {
        if let Some(p) = self.proposals.get_mut(id) {
            if p.status != ProposalStatus::Rejected {
                p.status = ProposalStatus::Approved;
                return true;
            }
        }
        false
    }

    /// Transition an Approved proposal to Active state.
    /// Returns true if successful, false if proposal not found, not approved, or already rejected.
    pub fn activate(&mut self, id: &str) -> bool {
        if let Some(p) = self.proposals.get_mut(id) {
            if p.status == ProposalStatus::Approved {
                p.status = ProposalStatus::Active;
                return true;
            }
        }
        false
    }

    /// Transition a proposal to Rejected state.
    /// Can be called from any state.
    pub fn reject(&mut self, id: &str) -> bool {
        if let Some(p) = self.proposals.get_mut(id) {
            p.status = ProposalStatus::Rejected;
            return true;
        }
        false
    }

    /// Retrieve a proposal by id.
    pub fn get(&self, id: &str) -> Option<&ActivatableProposal> {
        self.proposals.get(id)
    }

    /// Get all active proposals.
    pub fn active(&self) -> Vec<&ActivatableProposal> {
        self.proposals
            .values()
            .filter(|p| p.status == ProposalStatus::Active)
            .collect()
    }

    /// Get all approved proposals (includes active).
    pub fn approved(&self) -> Vec<&ActivatableProposal> {
        self.proposals
            .values()
            .filter(|p| p.status == ProposalStatus::Approved || p.status == ProposalStatus::Active)
            .collect()
    }

    /// Get all proposals in advisory state.
    pub fn advisory(&self) -> Vec<&ActivatableProposal> {
        self.proposals
            .values()
            .filter(|p| p.status == ProposalStatus::Advisory)
            .collect()
    }

    /// Document how an active proposal influences routing/configuration.
    /// Returns a description of the effect.
    pub fn apply_to_routing(&self, proposal: &ActivatableProposal) -> String {
        match proposal.kind.as_str() {
            "ModelSwitch" => format!(
                "Route {} to a different model per payload: {}",
                proposal.role, proposal.payload
            ),
            "PromptStabilization" => format!(
                "Apply stabilized prompt to {} with payload: {}",
                proposal.role, proposal.payload
            ),
            "RoutingRestriction" => format!(
                "Restrict {} from long tasks per: {}",
                proposal.role, proposal.payload
            ),
            "ContextReset" => format!(
                "Enable context reset for {} per: {}",
                proposal.role, proposal.payload
            ),
            "JitMemory" => format!(
                "Switch {} to JIT memory per: {}",
                proposal.role, proposal.payload
            ),
            other => format!(
                "Unknown proposal kind '{}' for role {} with payload: {}",
                other, proposal.role, proposal.payload
            ),
        }
    }

    /// Get all proposals (any state).
    pub fn all(&self) -> Vec<&ActivatableProposal> {
        self.proposals.values().collect()
    }
}
