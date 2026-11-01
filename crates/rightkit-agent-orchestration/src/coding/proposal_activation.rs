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

#[cfg(test)]
mod tests {
    use super::*;

    fn proposal(id: &str, kind: &str, role: &str) -> (String, String, String, String, f64) {
        (
            id.to_string(),
            kind.to_string(),
            role.to_string(),
            "{}".to_string(),
            0.85,
        )
    }

    #[test]
    fn advisory_to_approved_to_active() {
        let mut pa = ProposalActivator::new();
        let (id, kind, role, payload, conf) = proposal("p1", "ModelSwitch", "coder");
        pa.register_advisory(&id, &kind, &role, &payload, conf);

        let p = pa.get(&id).unwrap();
        assert_eq!(p.status, ProposalStatus::Advisory);

        pa.approve(&id);
        let p = pa.get(&id).unwrap();
        assert_eq!(p.status, ProposalStatus::Approved);

        pa.activate(&id);
        let p = pa.get(&id).unwrap();
        assert_eq!(p.status, ProposalStatus::Active);
    }

    #[test]
    fn cannot_activate_if_not_approved() {
        let mut pa = ProposalActivator::new();
        pa.register_advisory("p1", "ModelSwitch", "coder", "{}", 0.85);

        assert!(!pa.activate("p1"));
        let p = pa.get("p1").unwrap();
        assert_eq!(p.status, ProposalStatus::Advisory);
    }

    #[test]
    fn reject_at_any_stage() {
        let mut pa = ProposalActivator::new();
        pa.register_advisory("p1", "ModelSwitch", "coder", "{}", 0.85);
        pa.approve("p1");

        assert!(pa.reject("p1"));
        let p = pa.get("p1").unwrap();
        assert_eq!(p.status, ProposalStatus::Rejected);
    }

    #[test]
    fn cannot_approve_rejected() {
        let mut pa = ProposalActivator::new();
        pa.register_advisory("p1", "ModelSwitch", "coder", "{}", 0.85);
        pa.reject("p1");

        assert!(!pa.approve("p1"));
        let p = pa.get("p1").unwrap();
        assert_eq!(p.status, ProposalStatus::Rejected);
    }

    #[test]
    fn active_returns_only_active() {
        let mut pa = ProposalActivator::new();
        pa.register_advisory("p1", "ModelSwitch", "coder", "{}", 0.85);
        pa.register_advisory("p2", "PromptStabilization", "auditor", "{}", 0.7);

        pa.approve("p1");
        pa.activate("p1");

        pa.approve("p2");

        let active = pa.active();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "p1");
    }

    #[test]
    fn advisory_returns_only_advisory() {
        let mut pa = ProposalActivator::new();
        pa.register_advisory("p1", "ModelSwitch", "coder", "{}", 0.85);
        pa.register_advisory("p2", "PromptStabilization", "auditor", "{}", 0.7);

        pa.approve("p1");

        let advisory = pa.advisory();
        assert_eq!(advisory.len(), 1);
        assert_eq!(advisory[0].id, "p2");
    }

    #[test]
    fn approved_includes_active() {
        let mut pa = ProposalActivator::new();
        pa.register_advisory("p1", "ModelSwitch", "coder", "{}", 0.85);
        pa.register_advisory("p2", "PromptStabilization", "auditor", "{}", 0.7);

        pa.approve("p1");
        pa.activate("p1");
        pa.approve("p2");

        let approved = pa.approved();
        assert_eq!(approved.len(), 2);
    }

    #[test]
    fn apply_to_routing_model_switch() {
        let pa = ProposalActivator::new();
        let p = ActivatableProposal {
            id: "p1".into(),
            kind: "ModelSwitch".into(),
            role: "coder".into(),
            payload: r#"{"to_model":"qwen3"}"#.into(),
            confidence: 0.85,
            status: ProposalStatus::Active,
        };

        let desc = pa.apply_to_routing(&p);
        assert!(desc.contains("coder"));
        assert!(desc.contains("qwen3"));
    }

    #[test]
    fn apply_to_routing_context_reset() {
        let pa = ProposalActivator::new();
        let p = ActivatableProposal {
            id: "p2".into(),
            kind: "ContextReset".into(),
            role: "auditor".into(),
            payload: "{}".into(),
            confidence: 0.75,
            status: ProposalStatus::Active,
        };

        let desc = pa.apply_to_routing(&p);
        assert!(desc.contains("auditor"));
        assert!(desc.contains("context reset"));
    }

    #[test]
    fn state_machine_reject_at_advisory() {
        let mut pa = ProposalActivator::new();
        pa.register_advisory("p1", "ModelSwitch", "coder", "{}", 0.85);

        assert!(pa.reject("p1"));
        assert!(!pa.approve("p1"));
        assert!(!pa.activate("p1"));

        let p = pa.get("p1").unwrap();
        assert_eq!(p.status, ProposalStatus::Rejected);
    }

    #[test]
    fn nonexistent_proposal() {
        let mut pa = ProposalActivator::new();
        assert!(!pa.approve("nonexistent"));
        assert!(!pa.activate("nonexistent"));
        assert!(!pa.reject("nonexistent"));
        assert!(pa.get("nonexistent").is_none());
    }
}
