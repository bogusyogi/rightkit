#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Monotonic time used by behavior-changing worker decisions.
///
/// Decode duration measurement and diagnostics must continue to use
/// `std::time::Instant`; only their measured duration enters virtual time.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct WorkerInstant(u64);

impl WorkerInstant {
    #[cfg(test)]
    pub(crate) const ZERO: Self = Self(0);

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

/// Replay clock. Time advances only when the discrete-event scheduler selects
/// an event; it never sleeps or derives decision time from test wall clock.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct VirtualWorkerClock {
    pub(crate) now_nanos: AtomicU64,
}

#[cfg(test)]
impl VirtualWorkerClock {
    pub(crate) fn advance_to(&self, target: WorkerInstant) -> Result<(), ClockError> {
        let mut current = self.now_nanos.load(Ordering::Acquire);
        loop {
            if target.0 < current {
                return Err(ClockError::NonMonotonicAdvance {
                    current: WorkerInstant(current),
                    target,
                });
            }
            match self.now_nanos.compare_exchange_weak(
                current,
                target.0,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(observed) => current = observed,
            }
        }
    }
}

#[cfg(test)]
impl WorkerClock for VirtualWorkerClock {
    fn now(&self) -> WorkerInstant {
        WorkerInstant(self.now_nanos.load(Ordering::Acquire))
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClockError {
    NonMonotonicAdvance {
        current: WorkerInstant,
        target: WorkerInstant,
    },
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExecutorLane {
    Kws,
    MainAsr,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReplayEventKind {
    AudioBlock,
    ScriptedControl,
    LaneCompletion(ExecutorLane),
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReplayEvent {
    pub(crate) at: WorkerInstant,
    pub(crate) kind: ReplayEventKind,
}

/// Deadlines for the independent event streams used by Gate 2.
///
/// Lane completion is `submitted_at + measured_decode_duration`. It does not
/// advance audio or control time, preventing the invalid `audio + decode`
/// global-clock model.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct ReplayEventSchedule {
    pub(crate) next_audio: Option<WorkerInstant>,
    pub(crate) next_control: Option<WorkerInstant>,
    pub(crate) kws_completion: Option<WorkerInstant>,
    pub(crate) main_asr_completion: Option<WorkerInstant>,
}

#[cfg(test)]
impl ReplayEventSchedule {
    pub(crate) fn set_next_audio(&mut self, at: Option<WorkerInstant>) {
        self.next_audio = at;
    }

    pub(crate) fn set_next_control(&mut self, at: Option<WorkerInstant>) {
        self.next_control = at;
    }

    pub(crate) fn schedule_lane_completion(
        &mut self,
        lane: ExecutorLane,
        submitted_at: WorkerInstant,
        measured_decode_duration: Duration,
    ) {
        let completion = submitted_at.saturating_add(measured_decode_duration);
        match lane {
            ExecutorLane::Kws => self.kws_completion = Some(completion),
            ExecutorLane::MainAsr => self.main_asr_completion = Some(completion),
        }
    }

    pub(crate) fn clear_lane_completion(&mut self, lane: ExecutorLane) {
        match lane {
            ExecutorLane::Kws => self.kws_completion = None,
            ExecutorLane::MainAsr => self.main_asr_completion = None,
        }
    }

    pub(crate) fn next_event(&self) -> Option<ReplayEvent> {
        [
            self.next_audio.map(|at| ReplayEvent {
                at,
                kind: ReplayEventKind::AudioBlock,
            }),
            self.next_control.map(|at| ReplayEvent {
                at,
                kind: ReplayEventKind::ScriptedControl,
            }),
            self.kws_completion.map(|at| ReplayEvent {
                at,
                kind: ReplayEventKind::LaneCompletion(ExecutorLane::Kws),
            }),
            self.main_asr_completion.map(|at| ReplayEvent {
                at,
                kind: ReplayEventKind::LaneCompletion(ExecutorLane::MainAsr),
            }),
        ]
        .into_iter()
        .flatten()
        .min_by_key(|event| {
            let priority = match event.kind {
                ReplayEventKind::LaneCompletion(_) => 0,
                ReplayEventKind::ScriptedControl => 1,
                ReplayEventKind::AudioBlock => 2,
            };
            (event.at, priority)
        })
    }

    /// Consume one event & advance replay decision time to its timestamp.
    /// Audio/control producers install their following timestamp after handling
    /// the returned event; lane completion is one-shot.
    pub(crate) fn pop_next_event(
        &mut self,
        clock: &VirtualWorkerClock,
    ) -> Result<Option<ReplayEvent>, ClockError> {
        let Some(event) = self.next_event() else {
            return Ok(None);
        };
        clock.advance_to(event.at)?;
        match event.kind {
            ReplayEventKind::AudioBlock => self.next_audio = None,
            ReplayEventKind::ScriptedControl => self.next_control = None,
            ReplayEventKind::LaneCompletion(lane) => self.clear_lane_completion(lane),
        }
        Ok(Some(event))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(value: u64) -> WorkerInstant {
        WorkerInstant::from_duration_since_start(Duration::from_millis(value))
    }

    #[test]
    fn next_event_is_minimum_of_audio_control_and_lane_completions() {
        let mut schedule = ReplayEventSchedule::default();
        schedule.set_next_audio(Some(ms(20)));
        schedule.set_next_control(Some(ms(12)));
        schedule.schedule_lane_completion(ExecutorLane::Kws, ms(4), Duration::from_millis(4));
        schedule.schedule_lane_completion(ExecutorLane::MainAsr, ms(2), Duration::from_millis(13));

        assert_eq!(
            schedule.next_event(),
            Some(ReplayEvent {
                at: ms(8),
                kind: ReplayEventKind::LaneCompletion(ExecutorLane::Kws),
            })
        );
    }

    #[test]
    fn lane_completion_does_not_add_decode_time_to_audio_timeline() {
        let mut schedule = ReplayEventSchedule::default();
        schedule.set_next_audio(Some(ms(20)));
        schedule.schedule_lane_completion(ExecutorLane::Kws, ms(10), Duration::from_millis(30));
        schedule.schedule_lane_completion(ExecutorLane::MainAsr, ms(5), Duration::from_millis(7));

        assert_eq!(schedule.next_event().unwrap().at, ms(12));
        schedule.clear_lane_completion(ExecutorLane::MainAsr);
        assert_eq!(schedule.next_event().unwrap().at, ms(20));
        assert_eq!(
            schedule.next_event().unwrap().kind,
            ReplayEventKind::AudioBlock
        );
    }

    #[test]
    fn virtual_clock_rejects_backwards_advance() {
        let clock = VirtualWorkerClock::default();
        clock.advance_to(ms(20)).unwrap();
        clock.advance_to(ms(20)).unwrap();

        assert_eq!(clock.now(), ms(20));
        assert_eq!(
            clock.advance_to(ms(19)),
            Err(ClockError::NonMonotonicAdvance {
                current: ms(20),
                target: ms(19),
            })
        );
        assert_eq!(clock.now(), ms(20));
    }

    #[test]
    fn pop_advances_clock_and_consumes_only_selected_event() {
        let clock = VirtualWorkerClock::default();
        let mut schedule = ReplayEventSchedule::default();
        schedule.set_next_audio(Some(ms(20)));
        schedule.set_next_control(Some(ms(10)));

        assert_eq!(
            schedule.pop_next_event(&clock).unwrap(),
            Some(ReplayEvent {
                at: ms(10),
                kind: ReplayEventKind::ScriptedControl,
            })
        );
        assert_eq!(clock.now(), ms(10));
        assert_eq!(
            schedule.next_event().unwrap().kind,
            ReplayEventKind::AudioBlock
        );
    }

    #[test]
    fn completion_wins_a_same_instant_capture_tie() {
        let mut schedule = ReplayEventSchedule::default();
        schedule.set_next_audio(Some(ms(20)));
        schedule.schedule_lane_completion(ExecutorLane::Kws, ms(10), Duration::from_millis(10));

        assert_eq!(
            schedule.next_event().unwrap().kind,
            ReplayEventKind::LaneCompletion(ExecutorLane::Kws)
        );
    }
}
