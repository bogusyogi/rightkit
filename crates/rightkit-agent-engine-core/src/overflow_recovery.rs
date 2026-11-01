//! Reactive mid-stream context-overflow recovery.
//!
//! Atom `VG-002` (`ADAPT_TO_RUST`). Donor lineage: claude-code
//! `src/query/transitions.ts` (`reactive_compact_retry`) — a distinct
//! turn-loop state entered only when the *provider itself* rejects a
//! request mid-stream because the request already overflowed the context
//! window (a hard provider error), as opposed to:
//!
//! - proactive compaction, which runs *before* a request is sent because
//!   the host app's own token accounting predicted it would overflow (see
//!   `compaction_policy.rs`, `compactor.rs`); and
//! - output-limit recovery, which handles a response truncated by
//!   `max_tokens`, not a context-window rejection (see
//!   `compactor_output_budget.rs`).
//!
//! The donor's contract: on a reactive overflow, the turn loop transitions
//! into a bounded retry state that (1) forces an aggressive compaction of
//! the *already-sent* context rather than the lenient proactive policy,
//! (2) retries the same user turn against the freshly compacted context,
//! and (3) gives up after a small fixed number of attempts rather than
//! looping forever against a provider that keeps rejecting. This module
//! carries that state machine into Rust; it does not itself perform
//! compaction or call a provider — the engine's turn loop (hub-owned)
//! drives [`ReactiveOverflowRecovery`] and supplies the compaction/retry
//! side effects.

use serde::{Deserialize, Serialize};

/// Maximum number of reactive-recovery attempts before the turn loop
/// gives up and surfaces the overflow to the operator. Matches the
/// donor's small fixed retry bound — a provider that keeps rejecting
/// after aggressive compaction will not be fixed by trying a fourth time.
pub const MAX_REACTIVE_RETRIES: u8 = 1;

/// Distinct turn-loop state for reactive overflow recovery, kept
/// separate from proactive compaction and output-limit recovery so the
/// engine (and any UI/telemetry projecting turn state) can tell the three
/// apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReactiveOverflowState {
    /// No reactive recovery in progress. The default state for every
    /// turn; a turn that never overflows never leaves this state.
    Idle,
    /// The provider just rejected the in-flight request as a context
    /// overflow. The turn loop must not retry yet — it must first compact.
    Detected,
    /// Aggressive compaction of the already-sent context is running.
    Compacting,
    /// Compaction finished; the same user turn is being retried against
    /// the compacted context.
    Retrying { attempt: u8 },
    /// Every retry attempt was exhausted and the provider still rejects
    /// the (repeatedly compacted) context. The turn loop must surface
    /// this to the operator rather than loop again.
    Exhausted { attempts: u8 },
}

impl Default for ReactiveOverflowState {
    fn default() -> Self {
        ReactiveOverflowState::Idle
    }
}

impl ReactiveOverflowState {
    pub fn is_idle(&self) -> bool {
        matches!(self, ReactiveOverflowState::Idle)
    }

    pub fn is_recovering(&self) -> bool {
        matches!(
            self,
            ReactiveOverflowState::Detected
                | ReactiveOverflowState::Compacting
                | ReactiveOverflowState::Retrying { .. }
        )
    }

    pub fn is_exhausted(&self) -> bool {
        matches!(self, ReactiveOverflowState::Exhausted { .. })
    }
}

/// Typed reasons a transition was refused, so the turn loop can log or
/// surface exactly why a state change did not happen instead of silently
/// no-op'ing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransitionRefusal {
    /// Tried to mark an overflow detected while a recovery was already in
    /// flight. Reactive recovery is single-flight per turn.
    AlreadyRecovering,
    /// Tried to start compaction without first passing through
    /// `Detected`.
    NotDetected,
    /// Tried to retry without compaction having completed.
    NotCompacted,
    /// Tried to mark another overflow detected after the retry budget
    /// was already exhausted. The caller must surface the exhaustion,
    /// not keep cycling.
    AlreadyExhausted,
}

/// The bounded reactive-recovery state machine for one turn. Distinct
/// from proactive compaction and output-limit recovery per the donor
/// contract above.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactiveOverflowRecovery {
    state: ReactiveOverflowState,
    max_retries: u8,
    /// Retry attempts already spent this recovery cycle. Tracked
    /// independently of `state` because the attempt count carried by
    /// `Retrying`/`Exhausted` is unavailable once the machine has moved
    /// on to `Detected`/`Compacting` for the next attempt — deriving it
    /// from the current enum variant alone loses the count across that
    /// transition.
    attempts_used: u8,
}

impl Default for ReactiveOverflowRecovery {
    fn default() -> Self {
        Self::new(MAX_REACTIVE_RETRIES)
    }
}

impl ReactiveOverflowRecovery {
    pub fn new(max_retries: u8) -> Self {
        Self {
            state: ReactiveOverflowState::Idle,
            max_retries,
            attempts_used: 0,
        }
    }

    pub fn state(&self) -> ReactiveOverflowState {
        self.state
    }

    /// Whether the retry budget has already been spent. The turn loop checks
    /// this immediately after a provider rejects a retry, before starting
    /// another aggressive compaction pass.
    pub fn retry_budget_exhausted(&self) -> bool {
        self.attempts_used >= self.max_retries
    }

    /// Stop reactive recovery without performing another compaction or
    /// provider request. This is used when a retry itself overflows after the
    /// bounded retry budget has been consumed.
    pub fn exhaust(&mut self) -> ReactiveOverflowState {
        self.state = ReactiveOverflowState::Exhausted {
            attempts: self.attempts_used,
        };
        self.state
    }

    /// The provider rejected the in-flight request as a context overflow.
    /// Valid from `Idle` (first overflow this turn) or from `Retrying`
    /// (a retry attempt overflowed again, so recovery loops back to
    /// `Detected` for another compaction pass) — but never from
    /// `Exhausted`, and never re-entrant from `Detected`/`Compacting`
    /// themselves.
    pub fn overflow_detected(&mut self) -> Result<ReactiveOverflowState, TransitionRefusal> {
        match self.state {
            ReactiveOverflowState::Idle | ReactiveOverflowState::Retrying { .. } => {
                self.state = ReactiveOverflowState::Detected;
                Ok(self.state)
            }
            ReactiveOverflowState::Exhausted { .. } => Err(TransitionRefusal::AlreadyExhausted),
            ReactiveOverflowState::Detected | ReactiveOverflowState::Compacting => {
                Err(TransitionRefusal::AlreadyRecovering)
            }
        }
    }

    /// Aggressive compaction of the already-sent context has started.
    /// Valid only from `Detected`.
    pub fn begin_compaction(&mut self) -> Result<ReactiveOverflowState, TransitionRefusal> {
        match self.state {
            ReactiveOverflowState::Detected => {
                self.state = ReactiveOverflowState::Compacting;
                Ok(self.state)
            }
            _ => Err(TransitionRefusal::NotDetected),
        }
    }

    /// Compaction finished; the turn loop is about to retry the same
    /// user turn against the compacted context. Valid only from
    /// `Compacting`. If the retry budget is already used up, transitions
    /// straight to `Exhausted` instead of `Retrying` — the donor's retry
    /// loop checks the budget before issuing another attempt, not after.
    pub fn compaction_complete(&mut self) -> Result<ReactiveOverflowState, TransitionRefusal> {
        match self.state {
            ReactiveOverflowState::Compacting => {
                let next_attempt = self.attempts_used + 1;
                self.state = if next_attempt > self.max_retries {
                    ReactiveOverflowState::Exhausted {
                        attempts: self.attempts_used,
                    }
                } else {
                    self.attempts_used = next_attempt;
                    ReactiveOverflowState::Retrying {
                        attempt: next_attempt,
                    }
                };
                Ok(self.state)
            }
            _ => Err(TransitionRefusal::NotCompacted),
        }
    }

    /// The retried turn succeeded against the compacted context. Resets
    /// recovery back to `Idle` — a later overflow on a later turn starts
    /// a fresh recovery cycle with a fresh retry budget.
    pub fn recovered(&mut self) {
        self.state = ReactiveOverflowState::Idle;
        self.attempts_used = 0;
    }
}

/// Typed result of one reactive overflow recovery cycle (A043), recorded as
/// an `overflow_recovery` observation. Recovery compacts and retries at most
/// [`MAX_REACTIVE_RETRIES`] time(s) per cycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverflowRecoveryOutcome {
    /// The retried provider call succeeded against the compacted context.
    Recovered,
    /// The retry budget was spent; the overflow is surfaced as a failure.
    Exhausted,
    /// Reactive compaction itself failed or compacted nothing; no retry ran.
    CompactionFailed,
}

impl OverflowRecoveryOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recovered => "recovered",
            Self::Exhausted => "exhausted",
            Self::CompactionFailed => "compaction_failed",
        }
    }
}

/// Whether a provider error is a request-context rejection that can enter
/// reactive recovery. This deliberately excludes output-token exhaustion:
/// output-limit continuation remains a separate turn-loop path.
pub fn is_reactive_context_overflow(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    let explicit_context = error.contains("context_length_exceeded")
        || error.contains("context window")
        || error.contains("context length")
        || error.contains("maximum context")
        || error.contains("maximum input tokens")
        || error.contains("too many tokens")
        || error.contains("prompt_too_long")
        || error.contains("prompt is too long")
        || error.contains("input is too long")
        // CRA-10 pre-send admission: `PreparedRequestGateV1::admit` rejects a
        // prepared body that exceeds window-minus-reserve, and the typed
        // `CapacityError::FinalInsertionOverBudget` crosses the Llm seam as
        // this stable Display text (`ProviderError::Capacity` is
        // `#[error(transparent)]`). The rejection is context-size driven, so
        // it enters the same bounded compact-then-retry recovery instead of
        // falling through to a misclassified spend-budget `RunFailed`.
        || error.contains("final insertion over budget");
    // Claude Code's transition is keyed by a withheld 413 prompt-too-long
    // response. Provider adapters currently surface status + message as one
    // string, so require context/token wording alongside 413 to avoid
    // treating unrelated payload/media 413s as context overflow.
    explicit_context
        || (error.contains("413")
            && (error.contains("prompt") || error.contains("context") || error.contains("token")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_outcome_labels_are_stable_and_retry_is_at_most_once() {
        assert_eq!(OverflowRecoveryOutcome::Recovered.as_str(), "recovered");
        assert_eq!(OverflowRecoveryOutcome::Exhausted.as_str(), "exhausted");
        assert_eq!(
            OverflowRecoveryOutcome::CompactionFailed.as_str(),
            "compaction_failed"
        );
        let mut r = ReactiveOverflowRecovery::default();
        r.overflow_detected().unwrap();
        r.begin_compaction().unwrap();
        r.compaction_complete().unwrap();
        // A second overflow in the same cycle is refused: one retry only.
        assert!(r.retry_budget_exhausted());
    }

    #[test]
    fn starts_idle_and_not_recovering() {
        let recovery = ReactiveOverflowRecovery::default();
        assert!(recovery.state().is_idle());
        assert!(!recovery.state().is_recovering());
    }

    #[test]
    fn happy_path_detected_compacting_retrying_recovered() {
        let mut recovery = ReactiveOverflowRecovery::new(2);
        assert_eq!(
            recovery.overflow_detected().unwrap(),
            ReactiveOverflowState::Detected
        );
        assert!(recovery.state().is_recovering());
        assert_eq!(
            recovery.begin_compaction().unwrap(),
            ReactiveOverflowState::Compacting
        );
        assert_eq!(
            recovery.compaction_complete().unwrap(),
            ReactiveOverflowState::Retrying { attempt: 1 }
        );
        recovery.recovered();
        assert!(recovery.state().is_idle());
    }

    #[test]
    fn distinct_from_idle_it_is_a_separate_turn_loop_state() {
        // The donor contract requires this state be distinguishable from
        // both Idle (no recovery) and any proactive-compaction signal the
        // engine tracks separately — assert the recovering predicate is
        // true throughout Detected/Compacting/Retrying and false at the
        // Idle/Exhausted boundaries.
        let mut recovery = ReactiveOverflowRecovery::new(1);
        assert!(!recovery.state().is_recovering());
        recovery.overflow_detected().unwrap();
        assert!(recovery.state().is_recovering());
        recovery.begin_compaction().unwrap();
        assert!(recovery.state().is_recovering());
        recovery.compaction_complete().unwrap();
        assert!(recovery.state().is_recovering());
    }

    #[test]
    fn retry_budget_is_exhausted_after_max_retries() {
        let mut recovery = ReactiveOverflowRecovery::new(1);
        recovery.overflow_detected().unwrap();
        recovery.begin_compaction().unwrap();
        assert_eq!(
            recovery.compaction_complete().unwrap(),
            ReactiveOverflowState::Retrying { attempt: 1 }
        );
        // Retry overflowed again.
        recovery.overflow_detected().unwrap();
        recovery.begin_compaction().unwrap();
        assert_eq!(
            recovery.compaction_complete().unwrap(),
            ReactiveOverflowState::Exhausted { attempts: 1 }
        );
        assert!(recovery.state().is_exhausted());
    }

    #[test]
    fn exhausted_refuses_further_overflow_detection() {
        let mut recovery = ReactiveOverflowRecovery::new(0);
        recovery.overflow_detected().unwrap();
        recovery.begin_compaction().unwrap();
        assert_eq!(
            recovery.compaction_complete().unwrap(),
            ReactiveOverflowState::Exhausted { attempts: 0 }
        );
        assert_eq!(
            recovery.overflow_detected().unwrap_err(),
            TransitionRefusal::AlreadyExhausted
        );
    }

    #[test]
    fn cannot_reenter_detected_while_already_recovering() {
        let mut recovery = ReactiveOverflowRecovery::default();
        recovery.overflow_detected().unwrap();
        assert_eq!(
            recovery.overflow_detected().unwrap_err(),
            TransitionRefusal::AlreadyRecovering
        );
        recovery.begin_compaction().unwrap();
        assert_eq!(
            recovery.overflow_detected().unwrap_err(),
            TransitionRefusal::AlreadyRecovering
        );
    }

    #[test]
    fn cannot_begin_compaction_without_detection() {
        let mut recovery = ReactiveOverflowRecovery::default();
        assert_eq!(
            recovery.begin_compaction().unwrap_err(),
            TransitionRefusal::NotDetected
        );
    }

    #[test]
    fn cannot_complete_compaction_without_beginning_it() {
        let mut recovery = ReactiveOverflowRecovery::default();
        recovery.overflow_detected().unwrap();
        assert_eq!(
            recovery.compaction_complete().unwrap_err(),
            TransitionRefusal::NotCompacted
        );
    }

    #[test]
    fn recovered_resets_retry_budget_for_a_fresh_cycle() {
        let mut recovery = ReactiveOverflowRecovery::new(1);
        recovery.overflow_detected().unwrap();
        recovery.begin_compaction().unwrap();
        recovery.compaction_complete().unwrap(); // Retrying { attempt: 1 }
        recovery.recovered();
        assert!(recovery.state().is_idle());
        // A fresh overflow on a later turn gets a fresh attempt counter.
        recovery.overflow_detected().unwrap();
        recovery.begin_compaction().unwrap();
        assert_eq!(
            recovery.compaction_complete().unwrap(),
            ReactiveOverflowState::Retrying { attempt: 1 }
        );
    }

    #[test]
    fn zero_max_retries_exhausts_on_first_overflow() {
        let mut recovery = ReactiveOverflowRecovery::new(0);
        recovery.overflow_detected().unwrap();
        recovery.begin_compaction().unwrap();
        assert!(matches!(
            recovery.compaction_complete().unwrap(),
            ReactiveOverflowState::Exhausted { attempts: 0 }
        ));
    }

    #[test]
    fn cra10_admission_rejection_enters_reactive_recovery() {
        // Pin the stable marker the typed `CapacityError` emits at the
        // `EngineError::Llm` string boundary: an admission gate rejection is
        // a context overflow, never a spend-budget failure.
        assert!(is_reactive_context_overflow(
            "CRA-10 final insertion over budget: measured 150000 tokens exceeds effective budget 130000 tokens"
        ));
        // A missing capacity profile is a configuration gap that compaction
        // cannot fix — it must not enter recovery.
        assert!(!is_reactive_context_overflow(
            "CRA-10 capacity unavailable for route r1: capacity profile absent"
        ));
    }

    #[test]
    fn default_max_retries_matches_documented_constant() {
        let recovery = ReactiveOverflowRecovery::default();
        assert_eq!(MAX_REACTIVE_RETRIES, 1);
        assert_eq!(recovery.max_retries, MAX_REACTIVE_RETRIES);
    }
}
