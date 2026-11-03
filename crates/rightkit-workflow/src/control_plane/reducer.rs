//! Pure command/event reducer for the workflow control plane.
//!
//! WP-01-T03 acceptance:
//! - reducer is **pure** and is the **only** lifecycle transition authority;
//! - illegal, stale, duplicate, and terminal-state commands are
//!   deterministic typed outcomes.
//!

use thiserror::Error;

use crate::control_plane::domain::{JournalEvent, LifecycleState, Revision, RunId};
// Re-export so callers (and the crate prelude) can reach the outcome type
// without poking into `crate::control_plane::domain`.
pub use crate::control_plane::domain::ReducerOutcome;

/// Typed failure the reducer can emit. Replaces free-form reducer strings.
#[derive(Debug, Clone, PartialEq, Eq, Error, serde::Serialize, serde::Deserialize)]
pub enum ReducerError {
    /// Caller passed an `expected_revision` that does not match the
    /// current state revision.
    #[error("revision conflict: expected {expected}, observed {observed}")]
    RevisionConflict {
        expected: Revision,
        observed: Revision,
    },
    /// Caller asked the reducer to act on an object that is already in a
    /// terminal lifecycle phase.
    #[error("state is terminal ({phase:?}); no further transitions allowed")]
    Terminal { phase: String },
    /// Command id has already been processed (idempotent replay).
    #[error("duplicate command id `{0}`")]
    DuplicateCommand(String),
    /// A domain-level invariant failed; the reducer must not have been
    /// invoked with this state.
    #[error("internal reducer invariant violated: {0}")]
    Internal(String),
    #[error("unknown workflow command kind `{0}`")]
    UnknownCommand(String),
    #[error("illegal transition from {phase} via {command}")]
    IllegalTransition { phase: String, command: String },
}

/// Closed lifecycle command vocabulary. Storage/API adapters may deserialize
/// strings, but reducer acceptance is limited to this set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    Admit,
    Start,
    Complete,
    Fail,
    Pause,
    Resume,
    Cancel,
    Retry,
    NodeClaim,
    NodeStart,
    NodeComplete,
    NodeFail,
    NodeRetry,
    NodeSkip,
    NodeWait,
    ApprovalRequest,
    ApprovalDecide,
    EffectPlan,
    EffectObserve,
}

impl CommandKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admit => "admit",
            Self::Start => "start",
            Self::Complete => "complete",
            Self::Fail => "fail",
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Cancel => "cancel",
            Self::Retry => "retry",
            Self::NodeClaim => "node_claim",
            Self::NodeStart => "node_start",
            Self::NodeComplete => "node_complete",
            Self::NodeFail => "node_fail",
            Self::NodeRetry => "node_retry",
            Self::NodeSkip => "node_skip",
            Self::NodeWait => "node_wait",
            Self::ApprovalRequest => "approval_request",
            Self::ApprovalDecide => "approval_decide",
            Self::EffectPlan => "effect_plan",
            Self::EffectObserve => "effect_observe",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "admit" => Self::Admit,
            "start" => Self::Start,
            "complete" => Self::Complete,
            "fail" => Self::Fail,
            "pause" => Self::Pause,
            "resume" => Self::Resume,
            "cancel" => Self::Cancel,
            "retry" => Self::Retry,
            "node_claim" => Self::NodeClaim,
            "node_start" => Self::NodeStart,
            "node_complete" => Self::NodeComplete,
            "node_fail" => Self::NodeFail,
            "node_retry" => Self::NodeRetry,
            "node_skip" => Self::NodeSkip,
            "node_wait" => Self::NodeWait,
            "approval_request" => Self::ApprovalRequest,
            "approval_decide" => Self::ApprovalDecide,
            "effect_plan" => Self::EffectPlan,
            "effect_observe" => Self::EffectObserve,
            _ => return None,
        })
    }
}

/// Node-local state with revision and lease-generation fencing. Kept separate
/// from `LifecycleState` so run-level transitions retain a compact snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeLifecycleState {
    pub node_id: String,
    pub revision: Revision,
    pub generation: u64,
    pub phase: NodePhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodePhase {
    Ready,
    Claimed,
    Running,
    Waiting,
    Succeeded,
    Skipped,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecisionKind {
    Approved,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectObservationKind {
    Confirmed,
    Failed,
    OutcomeUnknown,
    Reconciling,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeCommand {
    pub command_id: String,
    pub kind: CommandKind,
    pub node_id: String,
    pub expected_revision: Revision,
    pub generation: u64,
    pub approval_decision: Option<ApprovalDecisionKind>,
    pub effect_observation: Option<EffectObservationKind>,
}

/// Typed node reducer outcome. A stale generation or revision leaves state
/// unchanged, making heartbeat/observation/completion replay safe.
pub fn reduce_node(
    state: &NodeLifecycleState,
    command: &NodeCommand,
) -> Result<NodeLifecycleState, ReducerError> {
    if command.node_id != state.node_id {
        return Err(ReducerError::Internal("node id mismatch".into()));
    }
    if command.expected_revision != state.revision {
        return Err(ReducerError::RevisionConflict {
            expected: command.expected_revision,
            observed: state.revision,
        });
    }
    let terminal = matches!(
        state.phase,
        NodePhase::Succeeded | NodePhase::Skipped | NodePhase::Failed | NodePhase::Cancelled
    );
    if terminal {
        return Err(ReducerError::Terminal {
            phase: format!("{:?}", state.phase),
        });
    }
    let (phase, generation) = match command.kind {
        CommandKind::NodeClaim if command.generation > state.generation => {
            (NodePhase::Claimed, command.generation)
        }
        CommandKind::NodeStart
            if command.generation == state.generation && state.phase == NodePhase::Claimed =>
        {
            (NodePhase::Running, state.generation)
        }
        CommandKind::NodeWait
            if command.generation == state.generation && state.phase == NodePhase::Running =>
        {
            (NodePhase::Waiting, state.generation)
        }
        CommandKind::NodeComplete
            if command.generation == state.generation
                && matches!(state.phase, NodePhase::Running | NodePhase::Waiting) =>
        {
            (NodePhase::Succeeded, state.generation)
        }
        CommandKind::NodeFail
            if command.generation == state.generation
                && matches!(
                    state.phase,
                    NodePhase::Claimed | NodePhase::Running | NodePhase::Waiting
                ) =>
        {
            (NodePhase::Failed, state.generation)
        }
        CommandKind::NodeRetry
            if command.generation == state.generation
                && matches!(state.phase, NodePhase::Claimed | NodePhase::Running) =>
        {
            (NodePhase::Ready, state.generation)
        }
        CommandKind::NodeSkip
            if command.generation == state.generation && state.phase == NodePhase::Ready =>
        {
            (NodePhase::Skipped, state.generation)
        }
        CommandKind::ApprovalRequest
            if command.generation == state.generation && state.phase == NodePhase::Running =>
        {
            (NodePhase::Waiting, state.generation)
        }
        CommandKind::ApprovalDecide
            if command.generation == state.generation
                && state.phase == NodePhase::Waiting
                && command.approval_decision == Some(ApprovalDecisionKind::Approved) =>
        {
            (NodePhase::Succeeded, state.generation)
        }
        CommandKind::ApprovalDecide
            if command.generation == state.generation
                && state.phase == NodePhase::Waiting
                && command.approval_decision == Some(ApprovalDecisionKind::Rejected) =>
        {
            (NodePhase::Failed, state.generation)
        }
        CommandKind::EffectPlan
            if command.generation == state.generation && state.phase == NodePhase::Running =>
        {
            (NodePhase::Waiting, state.generation)
        }
        CommandKind::EffectObserve
            if command.generation == state.generation
                && state.phase == NodePhase::Waiting
                && command.effect_observation == Some(EffectObservationKind::Confirmed) =>
        {
            (NodePhase::Succeeded, state.generation)
        }
        CommandKind::EffectObserve
            if command.generation == state.generation
                && state.phase == NodePhase::Waiting
                && command.effect_observation == Some(EffectObservationKind::Failed) =>
        {
            (NodePhase::Failed, state.generation)
        }
        CommandKind::EffectObserve
            if command.generation == state.generation
                && state.phase == NodePhase::Waiting
                && command.effect_observation == Some(EffectObservationKind::Cancelled) =>
        {
            (NodePhase::Cancelled, state.generation)
        }
        CommandKind::EffectObserve
            if command.generation == state.generation
                && state.phase == NodePhase::Waiting
                && matches!(
                    command.effect_observation,
                    Some(
                        EffectObservationKind::OutcomeUnknown | EffectObservationKind::Reconciling
                    )
                ) =>
        {
            (NodePhase::Waiting, state.generation)
        }
        _ => {
            return Err(ReducerError::IllegalTransition {
                phase: format!("{:?}", state.phase),
                command: command.kind.as_str().into(),
            });
        }
    };
    Ok(NodeLifecycleState {
        node_id: state.node_id.clone(),
        revision: state.revision.next(),
        generation,
        phase,
    })
}

/// One reducer command. Transport adapters retain the string spelling for
/// durable compatibility; [`reduce`] parses it into closed [`CommandKind`]
/// before any transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub command_id: String,
    pub kind: String,
    pub run_id: RunId,
    pub expected_revision: Revision,
    pub payload_json: String,
}

/// Apply `command` against `state` and return the new outcome. The
/// function is pure; it performs no I/O and never mutates its inputs.
///
pub fn reduce(state: &LifecycleState, command: &Command) -> ReducerOutcome {
    let Some(kind) = CommandKind::parse(&command.kind) else {
        return ReducerOutcome::Rejected {
            reason: ReducerError::UnknownCommand(command.kind.clone()),
            observed_revision: state.revision,
        };
    };
    if matches!(
        kind,
        CommandKind::NodeClaim
            | CommandKind::NodeStart
            | CommandKind::NodeComplete
            | CommandKind::NodeFail
            | CommandKind::NodeRetry
            | CommandKind::NodeSkip
            | CommandKind::NodeWait
            | CommandKind::ApprovalRequest
            | CommandKind::ApprovalDecide
            | CommandKind::EffectPlan
            | CommandKind::EffectObserve
    ) {
        return ReducerOutcome::Rejected {
            reason: ReducerError::Internal("node command requires reduce_node".into()),
            observed_revision: state.revision,
        };
    }
    // 1. Terminal phases are sticky.
    match state.phase {
        crate::control_plane::domain::LifecyclePhase::Succeeded
        | crate::control_plane::domain::LifecyclePhase::Cancelled
        | crate::control_plane::domain::LifecyclePhase::Failed => {
            return ReducerOutcome::Rejected {
                reason: ReducerError::Terminal {
                    phase: format!("{:?}", state.phase),
                },
                observed_revision: state.revision,
            };
        }
        _ => {}
    }

    // 2. Optimistic revision check.
    if command.expected_revision != state.revision {
        return ReducerOutcome::Rejected {
            reason: ReducerError::RevisionConflict {
                expected: command.expected_revision,
                observed: state.revision,
            },
            observed_revision: state.revision,
        };
    }

    // 3. Accepted commands advance lifecycle exactly once and emit one
    // journal event. Storage enforces command-id idempotency atomically.
    let next_revision = state.revision.next();
    let content_hash =
        crate::control_plane::domain::ContentHash::sha256(command.payload_json.as_bytes());
    let event = JournalEvent {
        event_id: format!("evt_{}_{}", command.command_id, next_revision.0),
        run_id: command.run_id.clone(),
        sequence: next_revision.0,
        kind: command.kind.clone(),
        payload_json: command.payload_json.clone(),
        content_hash,
        occurred_at_ms: 0, // filled by the daemon at apply time
    };

    use crate::control_plane::domain::LifecyclePhase as P;
    let phase = match (state.phase, kind) {
        (P::Pending, CommandKind::Admit) => P::Pending,
        (P::Pending, CommandKind::Start) => P::Running,
        (P::Running, CommandKind::Pause) => P::Suspended,
        (P::Suspended, CommandKind::Resume | CommandKind::Retry) => P::Running,
        (P::Pending | P::Running | P::Suspended, CommandKind::Cancel) => P::Cancelled,
        (P::Running, CommandKind::Complete) => P::Succeeded,
        (P::Pending | P::Running | P::Suspended, CommandKind::Fail) => P::Failed,
        (
            _,
            CommandKind::NodeClaim
            | CommandKind::NodeStart
            | CommandKind::NodeComplete
            | CommandKind::NodeFail
            | CommandKind::NodeRetry
            | CommandKind::NodeSkip
            | CommandKind::NodeWait
            | CommandKind::ApprovalRequest
            | CommandKind::ApprovalDecide
            | CommandKind::EffectPlan
            | CommandKind::EffectObserve,
        ) => unreachable!("node commands rejected above"),
        (phase, command) => {
            return ReducerOutcome::Rejected {
                reason: ReducerError::IllegalTransition {
                    phase: format!("{phase:?}"),
                    command: command.as_str().into(),
                },
                observed_revision: state.revision,
            };
        }
    };
    let next_state = LifecycleState {
        workflow_id: state.workflow_id.clone(),
        version_id: state.version_id.clone(),
        revision: next_revision,
        phase,
    };

    ReducerOutcome::Applied {
        next_state: Box::new(next_state),
        next_revision,
        journal_events: vec![event],
    }
}
