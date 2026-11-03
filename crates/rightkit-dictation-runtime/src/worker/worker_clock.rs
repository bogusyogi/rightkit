use std::time::{Duration, Instant};

/// Monotonic time used by behavior-changing worker decisions.
///
/// Decode duration measurement and diagnostics must continue to use
/// `std::time::Instant`; only their measured duration enters virtual time.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct WorkerInstant(u64);

impl WorkerInstant {
    pub(crate) fn from_duration_since_start(value: Duration) -> Self {
        Self(duration_nanos(value))
    }

    pub(crate) fn duration_since(self, earlier: Self) -> Duration {
        Duration::from_nanos(self.0.saturating_sub(earlier.0))
    }

    pub(crate) fn saturating_add(self, duration: Duration) -> Self {
        Self(self.0.saturating_add(duration_nanos(duration)))
    }
}

pub(crate) fn duration_nanos(value: Duration) -> u64 {
    value.as_nanos().min(u128::from(u64::MAX)) as u64
}

/// Clock seam shared by production and deterministic replay.
pub(crate) trait WorkerClock: Send + Sync {
    fn now(&self) -> WorkerInstant;
}

/// Production clock. Its origin is private so callers cannot mix unrelated
/// `Instant` values with worker decision time.
pub(crate) struct MonotonicWorkerClock {
    pub(crate) origin: Instant,
}

impl MonotonicWorkerClock {
    pub(crate) fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for MonotonicWorkerClock {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkerClock for MonotonicWorkerClock {
    fn now(&self) -> WorkerInstant {
        WorkerInstant::from_duration_since_start(self.origin.elapsed())
    }
}
