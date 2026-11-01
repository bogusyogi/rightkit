//! Pure, event-driven hover scheduling for the Windows pill.
//!
//! The Windows pill used to learn about the cursor from a thread that slept
//! `PILL_HOVER_IDLE_POLL_MS` (90 ms) forever and hopped to the main thread on every
//! tick. Hover is now driven by mouse input: a low-level mouse hook feeds
//! cursor positions to [`HoverEdgeTracker`], which reports only the changes that can
//! alter the hover answer, and [`HoverPacer`] turns those reports into a bounded
//! number of main-thread passes. While the cursor is outside the pill and nothing
//! else changed, neither piece produces work and no thread has a timer armed.
//!
//! Both types are OS-free so the transition and pacing rules unit-test on any host.

use super::model::PILL_HOVER_ACTIVE_POLL_MS;

/// What one observed cursor position means for the hover pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoverSignal {
    /// Nothing the pill can react to changed (cursor still outside, or no change
    /// worth a pass). The hook sends nothing.
    Ignore,
    /// The cursor crossed the pill window rect or the pill hitbox in either
    /// direction. Run a pass immediately.
    Transition,
    /// The cursor moved while inside the hitbox (or inside the window while the
    /// pill is expanded): the renderer's per-button highlight needs the new
    /// position. Paced, with a trailing pass so the final position is never lost.
    Move,
}

/// Remembers which regions the cursor was in at the previous mouse event so only
/// crossings are reported as [`HoverSignal::Transition`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HoverEdgeTracker {
    inside_window: bool,
    inside_hitbox: bool,
}

impl HoverEdgeTracker {
    /// `inside_window`: the cursor is over the visible pill window rect.
    /// `inside_hitbox`: it is over the rendered capsule/action dock.
    /// `hovering`: the pill is currently expanded by hover (or the clipboard).
    pub fn observe(
        &mut self,
        inside_window: bool,
        inside_hitbox: bool,
        hovering: bool,
    ) -> HoverSignal {
        let crossed = inside_window != self.inside_window || inside_hitbox != self.inside_hitbox;
        self.inside_window = inside_window;
        self.inside_hitbox = inside_hitbox;
        if crossed {
            HoverSignal::Transition
        } else if inside_hitbox || (inside_window && hovering) {
            HoverSignal::Move
        } else {
            HoverSignal::Ignore
        }
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Requests delivered to the pacer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoverMsg {
    /// Run a pass now.
    Transition,
    /// Run a pass now if the gap since the last one has elapsed, else once when
    /// it does (trailing).
    Move,
    /// A pass left the debounce unsettled (the cursor left but `HoverGate` has
    /// not yet seen enough outside ticks): run one more after the gap.
    Settle,
}

/// Decides when a main-thread hover pass is requested.
///
/// All times are caller-supplied monotonic milliseconds. `next_deadline_ms` is
/// `None` whenever nothing is pending, which is the idle state: the owner must
/// then block without a timeout.
#[derive(Clone, Copy, Debug)]
pub struct HoverPacer {
    gap_ms: u64,
    last_pass_ms: Option<u64>,
    deadline_ms: Option<u64>,
}

impl Default for HoverPacer {
    fn default() -> Self {
        Self::new(PILL_HOVER_ACTIVE_POLL_MS)
    }
}

impl HoverPacer {
    pub const fn new(gap_ms: u64) -> Self {
        Self {
            gap_ms,
            last_pass_ms: None,
            deadline_ms: None,
        }
    }

    fn ran(&mut self, now_ms: u64) -> bool {
        self.last_pass_ms = Some(now_ms);
        self.deadline_ms = None;
        true
    }

    fn arm(&mut self, at_ms: u64) {
        self.deadline_ms = Some(match self.deadline_ms {
            Some(existing) if existing <= at_ms => existing,
            _ => at_ms,
        });
    }

    /// Returns `true` when a pass must be requested now.
    pub fn on_msg(&mut self, msg: HoverMsg, now_ms: u64) -> bool {
        match msg {
            HoverMsg::Transition => self.ran(now_ms),
            HoverMsg::Move => match self.last_pass_ms {
                Some(last) if now_ms.saturating_sub(last) < self.gap_ms => {
                    self.arm(last + self.gap_ms);
                    false
                }
                _ => self.ran(now_ms),
            },
            HoverMsg::Settle => {
                self.arm(now_ms + self.gap_ms);
                false
            }
        }
    }

    /// Call when the owner's timer fires. Returns `true` when a pass is due.
    pub fn on_timer(&mut self, now_ms: u64) -> bool {
        match self.deadline_ms {
            Some(deadline) if deadline <= now_ms => self.ran(now_ms),
            _ => false,
        }
    }

    /// Absolute time the owner must wake at; `None` means block indefinitely.
    pub fn next_deadline_ms(&self) -> Option<u64> {
        self.deadline_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_cursor_outside_the_pill_produces_no_signal() {
        let mut tracker = HoverEdgeTracker::default();
        for _ in 0..1000 {
            assert_eq!(tracker.observe(false, false, false), HoverSignal::Ignore);
        }
        // Even while the pill is expanded, a cursor outside the window is silent.
        for _ in 0..10 {
            assert_eq!(tracker.observe(false, false, true), HoverSignal::Ignore);
        }
    }

    #[test]
    fn entering_and_leaving_the_hitbox_are_transitions() {
        let mut tracker = HoverEdgeTracker::default();
        assert_eq!(tracker.observe(true, false, false), HoverSignal::Transition);
        assert_eq!(tracker.observe(true, true, false), HoverSignal::Transition);
        assert_eq!(tracker.observe(true, true, true), HoverSignal::Move);
        assert_eq!(tracker.observe(true, true, true), HoverSignal::Move);
        assert_eq!(tracker.observe(true, false, true), HoverSignal::Transition);
        assert_eq!(tracker.observe(false, false, true), HoverSignal::Transition);
        assert_eq!(tracker.observe(false, false, true), HoverSignal::Ignore);
    }

    #[test]
    fn window_headroom_is_silent_unless_the_pill_is_expanded() {
        let mut tracker = HoverEdgeTracker::default();
        assert_eq!(tracker.observe(true, false, false), HoverSignal::Transition);
        // Transparent headroom around a collapsed pill: no per-move work.
        assert_eq!(tracker.observe(true, false, false), HoverSignal::Ignore);
        // Once expanded, the renderer wants positions across the window.
        assert_eq!(tracker.observe(true, false, true), HoverSignal::Move);
    }

    #[test]
    fn reset_forgets_previous_regions() {
        let mut tracker = HoverEdgeTracker::default();
        tracker.observe(true, true, true);
        tracker.reset();
        assert_eq!(tracker.observe(true, true, true), HoverSignal::Transition);
    }

    #[test]
    fn transition_runs_immediately_even_inside_the_gap() {
        let mut pacer = HoverPacer::new(40);
        assert!(pacer.on_msg(HoverMsg::Transition, 100));
        assert!(pacer.on_msg(HoverMsg::Transition, 101));
        assert_eq!(pacer.next_deadline_ms(), None);
    }

    #[test]
    fn first_move_runs_and_moves_inside_the_gap_collapse_to_one_trailing_pass() {
        let mut pacer = HoverPacer::new(40);
        assert!(pacer.on_msg(HoverMsg::Move, 1000));
        // A burst of 1 kHz mouse samples inside the gap.
        for t in 1001..1030 {
            assert!(!pacer.on_msg(HoverMsg::Move, t));
        }
        // One trailing pass at last + gap, never earlier, never rescheduled later.
        assert_eq!(pacer.next_deadline_ms(), Some(1040));
        assert!(!pacer.on_timer(1039));
        assert!(pacer.on_timer(1040));
        assert_eq!(pacer.next_deadline_ms(), None);
        // After the trailing pass the pacer is idle again: the owner blocks.
        assert!(!pacer.on_timer(5000));
    }

    #[test]
    fn move_after_the_gap_runs_immediately() {
        let mut pacer = HoverPacer::new(40);
        assert!(pacer.on_msg(HoverMsg::Move, 0));
        assert!(pacer.on_msg(HoverMsg::Move, 40));
        assert_eq!(pacer.next_deadline_ms(), None);
    }

    #[test]
    fn settle_schedules_exactly_one_follow_up_and_keeps_the_earliest_deadline() {
        let mut pacer = HoverPacer::new(40);
        assert!(!pacer.on_msg(HoverMsg::Settle, 500));
        assert_eq!(pacer.next_deadline_ms(), Some(540));
        // A later settle never pushes an earlier deadline back.
        assert!(!pacer.on_msg(HoverMsg::Settle, 520));
        assert_eq!(pacer.next_deadline_ms(), Some(540));
        assert!(pacer.on_timer(540));
        assert_eq!(pacer.next_deadline_ms(), None);
    }

    #[test]
    fn leave_debounce_chain_ends_after_it_settles() {
        // Mirrors the main-thread pass: it asks for a Settle while HoverGate is
        // still hovering with the cursor outside, and stops asking once it leaves.
        let mut pacer = HoverPacer::new(40);
        let mut now = 0;
        assert!(pacer.on_msg(HoverMsg::Transition, now)); // pass 1
        let mut passes = 1;
        for _ in 0..2 {
            // HOVER_LEAVE_TICKS(3) - 1 further unsettled passes.
            pacer.on_msg(HoverMsg::Settle, now);
            now = pacer.next_deadline_ms().expect("settle armed");
            assert!(pacer.on_timer(now));
            passes += 1;
        }
        assert_eq!(passes, 3);
        assert_eq!(pacer.next_deadline_ms(), None, "chain must end, not recur");
    }

    #[test]
    fn a_transition_cancels_a_pending_trailing_pass() {
        let mut pacer = HoverPacer::new(40);
        assert!(pacer.on_msg(HoverMsg::Move, 0));
        assert!(!pacer.on_msg(HoverMsg::Move, 10));
        assert_eq!(pacer.next_deadline_ms(), Some(40));
        assert!(pacer.on_msg(HoverMsg::Transition, 20));
        assert_eq!(pacer.next_deadline_ms(), None);
    }
}
