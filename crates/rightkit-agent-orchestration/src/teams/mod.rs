//! VG-007 — durable peer agent-team coordination.
//!
//! `ADAPT_TO_RUST` from cline `apps/cli/src/utils/team-command.ts`. The
//! donor's `/team` command spawns a **peer** team of agents (`enableAgentTeams`
//! + `createTeamName`) that coordinates across independent sessions/processes
//! — distinct from the host app's existing parent-child subagent dispatch
//! (`crate::spawn`, `rightkit_agent_coding_tools::TeamDirectory`) where
//! one process owns and blocks on its children.
//!
//! Peer coordination needs three durable, shared surfaces that survive any
//! single peer's process exit and are safe under concurrent writers:
//!
//! - **task board** ([`board`]) — a shared list of claimable work items peers
//!   pull from and update as they progress.
//! - **mailbox** ([`mailbox`]) — point-to-point and broadcast messages between
//!   named peers, delivered at-least-once and consumed once per recipient.
//! - **mission log** ([`missionlog`]) — an append-only journal of what every
//!   peer did, for audit and for a peer that joins late to reconstruct state.
//!
//! All three are backed by one SQLite database per team (mirroring the
//! [`crate::checkpoint::GraphCheckpointStore`] durability pattern already used
//! for graph checkpoints in this crate) so peers on the same machine can share
//! a team by pointing at the same file, and writes are atomic under
//! concurrent access without a bespoke lock file.
//!
//! Hosts construct [`TeamRuntime`] after manifest import, then provide a real
//! target executor at `execute_next`; FFI and UI bridge ownership stays with
//! their respective runtime lanes.

pub mod board;
pub mod delegation;
pub mod mailbox;
pub mod manifest_import;
pub mod missionlog;
pub mod peer_approval;
pub mod section_context;
pub mod store;

pub use board::{TaskBoardItem, TaskBoardStatus};
pub use delegation::{
    delegate, DelegationError, DelegationRequest, DelegationState, PeerDelegation, PeerDispatch,
    PeerExecutionResult, PeerTaskExecutor, ReceivedPeerDelegation, TeamRuntime, TeamRuntimeError,
};
pub use mailbox::MailboxMessage;
pub use manifest_import::{import_manifest, ManifestImportError, PeerManifest, TeamManifest};
pub use missionlog::MissionLogEntry;
pub use peer_approval::{ApprovedPeer, PeerApproval, PeerApprovalError, PeerApprovalGate};
pub use store::{TeamCoordinationError, TeamCoordinationStore};
