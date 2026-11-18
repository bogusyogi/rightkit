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
