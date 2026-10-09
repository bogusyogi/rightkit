//! Bounded ownership work. A global cap and a per-repository cap bound concurrent
//! scanner work, and the per-repository cap is held separately for the inputs probe and
//! the scan (see [`Kind`]). Node has no aggregate bound. Every acquisition waits at most
//! `acquire_timeout`, so a saturated host refuses instead of queueing without limit.

use std::collections::HashMap;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// What a permit is held for. A repository has one permit of each kind, so an inputs
/// probe never queues behind a running scan, and a follower can probe, find the leader's
/// key, and join the leader's in-flight scan (CONTRACT section 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Probe,
    Scan,
}

/// Concurrency limits for ownership work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerLimits {
    global: usize,
    per_repository: usize,
    acquire_timeout: Duration,
}

impl WorkerLimits {
    /// Both caps must be at least one, and a repository cannot exceed the global cap.
    /// `per_repository` applies to each [`Kind`] separately.
    pub fn new(
        global: usize,
        per_repository: usize,
        acquire_timeout: Duration,
    ) -> Result<Self, String> {
        if global == 0 || per_repository == 0 || per_repository > global {
            return Err(
                "worker limits must be positive and per-repository must not exceed global".into(),
            );
        }
        Ok(Self {
            global,
            per_repository,
            acquire_timeout,
        })
    }

    pub fn global(&self) -> usize {
        self.global
    }

    pub fn per_repository(&self) -> usize {
        self.per_repository
    }

    pub fn acquire_timeout(&self) -> Duration {
        self.acquire_timeout
    }
}

impl Default for WorkerLimits {
    fn default() -> Self {
        // One probe and one scan per repository, held as separate permits. The probe
        // discovers the cache key, so a second admission must be able to probe while the
        // first scan runs and then join that scan.
        Self {
            global: 4,
            per_repository: 1,
            acquire_timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Default)]
struct LimiterState {
    active: usize,
    per_repository: HashMap<(String, Kind), usize>,
}

/// Counting gate over worker processes, keyed by repository.
#[derive(Debug)]
pub(crate) struct Limiter {
    limits: WorkerLimits,
    state: Mutex<LimiterState>,
    released: Condvar,
}

impl Limiter {
    pub(crate) fn new(limits: WorkerLimits) -> Self {
        Self {
            limits,
            state: Mutex::new(LimiterState::default()),
            released: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, LimiterState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Wait up to the acquire timeout for a slot under both caps.
    pub(crate) fn acquire(&self, repository: &str, kind: Kind) -> Result<Permit<'_>, String> {
        let deadline = Instant::now() + self.limits.acquire_timeout;
        let mut state = self.lock();
        loop {
            let repository_active = state
                .per_repository
                .get(&(repository.to_string(), kind))
                .copied()
                .unwrap_or(0);
            if state.active < self.limits.global && repository_active < self.limits.per_repository {
                state.active += 1;
                *state
                    .per_repository
                    .entry((repository.to_string(), kind))
                    .or_insert(0) += 1;
                return Ok(Permit {
                    limiter: self,
                    repository: repository.to_string(),
                    kind,
                });
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!(
                    "ownership worker capacity unavailable for {repository} within {} ms",
                    self.limits.acquire_timeout.as_millis()
                ));
            }
            let (guard, _) = self
                .released
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner);
            state = guard;
        }
    }

    fn release(&self, repository: &str, kind: Kind) {
        let mut state = self.lock();
        state.active = state.active.saturating_sub(1);
        let key = (repository.to_string(), kind);
        if let Some(count) = state.per_repository.get_mut(&key) {
            *count -= 1;
            if *count == 0 {
                state.per_repository.remove(&key);
            }
        }
        drop(state);
        self.released.notify_all();
    }

    #[cfg(test)]
    pub(crate) fn active(&self) -> usize {
        self.lock().active
    }
}

/// A held worker slot. Dropping it releases the slot.
#[derive(Debug)]
pub(crate) struct Permit<'a> {
    limiter: &'a Limiter,
    repository: String,
    kind: Kind,
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.limiter.release(&self.repository, self.kind);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_zero_and_inverted_limits() {
        assert!(WorkerLimits::new(0, 1, Duration::from_secs(1)).is_err());
        assert!(WorkerLimits::new(2, 0, Duration::from_secs(1)).is_err());
        assert!(WorkerLimits::new(1, 2, Duration::from_secs(1)).is_err());
        assert!(WorkerLimits::new(2, 2, Duration::from_secs(1)).is_ok());
    }

    #[test]
    fn global_cap_blocks_then_times_out_and_release_frees_capacity() {
        let limiter = Limiter::new(WorkerLimits::new(1, 1, Duration::from_millis(50)).unwrap());
        let held = limiter.acquire("repo-a", Kind::Scan).unwrap();
        let blocked = limiter.acquire("repo-b", Kind::Scan).unwrap_err();
        assert!(blocked.contains("capacity unavailable"), "{blocked}");
        drop(held);
        assert_eq!(limiter.active(), 0);
        assert!(limiter.acquire("repo-b", Kind::Scan).is_ok());
    }

    #[test]
    fn probe_and_scan_permits_are_separate_per_repository() {
        let limiter = Limiter::new(WorkerLimits::new(4, 1, Duration::from_millis(50)).unwrap());
        let scan = limiter.acquire("repo-a", Kind::Scan).unwrap();
        // A probe is not queued behind the scan, but a second scan and a second probe are.
        let probe = limiter.acquire("repo-a", Kind::Probe).unwrap();
        assert!(limiter.acquire("repo-a", Kind::Scan).is_err());
        assert!(limiter.acquire("repo-a", Kind::Probe).is_err());
        drop((scan, probe));
        assert_eq!(limiter.active(), 0);
    }

    #[test]
    fn per_repository_cap_allows_other_repositories() {
        let limiter = Limiter::new(WorkerLimits::new(3, 1, Duration::from_millis(50)).unwrap());
        let first = limiter.acquire("repo-a", Kind::Scan).unwrap();
        assert!(limiter.acquire("repo-a", Kind::Scan).is_err());
        let other = limiter.acquire("repo-b", Kind::Scan).unwrap();
        assert_eq!(limiter.active(), 2);
        drop((first, other));
        assert_eq!(limiter.active(), 0);
    }
}
