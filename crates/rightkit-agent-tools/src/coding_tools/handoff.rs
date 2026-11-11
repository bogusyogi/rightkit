//! Explicit cross-target session handoff (ENG-66).
//!
//! A handoff moves a session's ownership between execution targets
//! (`Local`, `Cloud`, `Computer` — see [`crate::coding_tools::cloud_endpoint::ExecutionTarget`])
//! deliberately. Nothing in this module ever changes a session's active
//! target implicitly: [`HandoffController::begin`] is the only way in, and it
//! requires the caller to name both the source and destination target
//! explicitly, even when the session is already idle.
//!
//! # Never both own it
//!
//! [`HandoffController`] tracks one `HandoffState` per session. A handoff
//! goes through two phases before the destination is allowed to
//! resume:
//!
//! 1. **Quiesce** ([`HandoffController::begin`]) — the source target is
//!    marked quiescing; [`HandoffController::confirm_quiesced`] must be
//!    called by the source's own runtime once it has genuinely stopped
//!    issuing tool calls for the session (this module does not itself stop
//!    a target's dispatch loop — that is the caller's responsibility; this
//!    module only refuses to let the destination proceed until it is told
//!    the source has stopped).
//! 2. **Resume** ([`HandoffController::resume_at_destination`]) — only
//!    callable after `confirm_quiesced`; it flips ownership to the
//!    destination target and returns the package cursor the destination
//!    should replay from.
//!
//! Calling `resume_at_destination` before `confirm_quiesced` returns
//! [`HandoffError::SourceNotQuiesced`] — this is the enforcement point that
//! makes double-ownership (two targets both driving one session at once)
//! structurally impossible through this module, rather than merely
//! discouraged by convention.
//!
//! # Credential safety
//!
//! This module carries no credential material. A handoff transfers a
//! [`rightkit_agent_engine_history::session_package::SessionPackage`] (transcript +
//! cursor + workspace identity + target metadata only); any credential the
//! destination target needs is re-established there through its own normal
//! auth flow (e.g. `cloud_endpoint::update_target_credential`), never copied
//! across the handoff boundary.

use crate::coding_tools::cloud_endpoint::ExecutionTarget;
use rightkit_agent_engine_history::session_package::SessionPackage;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HandoffPhase {
    /// Source target has been asked to stop but has not yet confirmed.
    Quiescing,
    /// Source confirmed it has stopped; destination may now resume.
    SourceQuiesced,
    /// Destination has resumed; handoff is complete and the record is
    /// retained only for audit/inspection until explicitly cleared.
    Completed,
}

#[derive(Debug, Clone)]
struct HandoffState {
    source: ExecutionTarget,
    destination: ExecutionTarget,
    controller_epoch: u64,
    phase: HandoffPhase,
}

/// Opaque controller fence for one handoff attempt. Callers must retain this
/// lease through quiescence and destination resume so a delayed controller
/// cannot act on a replacement handoff for the same session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandoffLease {
    pub controller_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandoffError {
    /// A handoff is already in flight for this session; finish or cancel it
    /// before starting another.
    AlreadyInFlight,
    /// No handoff is in flight for this session.
    NoHandoffInFlight,
    /// `resume_at_destination` was called before the source confirmed it
    /// had quiesced. This is the guard that prevents two targets from both
    /// owning the session at once.
    SourceNotQuiesced,
    /// Source and destination named the same target — not a handoff.
    SameTarget,
    /// `confirm_quiesced`/`resume_at_destination` named a target that does
    /// not match the in-flight handoff's recorded source/destination.
    TargetMismatch,
    /// A stale controller attempted to advance a replacement handoff.
    StaleEpoch,
    /// A lifecycle operation was not valid for the recorded handoff phase.
    InvalidPhase,
    /// Session identity is required for a handoff key.
    EmptySessionId,
}

impl std::fmt::Display for HandoffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyInFlight => write!(f, "a handoff is already in flight for this session"),
            Self::NoHandoffInFlight => write!(f, "no handoff is in flight for this session"),
            Self::SourceNotQuiesced => write!(
                f,
                "destination target cannot resume before the source target has \
                 confirmed it quiesced"
            ),
            Self::SameTarget => write!(f, "handoff source and destination must differ"),
            Self::TargetMismatch => {
                write!(f, "target does not match the in-flight handoff's record")
            }
            Self::StaleEpoch => write!(f, "handoff controller epoch is stale"),
            Self::InvalidPhase => {
                write!(f, "handoff lifecycle phase does not permit this operation")
            }
            Self::EmptySessionId => write!(f, "handoff session id must not be empty"),
        }
    }
}

impl std::error::Error for HandoffError {}

/// Process-wide handoff state, keyed by session id. Mirrors the pattern
/// `cloud_endpoint`'s credential registry uses for session-scoped state.
static HANDOFFS: OnceLock<Mutex<HashMap<String, HandoffState>>> = OnceLock::new();
static NEXT_CONTROLLER_EPOCH: AtomicU64 = AtomicU64::new(1);

fn registry() -> &'static Mutex<HashMap<String, HandoffState>> {
    HANDOFFS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Coordinates an explicit cross-target handoff for one session.
///
/// Stateless wrapper over the process-wide registry (mirrors
/// `cloud_endpoint`'s free-function style) so callers do not need to thread
/// an instance through; kept as a named type for a clear API surface and to
/// leave room for a future non-global-registry constructor without breaking
/// callers.
pub struct HandoffController;

impl HandoffController {
    /// Begin an explicit handoff of `session_id` from `source` to
    /// `destination`. Marks the source quiescing; the destination cannot
    /// resume until [`Self::confirm_quiesced`] is called.
    pub fn begin(
        session_id: &str,
        source: ExecutionTarget,
        destination: ExecutionTarget,
    ) -> Result<(), HandoffError> {
        Self::begin_fenced(session_id, source, destination).map(|_| ())
    }

    /// Begin a handoff and return its controller fence. Every subsequent
    /// lifecycle transition should use the fenced methods below.
    pub fn begin_fenced(
        session_id: &str,
        source: ExecutionTarget,
        destination: ExecutionTarget,
    ) -> Result<HandoffLease, HandoffError> {
        if session_id.trim().is_empty() {
            return Err(HandoffError::EmptySessionId);
        }
        if source == destination {
            return Err(HandoffError::SameTarget);
        }
        let mut map = registry().lock().unwrap();
        if map.contains_key(session_id) {
            return Err(HandoffError::AlreadyInFlight);
        }
        let lease = HandoffLease {
            controller_epoch: NEXT_CONTROLLER_EPOCH.fetch_add(1, Ordering::Relaxed),
        };
        map.insert(
            session_id.to_string(),
            HandoffState {
                source,
                destination,
                controller_epoch: lease.controller_epoch,
                phase: HandoffPhase::Quiescing,
            },
        );
        Ok(lease)
    }

    /// The source target's own runtime calls this once it has genuinely
    /// stopped dispatching tool calls for `session_id`. `source` must match
    /// the target named in [`Self::begin`].
    pub fn confirm_quiesced(session_id: &str, source: ExecutionTarget) -> Result<(), HandoffError> {
        let mut map = registry().lock().unwrap();
        let state = map
            .get_mut(session_id)
            .ok_or(HandoffError::NoHandoffInFlight)?;
        if state.source != source {
            return Err(HandoffError::TargetMismatch);
        }
        if state.phase != HandoffPhase::Quiescing {
            return Err(HandoffError::InvalidPhase);
        }
        state.phase = HandoffPhase::SourceQuiesced;
        Ok(())
    }

    /// Fenced form of [`Self::confirm_quiesced`].
    pub fn confirm_quiesced_fenced(
        session_id: &str,
        source: ExecutionTarget,
        lease: HandoffLease,
    ) -> Result<(), HandoffError> {
        let mut map = registry().lock().unwrap();
        let state = map
            .get_mut(session_id)
            .ok_or(HandoffError::NoHandoffInFlight)?;
        if state.controller_epoch != lease.controller_epoch {
            return Err(HandoffError::StaleEpoch);
        }
        if state.source != source {
            return Err(HandoffError::TargetMismatch);
        }
        if state.phase != HandoffPhase::Quiescing {
            return Err(HandoffError::InvalidPhase);
        }
        state.phase = HandoffPhase::SourceQuiesced;
        Ok(())
    }

    /// The destination target resumes the session, consuming `package`'s
    /// cursor as its replay starting point. Fails with
    /// [`HandoffError::SourceNotQuiesced`] unless [`Self::confirm_quiesced`]
    /// has already been recorded for this handoff — this is what prevents
    /// two targets from ever both owning the session.
    pub fn resume_at_destination(
        session_id: &str,
        destination: ExecutionTarget,
        package: &SessionPackage,
    ) -> Result<u64, HandoffError> {
        let mut map = registry().lock().unwrap();
        let state = map
            .get_mut(session_id)
            .ok_or(HandoffError::NoHandoffInFlight)?;
        if state.destination != destination {
            return Err(HandoffError::TargetMismatch);
        }
        match state.phase {
            HandoffPhase::Quiescing => return Err(HandoffError::SourceNotQuiesced),
            HandoffPhase::SourceQuiesced => {}
            HandoffPhase::Completed => return Err(HandoffError::InvalidPhase),
        }
        state.phase = HandoffPhase::Completed;
        Ok(package.cursor)
    }

    /// Fenced form of [`Self::resume_at_destination`], rejecting stale
    /// controllers before a destination can claim session ownership.
    pub fn resume_at_destination_fenced(
        session_id: &str,
        destination: ExecutionTarget,
        package: &SessionPackage,
        lease: HandoffLease,
    ) -> Result<u64, HandoffError> {
        let mut map = registry().lock().unwrap();
        let state = map
            .get_mut(session_id)
            .ok_or(HandoffError::NoHandoffInFlight)?;
        if state.controller_epoch != lease.controller_epoch {
            return Err(HandoffError::StaleEpoch);
        }
        if state.destination != destination {
            return Err(HandoffError::TargetMismatch);
        }
        match state.phase {
            HandoffPhase::Quiescing => return Err(HandoffError::SourceNotQuiesced),
            HandoffPhase::SourceQuiesced => {}
            HandoffPhase::Completed => return Err(HandoffError::InvalidPhase),
        }
        state.phase = HandoffPhase::Completed;
        Ok(package.cursor)
    }

    /// Drop the in-flight (or completed) handoff record for `session_id`,
    /// e.g. once the destination has durably persisted its own resumed
    /// state. Idempotent: clearing a session with no record is a no-op.
    pub fn clear(session_id: &str) {
        registry().lock().unwrap().remove(session_id);
    }

    /// True if a handoff is currently in flight (not yet completed) for
    /// `session_id`. Used by callers that need to refuse concurrent tool
    /// dispatch on a session mid-handoff.
    pub fn is_in_flight(session_id: &str) -> bool {
        matches!(
            registry().lock().unwrap().get(session_id),
            Some(state) if state.phase != HandoffPhase::Completed
        )
    }
}
