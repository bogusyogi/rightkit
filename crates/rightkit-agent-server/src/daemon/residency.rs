//! Daemon residency policy (DL-009).
//!
//! Ported from prime-agent's `core/session-action-store.ts` residency
//! predicates and `modes/daemon/daemon-supervisor.ts` idle sweep policy.  The
//! policy is deliberately pure: the supervisor supplies refreshed summaries,
//! while this module decides whether a session may be passivated or a worker
//! tree may be evicted.  In particular, a missing/disconnected worker is
//! never treated as idle merely because it has no attached client.

use std::time::Duration;

/// Configured idle timeout. `Off` retains the daemon indefinitely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleEvictionMinutes {
    Off,
    Minutes(u64),
}

impl IdleEvictionMinutes {
    pub const fn minutes(value: u64) -> Self {
        Self::Minutes(value)
    }

    fn threshold_ms(self) -> Option<u128> {
        match self {
            Self::Off | Self::Minutes(0) => None,
            Self::Minutes(minutes) => Some(u128::from(minutes).saturating_mul(60_000)),
        }
    }
}

/// Session state needed by the idle policy. `last_activity_at_ms` is `None`
/// when the timestamp was absent or malformed; such a session is protected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionResidencySnapshot {
    pub is_session_active: bool,
    /// Descendants are explicit as well as being reflected in the canonical
    /// busy projection. This prevents a parent from being evicted while a
    /// descendant still owns work.
    pub has_active_descendant: bool,
    pub attached_clients: usize,
    pub has_registered_heartbeat: bool,
    pub has_registered_cron_job: bool,
    /// Durable schedules include persisted schedule claims, not only the
    /// currently loaded cron catalog.
    pub has_durable_scheduled_work: bool,
    pub last_activity_at_ms: Option<u128>,
}

/// Worker lifecycle as persisted by the daemon supervisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerLifecycle {
    Starting,
    Ready,
    Recovering,
    Stopping,
    Failed,
}

/// Refreshed supervisor-owned state for one worker tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerResidencySnapshot {
    pub lifecycle: WorkerLifecycle,
    pub is_connected: bool,
    pub is_stopping: bool,
    pub has_owner_client: bool,
    pub is_preparing_update_restart: bool,
    pub sessions: Vec<SessionResidencySnapshot>,
}

/// Why a worker or session is protected from idle eviction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidencyBlockReason {
    EvictionDisabled,
    InvalidIdleThreshold,
    AttachedClient,
    ActiveSession,
    ActiveDescendant,
    RegisteredHeartbeat,
    RegisteredCronJob,
    DurableScheduledWork,
    MissingActivityTimestamp,
    NotIdleLongEnough,
    NotReady,
    DisconnectedWorker,
    StoppingWorker,
    OwnerClient,
    PreparingUpdateRestart,
    EmptyWorker,
}

/// A pure decision that is useful to logs and machine-readable diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidencyDecision {
    Eligible,
    Protected(ResidencyBlockReason),
}

fn idle_threshold_met(
    session: &SessionResidencySnapshot,
    idle_eviction_minutes: IdleEvictionMinutes,
    now_ms: u128,
) -> ResidencyDecision {
    let Some(threshold_ms) = idle_eviction_minutes.threshold_ms() else {
        return ResidencyDecision::Protected(
            if matches!(idle_eviction_minutes, IdleEvictionMinutes::Off) {
                ResidencyBlockReason::EvictionDisabled
            } else {
                ResidencyBlockReason::InvalidIdleThreshold
            },
        );
    };
    if session.is_session_active {
        return ResidencyDecision::Protected(ResidencyBlockReason::ActiveSession);
    }
    if session.has_active_descendant {
        return ResidencyDecision::Protected(ResidencyBlockReason::ActiveDescendant);
    }
    if session.attached_clients != 0 {
        return ResidencyDecision::Protected(ResidencyBlockReason::AttachedClient);
    }
    if session.has_registered_heartbeat {
        return ResidencyDecision::Protected(ResidencyBlockReason::RegisteredHeartbeat);
    }
    if session.has_registered_cron_job {
        return ResidencyDecision::Protected(ResidencyBlockReason::RegisteredCronJob);
    }
    if session.has_durable_scheduled_work {
        return ResidencyDecision::Protected(ResidencyBlockReason::DurableScheduledWork);
    }
    let Some(last_activity_at_ms) = session.last_activity_at_ms else {
        return ResidencyDecision::Protected(ResidencyBlockReason::MissingActivityTimestamp);
    };
    if now_ms.saturating_sub(last_activity_at_ms) < threshold_ms {
        return ResidencyDecision::Protected(ResidencyBlockReason::NotIdleLongEnough);
    }
    ResidencyDecision::Eligible
}

/// Pure per-node residency policy. Roots remain owned by whole-worker
/// eviction, matching prime-agent's `canPassivateSession` contract.
pub fn can_passivate_session(
    session: &SessionResidencySnapshot,
    has_parent: bool,
    has_non_passive_descendants: bool,
    is_hydrating: bool,
    idle_eviction_minutes: IdleEvictionMinutes,
    now_ms: u128,
) -> bool {
    has_parent
        && !has_non_passive_descendants
        && !is_hydrating
        && matches!(
            idle_threshold_met(session, idle_eviction_minutes, now_ms),
            ResidencyDecision::Eligible
        )
}

/// Pure whole-tree residency policy. Supervisor-owned attachment and worker
/// connection state are required, so disconnected workers cannot be mistaken
/// for safe idle candidates.
pub fn can_evict_worker(
    worker: &WorkerResidencySnapshot,
    idle_eviction_minutes: IdleEvictionMinutes,
    now_ms: u128,
) -> bool {
    matches!(
        worker_residency_decision(worker, idle_eviction_minutes, now_ms),
        ResidencyDecision::Eligible
    )
}

/// Same decision as [`can_evict_worker`], retained for refreshed-summary logs.
pub fn worker_residency_decision(
    worker: &WorkerResidencySnapshot,
    idle_eviction_minutes: IdleEvictionMinutes,
    now_ms: u128,
) -> ResidencyDecision {
    if worker.lifecycle != WorkerLifecycle::Ready {
        return ResidencyDecision::Protected(ResidencyBlockReason::NotReady);
    }
    if !worker.is_connected {
        return ResidencyDecision::Protected(ResidencyBlockReason::DisconnectedWorker);
    }
    if worker.is_stopping {
        return ResidencyDecision::Protected(ResidencyBlockReason::StoppingWorker);
    }
    if worker.has_owner_client {
        return ResidencyDecision::Protected(ResidencyBlockReason::OwnerClient);
    }
    if worker.is_preparing_update_restart {
        return ResidencyDecision::Protected(ResidencyBlockReason::PreparingUpdateRestart);
    }
    if worker.sessions.is_empty() {
        return ResidencyDecision::Protected(ResidencyBlockReason::EmptyWorker);
    }
    worker
        .sessions
        .iter()
        .map(|session| idle_threshold_met(session, idle_eviction_minutes, now_ms))
        .find(|decision| !matches!(decision, ResidencyDecision::Eligible))
        .unwrap_or(ResidencyDecision::Eligible)
}

/// Port of prime-agent's supervisor sweep cadence: at least one minute and at
/// most five minutes, with roughly three opportunities during the timeout.
pub fn idle_eviction_sweep_interval(idle_eviction_minutes: IdleEvictionMinutes) -> Duration {
    const MIN_SWEEP: Duration = Duration::from_secs(60);
    const MAX_SWEEP: Duration = Duration::from_secs(5 * 60);
    match idle_eviction_minutes {
        IdleEvictionMinutes::Off => MAX_SWEEP,
        IdleEvictionMinutes::Minutes(minutes) => {
            let requested = Duration::from_millis(minutes.saturating_mul(60_000) / 3);
            requested.clamp(MIN_SWEEP, MAX_SWEEP)
        }
    }
}
