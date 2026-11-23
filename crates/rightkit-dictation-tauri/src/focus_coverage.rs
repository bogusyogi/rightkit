//! Reference-counted lifetime of the on-demand focus-continuity observer.
//!
//! The observer runs only while a dictation needs it, so an idle app wakes no focus thread. A
//! dictation needs it from just before the start capture until its terminal state is committed
//! (delivery finishes inside `Transcribing`; the terminal state follows it).
//!
//!   begin: [`FocusCoverage::start_begin`] at the top of the start path (every start funnels through
//!          it), before the hotkey anchor is read.
//!   end:   [`FocusCoverage::reconcile`], which ends coverage only when no start is in flight AND
//!          the authoritative runtime state is not Recording, Transcribing or Pasting. It runs when
//!          a start finishes and on every committed state emission, so delivery, cancel, error and
//!          a failed start all end coverage through one rule, and a session that started while an
//!          older one was still delivering keeps it.
//!
//! The start counter serializes "a start arms" against "reconcile ends": without it a late `Armed`
//! emission for the previous dictation could end the coverage a start had just armed. Lock order is
//! counter, then platform locks; the counter is a leaf (no runtime lock and no emission guard is
//! taken while it is held).

use std::sync::{Mutex, MutexGuard};

use rightkit_dictation_core::state::AppState;

/// The platform observer the counter drives.
pub struct FocusCoverageHost {
    /// Arm coverage. It does not wait for the first sample: nothing may sit in front of the
    /// microphone start.
    pub arm: fn(),
    /// End coverage.
    pub end: fn(),
    /// Whether coverage is currently wanted.
    pub wanted: fn() -> bool,
    /// The lock-free authoritative runtime state; `None` before the runtime is opened.
    pub runtime_state: fn() -> Option<AppState>,
}

/// True for the states during which the observer must keep covering.
pub fn state_needs_focus_coverage(state: &AppState) -> bool {
    matches!(
        state,
        AppState::Recording { .. } | AppState::Transcribing { .. } | AppState::Pasting { .. }
    )
}

/// The count of starts in flight. One per process.
pub struct FocusCoverage {
    starts: Mutex<u32>,
}

impl FocusCoverage {
    pub const fn new() -> Self {
        Self {
            starts: Mutex::new(0),
        }
    }

    fn gate(&self) -> MutexGuard<'_, u32> {
        self.starts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Arm coverage for a dictation that is about to start. Must be paired with
    /// [`Self::start_finish`].
    pub fn start_begin(&self, host: &FocusCoverageHost) {
        let mut starts = self.gate();
        *starts += 1;
        (host.arm)();
    }

    /// The start sequence is over (success or failure). `outcome` is the state the start produced,
    /// used only when the authoritative state is unavailable.
    pub fn start_finish(&self, host: &FocusCoverageHost, outcome: Option<&AppState>) {
        {
            let mut starts = self.gate();
            *starts = starts.saturating_sub(1);
        }
        self.reconcile(host, outcome);
    }

    /// End coverage unless a start is in flight or a dictation is still active. `hint` is consulted
    /// only when the lock-free runtime state is unavailable.
    pub fn reconcile(&self, host: &FocusCoverageHost, hint: Option<&AppState>) {
        let starts = self.gate();
        if *starts > 0 {
            return;
        }
        let active = (host.runtime_state)()
            .as_ref()
            .or(hint)
            .is_some_and(state_needs_focus_coverage);
        if !active && (host.wanted)() {
            (host.end)();
        }
    }
}

impl Default for FocusCoverage {
    fn default() -> Self {
        Self::new()
    }
}
