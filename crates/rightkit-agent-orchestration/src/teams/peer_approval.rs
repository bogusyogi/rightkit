//! Human approval capabilities for peer-to-peer team contact.
//!
//! An [`ApprovedPeer`] is intentionally unconstructible outside this module:
//! callers must obtain it from [`PeerApprovalGate::approve`]. Delegation also
//! checks the gate and the exact requester/target pair, so an approval for one
//! peer cannot be replayed against another.
//!
//! External-artifact gap: approvals are local in-memory attestations. This
//! module does not contact a human approval service or claim a remote identity.

use std::collections::BTreeMap;

use serde::Serialize;
use thiserror::Error;

use super::manifest_import::TeamManifest;

/// The approval record bound to one requester and one target peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PeerApproval {
    approval_id: String,
    team_id: String,
    requester_peer_id: String,
    target_peer_id: String,
}

/// An opaque capability proving that a peer pair was approved.
///
/// The private record prevents callers from manufacturing an approval token;
/// [`super::delegation::delegate`] accepts only tokens issued by the gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovedPeer {
    approval: PeerApproval,
}

/// In-memory approval authority for a validated team manifest.
#[derive(Debug, Clone, Default)]
pub struct PeerApprovalGate {
    approvals: BTreeMap<String, ApprovalKey>,
    next_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApprovalKey {
    team_id: String,
    requester_peer_id: String,
    target_peer_id: String,
}

/// Typed failures from the peer approval gate.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PeerApprovalError {
    /// The requested requester or target is absent from the validated fleet.
    #[error("peer `{peer_id}` is not present in team manifest")]
    UnknownPeer { peer_id: String },
    /// A peer cannot delegate to itself as a peer contact.
    #[error("peer `{peer_id}` cannot approve contact with itself")]
    SelfApproval { peer_id: String },
    /// The bounded approval id space was exhausted.
    #[error("peer approval id space exhausted")]
    IdExhausted,
}

impl PeerApprovalGate {
    /// Create an empty gate; no peer contact is approved initially.
    pub fn new() -> Self {
        Self::default()
    }

    /// Approve exactly one requester-to-target contact in the validated team.
    pub fn approve(
        &mut self,
        manifest: &TeamManifest,
        requester_peer_id: &str,
        target_peer_id: &str,
    ) -> Result<ApprovedPeer, PeerApprovalError> {
        if manifest.peer(requester_peer_id).is_none() {
            return Err(PeerApprovalError::UnknownPeer {
                peer_id: requester_peer_id.to_string(),
            });
        }
        if manifest.peer(target_peer_id).is_none() {
            return Err(PeerApprovalError::UnknownPeer {
                peer_id: target_peer_id.to_string(),
            });
        }
        if requester_peer_id == target_peer_id {
            return Err(PeerApprovalError::SelfApproval {
                peer_id: requester_peer_id.to_string(),
            });
        }

        let next_id = self
            .next_id
            .checked_add(1)
            .ok_or(PeerApprovalError::IdExhausted)?;
        self.next_id = next_id;
        let approval = PeerApproval {
            approval_id: format!("peer-approval-{next_id}"),
            team_id: manifest.team_id().to_string(),
            requester_peer_id: requester_peer_id.to_string(),
            target_peer_id: target_peer_id.to_string(),
        };
        self.approvals.insert(
            approval.approval_id.clone(),
            ApprovalKey {
                team_id: approval.team_id.clone(),
                requester_peer_id: approval.requester_peer_id.clone(),
                target_peer_id: approval.target_peer_id.clone(),
            },
        );
        Ok(ApprovedPeer { approval })
    }

    /// Revoke a previously issued approval before dispatch occurs.
    pub fn revoke(&mut self, approval: &ApprovedPeer) -> bool {
        self.approvals
            .remove(&approval.approval.approval_id)
            .is_some()
    }

    /// Return the durable id used to audit a successful delegation.
    pub fn approval_id(approval: &ApprovedPeer) -> &str {
        &approval.approval.approval_id
    }

    /// Check the opaque token against the live gate and exact delegation pair.
    pub(crate) fn authorizes(
        &self,
        approval: &ApprovedPeer,
        team_id: &str,
        requester_peer_id: &str,
        target_peer_id: &str,
    ) -> bool {
        let key = ApprovalKey {
            team_id: team_id.to_string(),
            requester_peer_id: requester_peer_id.to_string(),
            target_peer_id: target_peer_id.to_string(),
        };
        self.approvals
            .get(&approval.approval.approval_id)
            .is_some_and(|stored| stored == &key && approval.approval_matches(&key))
    }
}

impl ApprovedPeer {
    fn approval_matches(&self, key: &ApprovalKey) -> bool {
        self.approval.team_id == key.team_id
            && self.approval.requester_peer_id == key.requester_peer_id
            && self.approval.target_peer_id == key.target_peer_id
    }

    /// Return the approved requester id for audit/UI display.
    pub fn requester_peer_id(&self) -> &str {
        &self.approval.requester_peer_id
    }

    /// Return the approved target id for audit/UI display.
    pub fn target_peer_id(&self) -> &str {
        &self.approval.target_peer_id
    }

    /// Return the team id to which this capability is bound.
    pub fn team_id(&self) -> &str {
        &self.approval.team_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::teams::manifest_import::import_manifest;

    fn manifest() -> TeamManifest {
        import_manifest(
            r#"{"version":1,"team_id":"team-a","max_delegation_depth":2,"peers":[{"peer_id":"bot-a","role":"lead"},{"peer_id":"bot-b","role":"worker"}]}"#,
        )
        .unwrap()
    }

    #[test]
    fn approval_is_bound_to_the_exact_pair() {
        let mut gate = PeerApprovalGate::new();
        let approved = gate.approve(&manifest(), "bot-a", "bot-b").unwrap();
        assert!(gate.authorizes(&approved, "team-a", "bot-a", "bot-b"));
        assert!(!gate.authorizes(&approved, "team-a", "bot-b", "bot-a"));
        assert!(gate.revoke(&approved));
        assert!(!gate.authorizes(&approved, "team-a", "bot-a", "bot-b"));
    }
}
