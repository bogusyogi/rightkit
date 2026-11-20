//! Per-press release metadata serialized only with pure handoff transitions.
use std::sync::{Mutex, MutexGuard};

use crate::ptt::{PttAbortCause, PttEnd, PttHandoff, PttPhase};
use crate::release_edge::NativeReleaseEdge;

pub(crate) type ReleaseDispatch = (
    PttEnd,
    Option<PttAbortCause>,
    Option<Option<NativeReleaseEdge>>,
);

struct State {
    handoff: PttHandoff,
    generation: u64,
    release: Option<(PttEnd, Option<NativeReleaseEdge>)>,
}

pub(crate) struct ReleaseHandoff(Mutex<State>);

impl ReleaseHandoff {
    pub(crate) fn new() -> Self {
        Self(Mutex::new(State {
            handoff: PttHandoff::new(),
            generation: 0,
            release: None,
        }))
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        match self.0.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.handoff.reset();
                state.generation = 0;
                state.release = None;
                self.0.clear_poison();
                state
            }
        }
    }

    pub(crate) fn begin_start(&self, generation: u64) -> bool {
        let mut state = self.lock();
        if !state.handoff.begin_start() {
            return false;
        }
        if state.generation != generation {
            state.release = None;
        }
        state.generation = generation;
        if let Some((end, _)) = state.release {
            state.handoff.request_end(end, None);
        }
        true
    }

    pub(crate) fn is_active(&self) -> bool {
        self.lock().handoff.is_active()
    }

    pub(crate) fn phase(&self) -> Option<PttPhase> {
        self.lock().handoff.phase()
    }

    pub(crate) fn reset(&self) {
        let mut state = self.lock();
        state.handoff.reset();
        state.generation = 0;
        state.release = None;
    }

    fn finish(
        state: &mut State,
        result: Option<(PttEnd, Option<PttAbortCause>)>,
    ) -> Option<ReleaseDispatch> {
        result.map(|(end, cause)| {
            let release = state.release.take();
            let edge = if matches!(end, PttEnd::Stop | PttEnd::Send) {
                release.map(|(_, edge)| edge)
            } else {
                None
            };
            (end, cause, edge)
        })
    }

    pub(crate) fn request_release(
        &self,
        generation: u64,
        end: PttEnd,
        edge: Option<NativeReleaseEdge>,
    ) -> Option<ReleaseDispatch> {
        let mut state = self.lock();
        if state.handoff.is_active() && state.generation != generation {
            return None;
        }
        state.generation = generation;
        state.release = Some((end, edge));
        let result = state.handoff.request_end(end, None);
        Self::finish(&mut state, result)
    }

    pub(crate) fn request_end(
        &self,
        end: PttEnd,
        cause: Option<PttAbortCause>,
    ) -> Option<ReleaseDispatch> {
        let mut state = self.lock();
        if end == PttEnd::Cancel {
            state.release = None;
        }
        let result = state.handoff.request_end(end, cause);
        Self::finish(&mut state, result)
    }

    pub(crate) fn complete_start(
        &self,
        generation: u64,
        valid: bool,
    ) -> Option<ReleaseDispatch> {
        let mut state = self.lock();
        if state.generation != generation {
            return None;
        }
        let result = state.handoff.complete_start(valid);
        Self::finish(&mut state, result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::release_edge::NativeReleaseClock;

    fn edge(seconds: u64) -> NativeReleaseEdge {
        NativeReleaseEdge {
            clock: NativeReleaseClock::CoreAudioHostNsV1,
            seconds,
            nanos: 123,
        }
    }

    #[test]
    fn deferred_release_keeps_original_edge() {
        let handoff = ReleaseHandoff::new();
        assert!(handoff.begin_start(1));
        assert_eq!(handoff.request_release(1, PttEnd::Send, Some(edge(7))), None);
        assert_eq!(
            handoff.complete_start(1, false),
            Some((PttEnd::Send, None, Some(Some(edge(7)))))
        );
    }

    #[test]
    fn release_between_arm_check_and_start_is_retained() {
        let handoff = ReleaseHandoff::new();
        assert_eq!(handoff.request_release(1, PttEnd::Stop, Some(edge(8))), None);
        assert!(handoff.begin_start(1));
        assert_eq!(
            handoff.complete_start(1, false),
            Some((PttEnd::Stop, None, Some(Some(edge(8)))))
        );
    }

    #[test]
    fn reset_and_new_press_cannot_inherit_previous_edge() {
        let handoff = ReleaseHandoff::new();
        assert!(handoff.begin_start(1));
        handoff.request_release(1, PttEnd::Stop, Some(edge(9)));
        handoff.reset();
        assert!(handoff.begin_start(2));
        assert_eq!(handoff.complete_start(1, false), None);
        assert_eq!(
            handoff.complete_start(2, false),
            Some((PttEnd::Stop, None, None))
        );
    }

    #[test]
    fn copied_dispatch_survives_concurrent_next_press() {
        use std::sync::{Arc, Barrier};
        let handoff = Arc::new(ReleaseHandoff::new());
        assert!(handoff.begin_start(1));
        assert_eq!(handoff.complete_start(1, true), None);
        let first = handoff.request_release(1, PttEnd::Stop, Some(edge(11)));
        let barrier = Arc::new(Barrier::new(2));
        let worker = {
            let handoff = Arc::clone(&handoff);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                assert!(handoff.begin_start(2));
                handoff.request_release(2, PttEnd::Send, Some(edge(12)));
                handoff.complete_start(2, false)
            })
        };
        barrier.wait();
        let second = worker.join().unwrap();
        assert_eq!(first, Some((PttEnd::Stop, None, Some(Some(edge(11))))));
        assert_eq!(second, Some((PttEnd::Send, None, Some(Some(edge(12))))));
    }

    #[test]
    fn poison_clears_previous_press_metadata() {
        let handoff = ReleaseHandoff::new();
        assert!(handoff.begin_start(1));
        handoff.request_release(1, PttEnd::Stop, Some(edge(13)));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _state = handoff.0.lock().unwrap();
            panic!("poison handoff");
        }));
        assert!(handoff.begin_start(2));
        assert_eq!(handoff.complete_start(2, false), Some((PttEnd::Stop, None, None)));
    }

    #[test]
    fn missing_clock_and_cancel_are_distinct() {
        let handoff = ReleaseHandoff::new();
        assert!(handoff.begin_start(1));
        handoff.request_release(1, PttEnd::Stop, None);
        assert_eq!(
            handoff.complete_start(1, false),
            Some((PttEnd::Stop, None, Some(None)))
        );
        assert!(handoff.begin_start(2));
        handoff.request_release(2, PttEnd::Stop, Some(edge(10)));
        handoff.request_end(PttEnd::Cancel, None);
        assert_eq!(
            handoff.complete_start(2, false),
            Some((PttEnd::Cancel, None, None))
        );
    }
}
