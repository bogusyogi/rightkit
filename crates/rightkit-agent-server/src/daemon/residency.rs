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

#[cfg(test)]
mod tests {
    use super::*;

    fn idle_session(now_ms: u128) -> SessionResidencySnapshot {
        SessionResidencySnapshot {
            is_session_active: false,
            has_active_descendant: false,
            attached_clients: 0,
            has_registered_heartbeat: false,
            has_registered_cron_job: false,
            has_durable_scheduled_work: false,
            last_activity_at_ms: Some(now_ms - 10 * 60_000),
        }
    }

    fn idle_worker(now_ms: u128) -> WorkerResidencySnapshot {
        WorkerResidencySnapshot {
            lifecycle: WorkerLifecycle::Ready,
            is_connected: true,
            is_stopping: false,
            has_owner_client: false,
            is_preparing_update_restart: false,
            sessions: vec![idle_session(now_ms)],
        }
    }

    #[test]
    fn idle_session_is_eligible_after_threshold() {
        let now = 1_000_000;
        assert!(can_evict_worker(
            &idle_worker(now),
            IdleEvictionMinutes::minutes(5),
            now
        ));
    }

    #[test]
    fn attached_busy_descendant_heartbeat_cron_and_durable_schedule_are_protected() {
        let now = 1_000_000;
        let mut worker = idle_worker(now);
        let protected: [fn(&mut SessionResidencySnapshot); 6] = [
            |s| s.attached_clients = 1,
            |s| s.is_session_active = true,
            |s| s.has_active_descendant = true,
            |s| s.has_registered_heartbeat = true,
            |s| s.has_registered_cron_job = true,
            |s| s.has_durable_scheduled_work = true,
        ];
        for mutate in protected {
            worker.sessions[0] = idle_session(now);
            mutate(&mut worker.sessions[0]);
            assert!(!can_evict_worker(
                &worker,
                IdleEvictionMinutes::minutes(5),
                now
            ));
        }
    }

    #[test]
    fn disconnected_update_owner_and_stopping_workers_are_protected() {
        let now = 1_000_000;
        let protected: [fn(&mut WorkerResidencySnapshot); 5] = [
            |w| w.is_connected = false,
            |w| w.is_preparing_update_restart = true,
            |w| w.has_owner_client = true,
            |w| w.is_stopping = true,
            |w| w.lifecycle = WorkerLifecycle::Recovering,
        ];
        for mutate in protected {
            let mut worker = idle_worker(now);
            mutate(&mut worker);
            assert!(!can_evict_worker(
                &worker,
                IdleEvictionMinutes::minutes(5),
                now
            ));
        }
    }

    #[test]
    fn passivation_requires_a_non_root_without_non_passive_descendants() {
        let now = 1_000_000;
        let session = idle_session(now);
        assert!(!can_passivate_session(
            &session,
            false,
            false,
            false,
            IdleEvictionMinutes::minutes(5),
            now
        ));
        assert!(!can_passivate_session(
            &session,
            true,
            true,
            false,
            IdleEvictionMinutes::minutes(5),
            now
        ));
        assert!(can_passivate_session(
            &session,
            true,
            false,
            false,
            IdleEvictionMinutes::minutes(5),
            now
        ));
    }

    #[test]
    fn missing_timestamp_and_disabled_timeout_are_fail_closed() {
        let now = 1_000_000;
        let mut worker = idle_worker(now);
        worker.sessions[0].last_activity_at_ms = None;
        assert!(!can_evict_worker(
            &worker,
            IdleEvictionMinutes::minutes(5),
            now
        ));
        assert!(!can_evict_worker(
            &idle_worker(now),
            IdleEvictionMinutes::Off,
            now
        ));
        assert!(!can_evict_worker(
            &idle_worker(now),
            IdleEvictionMinutes::minutes(0),
            now
        ));
    }

    #[test]
    fn sweep_interval_is_bounded_and_off_is_slow() {
        assert_eq!(
            idle_eviction_sweep_interval(IdleEvictionMinutes::Off),
            Duration::from_secs(300)
        );
        assert_eq!(
            idle_eviction_sweep_interval(IdleEvictionMinutes::minutes(1)),
            Duration::from_secs(60)
        );
        assert_eq!(
            idle_eviction_sweep_interval(IdleEvictionMinutes::minutes(60)),
            Duration::from_secs(300)
        );
    }
}
