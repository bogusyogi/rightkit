//! The card swell: one of a hover card's two values on the bulb's spring.

use crate::motion;
use std::time::{Duration, Instant};

/// The longest real time one animation step of a card covers. The springs substep in 4 ms
/// (`motion::Spring::step`), so a long frame is stable; a cap shorter than the frames on a loaded
/// computer played the card's opening, closing and glide in slow motion (0.1 s per 300 ms frame
/// ran three times too long). Half a second only guards a stall.
pub const CARD_STEP_MAX: f32 = 0.5;
/// A card frame slower than this is logged (`card.frame_slow`, at most once a minute).
pub const CARD_FRAME_SLOW_MS: u128 = 50;

/// One of a card's two values (the Mac's `NoticeEmergence`, option A): on the bulb's spring
/// (0.42, 0.74), toward 1 while a card is up and toward 0 once it is put away, either at once
/// (`aim`) or a beat later (`aim_after`). A change of mind turns it round from where it is.
pub struct Swell {
    pub spring: motion::Spring,
    pub target: f32,
    pub last: Option<Instant>,
    /// A target to take up when its time comes, and when.
    pub due: Option<(Instant, f32)>,
}

impl Swell {
    pub const fn new() -> Self {
        Self {
            spring: motion::Spring::at(0.0),
            target: 0.0,
            last: None,
            due: None,
        }
    }

    /// Toward `target` now, dropping any aim still waiting.
    pub fn aim(&mut self, target: f32) {
        if !self.moving() {
            self.last = Some(Instant::now());
        }
        self.due = None;
        self.target = target;
    }

    /// Toward `target` after `motion::CARD_OPEN_DELAY`; until then it stays on its last aim.
    pub fn aim_after(&mut self, target: f32) {
        let delay = Duration::from_secs_f32(motion::CARD_OPEN_DELAY);
        self.due = Some((Instant::now() + delay, target));
    }

    pub fn moving(&self) -> bool {
        self.due.is_some() || !self.spring.resting_at(self.target)
    }

    /// One step to now; true while it still moves or waits to.
    pub fn step(&mut self) -> bool {
        let now = Instant::now();
        if let Some((at, target)) = self.due
            && now >= at
        {
            self.due = None;
            self.aim(target);
        }
        let seconds = self.last.map_or(0.0, |last| {
            now.duration_since(last).as_secs_f32().min(CARD_STEP_MAX)
        });
        self.last = Some(now);
        self.spring.step(self.target, motion::BULB, seconds);
        if self.spring.resting_at(self.target) {
            self.spring.settle(self.target);
            return self.due.is_some();
        }
        true
    }
}
