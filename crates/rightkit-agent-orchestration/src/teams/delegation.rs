//! Approval-gated, bounded peer delegation.
//!
//! [`delegate`] admits a delegation only when its requester, target, team,
//! depth, and task match a validated manifest and a live
//! [`PeerApprovalGate`]. The approval token is required at the call boundary
//! and the gate is checked again, making omission, pair substitution, and
//! revocation fail closed rather than relying on caller convention.
//!
//! `TeamRuntime` turns an admitted record into durable point-to-point mailbox
//! transport. A host supplies its real peer executor at the final boundary;
//! transport alone never fabricates a completion.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::manifest_import::TeamManifest;
use super::peer_approval::{ApprovedPeer, PeerApprovalGate};
use super::section_context::SectionContext;
use super::store::{TeamCoordinationError, TeamCoordinationStore};

const MAX_TASK_BYTES: usize = 64 * 1024;

/// A requested unit of work addressed to one named peer, with explicit team
/// and depth fields so admission can reject cross-team or unbounded work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationRequest {
    /// The validated team whose manifest governs this request.
    pub team_id: String,
    /// The peer asking another peer to work.
    pub requester_peer_id: String,
    /// The peer that should receive the work.
    pub target_peer_id: String,
    /// The bounded task payload to hand to the peer runtime.
    pub task: String,
    /// One-based depth in the peer delegation chain.
    pub depth: u8,
}

/// State of a locally admitted delegation record, kept distinct from remote
/// completion so local approval cannot be reported as execution evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DelegationState {
    /// The request passed all local gates but has not been acknowledged by a
    /// remote executor.
    Accepted,
}

/// A delegation record that carries the approval id used to admit it, so a
/// later executor can audit the exact human-gate decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerDelegation {
    request: DelegationRequest,
    approval_id: String,
    state: DelegationState,
}

/// Typed failures from the peer delegation admission gate.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DelegationError {
    /// The request named a different team than the imported manifest.
    #[error("delegation team `{requested}` does not match manifest team `{manifest}`")]
    TeamMismatch { requested: String, manifest: String },
    /// The requester or target was not admitted by the imported manifest.
    #[error("peer `{peer_id}` is not present in the team manifest")]
    UnknownPeer { peer_id: String },
    /// The request attempted to contact itself.
    #[error("peer `{peer_id}` cannot delegate to itself")]
    SelfDelegation { peer_id: String },
    /// No approval capability was supplied, so no peer contact may occur.
    #[error("peer delegation requires an approved peer-contact capability")]
    ApprovalRequired,
    /// The supplied approval was for a different requester/target/team pair,
    /// or had been revoked from the live gate.
    #[error("peer approval does not authorize this delegation")]
    ApprovalMismatch,
    /// The chain depth would exceed the imported fleet's hard ceiling.
    #[error("delegation depth {depth} exceeds manifest maximum {maximum}")]
    DepthExceeded { depth: u8, maximum: u8 },
    /// Empty or oversized task text is rejected rather than silently bounded.
    #[error("delegation task must be non-empty and at most {maximum} bytes")]
    InvalidTask { maximum: usize },
}

/// One delegation durably placed on its target peer's mailbox. This is a
/// transport receipt, not claimed completion: target execution must consume
/// and acknowledge this exact message later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerDispatch {
    pub delegation: PeerDelegation,
    pub message_id: String,
}

/// A received peer task that can be executed only by its manifest-named
/// target. Shared user context is exposed separately as an immutable prompt
/// fragment; no peer-facing API writes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceivedPeerDelegation {
    pub message_id: String,
    pub delegation: PeerDelegation,
}

/// Runtime supplied by a manifest-named peer. The transport invokes this
/// boundary with a read-only user brief; only this implementation may decide
/// whether task execution succeeded.
pub trait PeerTaskExecutor: Send + Sync {
    fn execute(
        &self,
        request: &DelegationRequest,
        shared_context: Option<&str>,
    ) -> Result<serde_json::Value, String>;
}

/// Durable result of invoking one target peer executor. `success: false`
/// records an executor failure; it never claims a synthetic peer response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerExecutionResult {
    pub message_id: String,
    pub success: bool,
    pub detail: serde_json::Value,
}

/// Live local peer-runtime boundary: named manifests persist in the same
/// SQLite store used for task transport, every contact passes an explicit
/// human approval capability, and a target receives its work through a
/// durable point-to-point mailbox rather than a simulated success value.
#[derive(Debug, Clone)]
pub struct TeamRuntime {
    manifest: TeamManifest,
    approval_gate: PeerApprovalGate,
    store: TeamCoordinationStore,
    shared_context: SectionContext,
}

/// Errors from durable peer transport after local delegation admission.
#[derive(Debug, Error)]
pub enum TeamRuntimeError {
    #[error(transparent)]
    Delegation(#[from] DelegationError),
    #[error(transparent)]
    Store(#[from] TeamCoordinationError),
    #[error("mailbox message `{message_id}` is not a valid peer delegation")]
    InvalidTransportMessage { message_id: String },
    #[error("peer `{peer_id}` is not present in team `{team_id}")]
    UnknownPeer { team_id: String, peer_id: String },
    #[error("peer `{peer_id}` cannot acknowledge work addressed to `{target_peer_id}")]
    WrongRecipient {
        peer_id: String,
        target_peer_id: String,
    },
}

/// Admit one peer delegation after validating every untrusted request field
/// and checking the exact approval capability against the live gate.
pub fn delegate(
    manifest: &TeamManifest,
    approval_gate: &PeerApprovalGate,
    approval: Option<&ApprovedPeer>,
    request: DelegationRequest,
) -> Result<PeerDelegation, DelegationError> {
    if request.team_id != manifest.team_id() {
        return Err(DelegationError::TeamMismatch {
            requested: request.team_id,
            manifest: manifest.team_id().to_string(),
        });
    }
    if manifest.peer(&request.requester_peer_id).is_none() {
        return Err(DelegationError::UnknownPeer {
            peer_id: request.requester_peer_id,
        });
    }
    if manifest.peer(&request.target_peer_id).is_none() {
        return Err(DelegationError::UnknownPeer {
            peer_id: request.target_peer_id,
        });
    }
    if request.requester_peer_id == request.target_peer_id {
        return Err(DelegationError::SelfDelegation {
            peer_id: request.requester_peer_id,
        });
    }
    if request.task.is_empty() || request.task.len() > MAX_TASK_BYTES {
        return Err(DelegationError::InvalidTask {
            maximum: MAX_TASK_BYTES,
        });
    }
    if request.depth == 0 || request.depth > manifest.max_delegation_depth() {
        return Err(DelegationError::DepthExceeded {
            depth: request.depth,
            maximum: manifest.max_delegation_depth(),
        });
    }

    let Some(approval) = approval else {
        return Err(DelegationError::ApprovalRequired);
    };
    if !approval_gate.authorizes(
        approval,
        manifest.team_id(),
        &request.requester_peer_id,
        &request.target_peer_id,
    ) {
        return Err(DelegationError::ApprovalMismatch);
    }

    Ok(PeerDelegation {
        request,
        approval_id: PeerApprovalGate::approval_id(approval).to_string(),
        state: DelegationState::Accepted,
    })
}

impl PeerDelegation {
    /// Return the validated request carried by this admitted delegation.
    pub fn request(&self) -> &DelegationRequest {
        &self.request
    }

    /// Return the approval id that authorized admission.
    pub fn approval_id(&self) -> &str {
        &self.approval_id
    }

    /// Return the local state without implying remote completion.
    pub fn state(&self) -> &DelegationState {
        &self.state
    }
}

impl TeamRuntime {
    /// Create a runtime from a validated manifest, saving that exact fleet so
    /// a later process can reconstruct identical peer identities.
    pub fn new(
        manifest: TeamManifest,
        store: TeamCoordinationStore,
        shared_context: SectionContext,
    ) -> Result<Self, TeamRuntimeError> {
        store.save_manifest(&manifest)?;
        Ok(Self {
            manifest,
            approval_gate: PeerApprovalGate::new(),
            store,
            shared_context,
        })
    }

    pub fn manifest(&self) -> &TeamManifest {
        &self.manifest
    }

    /// Explicit user approval is minted only for one requester/target pair.
    pub fn approve_peer_contact(
        &mut self,
        requester_peer_id: &str,
        target_peer_id: &str,
    ) -> Result<ApprovedPeer, super::peer_approval::PeerApprovalError> {
        self.approval_gate
            .approve(&self.manifest, requester_peer_id, target_peer_id)
    }

    /// Read-only shared user brief injected by a peer executor into its own
    /// prompt. There is deliberately no peer write counterpart.
    pub fn shared_context_prompt(&self) -> Option<String> {
        self.shared_context.prompt_fragment()
    }

    /// Admit one approved peer contact then durably transmit it to target's
    /// point-to-point mailbox. `delegate` validates team identity, named
    /// peers, depth, task size, and live approval before this method writes.
    pub fn dispatch(
        &self,
        approval: Option<&ApprovedPeer>,
        request: DelegationRequest,
    ) -> Result<PeerDispatch, TeamRuntimeError> {
        let delegation = delegate(&self.manifest, &self.approval_gate, approval, request)?;
        let request = delegation.request();
        let body = serde_json::json!({
            "kind": "peer_delegation",
            "delegation": &delegation,
        });
        let message = self.store.mailbox_send(
            self.manifest.team_id(),
            &request.requester_peer_id,
            Some(&request.target_peer_id),
            body,
        )?;
        self.store.missionlog_append(
            self.manifest.team_id(),
            &request.requester_peer_id,
            "peer_delegation_dispatched",
            serde_json::json!({
                "messageId": &message.message_id,
                "targetPeerId": &request.target_peer_id,
                "approvalId": delegation.approval_id(),
                "depth": request.depth,
            }),
        )?;
        Ok(PeerDispatch {
            delegation,
            message_id: message.message_id,
        })
    }

    /// Receive outstanding direct work for one manifest-named target. Invalid
    /// mailbox content fails closed instead of being executed as a task.
    pub fn receive_for(
        &self,
        peer_id: &str,
    ) -> Result<Vec<ReceivedPeerDelegation>, TeamRuntimeError> {
        if self.manifest.peer(peer_id).is_none() {
            return Err(TeamRuntimeError::UnknownPeer {
                team_id: self.manifest.team_id().to_string(),
                peer_id: peer_id.to_string(),
            });
        }
        self.store
            .mailbox_peek(self.manifest.team_id(), peer_id)?
            .into_iter()
            .filter(|message| message.to_peer.as_deref() == Some(peer_id))
            .map(|message| {
                let delegation: PeerDelegation = message
                    .body
                    .get("delegation")
                    .filter(|_| {
                        message.body.get("kind").and_then(serde_json::Value::as_str)
                            == Some("peer_delegation")
                    })
                    .cloned()
                    .and_then(|value| serde_json::from_value(value).ok())
                    .ok_or_else(|| TeamRuntimeError::InvalidTransportMessage {
                        message_id: message.message_id.clone(),
                    })?;
                let request = delegation.request();
                if request.team_id != self.manifest.team_id()
                    || request.requester_peer_id == request.target_peer_id
                    || request.target_peer_id != peer_id
                    || request.task.is_empty()
                    || request.task.len() > MAX_TASK_BYTES
                    || request.depth == 0
                    || request.depth > self.manifest.max_delegation_depth()
                    || self.manifest.peer(&request.requester_peer_id).is_none()
                    || self.manifest.peer(&request.target_peer_id).is_none()
                {
                    return Err(TeamRuntimeError::InvalidTransportMessage {
                        message_id: message.message_id,
                    });
                }
                Ok(ReceivedPeerDelegation {
                    message_id: message.message_id,
                    delegation,
                })
            })
            .collect()
    }

    /// Acknowledge a target's terminal execution result, consuming only its
    /// own direct mailbox message and leaving durable audit evidence.
    pub fn acknowledge(
        &self,
        peer_id: &str,
        received: &ReceivedPeerDelegation,
        succeeded: bool,
        detail: serde_json::Value,
    ) -> Result<(), TeamRuntimeError> {
        let target_peer_id = &received.delegation.request().target_peer_id;
        if target_peer_id != peer_id {
            return Err(TeamRuntimeError::WrongRecipient {
                peer_id: peer_id.to_string(),
                target_peer_id: target_peer_id.clone(),
            });
        }
        self.store
            .mailbox_ack_for_peer(self.manifest.team_id(), &received.message_id, peer_id)?;
        self.store.missionlog_append(
            self.manifest.team_id(),
            peer_id,
            if succeeded {
                "peer_delegation_completed"
            } else {
                "peer_delegation_failed"
            },
            serde_json::json!({
                "messageId": &received.message_id,
                "approvalId": received.delegation.approval_id(),
                "detail": detail,
            }),
        )?;
        Ok(())
    }

    /// Run one queued direct delegation through caller-provided peer runtime,
    /// then durably acknowledge its actual result. An empty mailbox returns
    /// `None`; execution errors are recorded as failed work rather than
    /// represented as peer success.
    pub fn execute_next<E: PeerTaskExecutor>(
        &self,
        peer_id: &str,
        executor: &E,
    ) -> Result<Option<PeerExecutionResult>, TeamRuntimeError> {
        let Some(received) = self.receive_for(peer_id)?.into_iter().next() else {
            return Ok(None);
        };
        let shared_context = self.shared_context_prompt();
        let outcome = executor.execute(received.delegation.request(), shared_context.as_deref());
        let (success, detail) = match outcome {
            Ok(detail) => (true, detail),
            Err(error) => (false, serde_json::json!({ "error": error })),
        };
        self.acknowledge(peer_id, &received, success, detail.clone())?;
        Ok(Some(PeerExecutionResult {
            message_id: received.message_id,
            success,
            detail,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::teams::manifest_import::import_manifest;
    use std::sync::Mutex;

    fn manifest() -> TeamManifest {
        import_manifest(
            r#"{"version":1,"team_id":"team-a","max_delegation_depth":2,"peers":[{"peer_id":"bot-a","role":"lead"},{"peer_id":"bot-b","role":"worker"},{"peer_id":"bot-c","role":"worker"}]}"#,
        )
        .unwrap()
    }

    fn request(target_peer_id: &str) -> DelegationRequest {
        DelegationRequest {
            team_id: "team-a".to_string(),
            requester_peer_id: "bot-a".to_string(),
            target_peer_id: target_peer_id.to_string(),
            task: "inspect the change".to_string(),
            depth: 1,
        }
    }

    #[test]
    fn unapproved_peer_is_refused() {
        let manifest = manifest();
        let gate = PeerApprovalGate::new();
        let error = delegate(&manifest, &gate, None, request("bot-b")).unwrap_err();
        assert!(matches!(error, DelegationError::ApprovalRequired));
    }

    #[test]
    fn approved_delegation_round_trips() {
        let manifest = manifest();
        let mut gate = PeerApprovalGate::new();
        let approval = gate.approve(&manifest, "bot-a", "bot-b").unwrap();
        let delegation = delegate(&manifest, &gate, Some(&approval), request("bot-b")).unwrap();
        let encoded = serde_json::to_vec(&delegation).unwrap();
        let decoded: PeerDelegation = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, delegation);
        assert_eq!(decoded.request().target_peer_id, "bot-b");
        assert_eq!(decoded.state(), &DelegationState::Accepted);
    }

    #[test]
    fn approval_for_another_peer_cannot_be_retargeted() {
        let manifest = manifest();
        let mut gate = PeerApprovalGate::new();
        let approval = gate.approve(&manifest, "bot-a", "bot-b").unwrap();
        let error = delegate(&manifest, &gate, Some(&approval), request("bot-c")).unwrap_err();
        assert!(matches!(error, DelegationError::ApprovalMismatch));
    }

    struct CapturingExecutor {
        prompts: Mutex<Vec<Option<String>>>,
    }

    impl CapturingExecutor {
        fn new() -> Self {
            Self {
                prompts: Mutex::new(Vec::new()),
            }
        }
    }

    impl PeerTaskExecutor for CapturingExecutor {
        fn execute(
            &self,
            _request: &DelegationRequest,
            shared_context: Option<&str>,
        ) -> Result<serde_json::Value, String> {
            self.prompts
                .lock()
                .unwrap()
                .push(shared_context.map(str::to_string));
            Ok(serde_json::json!({"completed": true}))
        }
    }

    #[test]
    fn approved_dispatch_is_nonblocking_and_injects_user_owned_context_on_execution() {
        let directory = tempfile::tempdir().unwrap();
        let store = TeamCoordinationStore::open(directory.path().join("team.sqlite")).unwrap();
        let mut context = SectionContext::new();
        context
            .set_by_user("delivery brief", "Keep output concise.")
            .unwrap();
        let mut runtime = TeamRuntime::new(manifest(), store, context).unwrap();
        let approval = runtime.approve_peer_contact("bot-a", "bot-b").unwrap();

        let dispatched = runtime.dispatch(Some(&approval), request("bot-b")).unwrap();
        assert_eq!(dispatched.delegation.state(), &DelegationState::Accepted);
        assert_eq!(runtime.receive_for("bot-b").unwrap().len(), 1);

        let executor = CapturingExecutor::new();
        let result = runtime.execute_next("bot-b", &executor).unwrap().unwrap();
        assert!(result.success);
        assert!(runtime.receive_for("bot-b").unwrap().is_empty());
        assert_eq!(
            executor.prompts.lock().unwrap().as_slice(),
            [Some(
                "## Shared Team Context\n\n### delivery brief\nKeep output concise.".to_string()
            )]
        );
    }

    #[test]
    fn depth_cap_refuses_contact_before_any_mailbox_write() {
        let directory = tempfile::tempdir().unwrap();
        let store = TeamCoordinationStore::open(directory.path().join("team.sqlite")).unwrap();
        let mut runtime = TeamRuntime::new(manifest(), store, SectionContext::new()).unwrap();
        let approval = runtime.approve_peer_contact("bot-a", "bot-b").unwrap();
        let mut too_deep = request("bot-b");
        too_deep.depth = 3;

        let error = runtime.dispatch(Some(&approval), too_deep).unwrap_err();
        assert!(matches!(
            error,
            TeamRuntimeError::Delegation(DelegationError::DepthExceeded { .. })
        ));
        assert!(runtime.receive_for("bot-b").unwrap().is_empty());
    }
}
