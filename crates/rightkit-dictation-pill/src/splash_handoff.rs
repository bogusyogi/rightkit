//! The hand-off between a native splash and the pill (platform-free state machine).
//!
//! While the native side reports the hand-off active the host defers pill mutations. As a backstop
//! against a native phase that never finishes, a hand-off that stays active longer than
//! `max_active` is forced complete, but only once the host says its engine is ready: forcing it
//! earlier tears the splash down while the app cannot yet serve a request, and discards the user's
//! first speech. A forced hand-off is final for the launch (it never alternates between deferred
//! and allowed) until [`SplashHandoff::reset`].
//!
//! The crate owns only this state machine. The host supplies `max_active` (its product choice),
//! the native answer, the engine-ready check and the act of telling the native side the hand-off
//! is over; nothing here reads a clock, a window or a setting (`now` is passed in).

use std::time::{Duration, Instant};

/// The hand-off latch.
#[derive(Debug)]
pub struct SplashHandoff {
    max_active: Duration,
    first_seen_active: Option<Instant>,
    forced: bool,
}

/// What the host must do after an observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandoffAction {
    None,
    /// Tell the native side the hand-off is over.
    ForceComplete,
}

/// The observed hand-off state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HandoffState {
    /// Treat the hand-off as active (defer pill mutations).
    pub active: bool,
    pub action: HandoffAction,
}

impl SplashHandoff {
    /// A latch that forces completion after the native side has reported the hand-off active for
    /// longer than `max_active`. `const`, so a host can keep it in a `static Mutex`.
    pub const fn new(max_active: Duration) -> SplashHandoff {
        SplashHandoff {
            max_active,
            first_seen_active: None,
            forced: false,
        }
    }

    /// A forced hand-off is final until [`SplashHandoff::reset`]; the host need not ask the native
    /// side again.
    pub fn is_forced(&self) -> bool {
        self.forced
    }

    /// A new splash starts: forget a forced hand-off and the elapsed marker.
    pub fn reset(&mut self) {
        self.forced = false;
        self.first_seen_active = None;
    }

    /// Fold in what the native side reports. `engine_ready` is asked only when the hand-off has
    /// been active longer than the backstop.
    pub fn observe(
        &mut self,
        native_active: bool,
        engine_ready: impl FnOnce() -> bool,
        now: Instant,
    ) -> HandoffState {
        if self.forced {
            return HandoffState {
                active: false,
                action: HandoffAction::None,
            };
        }
        if !native_active {
            self.first_seen_active = None;
            return HandoffState {
                active: false,
                action: HandoffAction::None,
            };
        }
        let started = *self.first_seen_active.get_or_insert(now);
        if now.saturating_duration_since(started) > self.max_active {
            if !engine_ready() {
                // Expected while the engine compiles: stay active, never end the hand-off early.
                return HandoffState {
                    active: true,
                    action: HandoffAction::None,
                };
            }
            self.first_seen_active = None;
            self.forced = true;
            return HandoffState {
                active: false,
                action: HandoffAction::ForceComplete,
            };
        }
        HandoffState {
            active: true,
            action: HandoffAction::None,
        }
    }
}
