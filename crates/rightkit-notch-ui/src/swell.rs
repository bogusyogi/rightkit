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

impl Default for Swell {
    fn default() -> Self {
        Self::new()
    }
}

/// A card's two swell values together: how far it has slid out of the notch (`slide`) and how far
/// it has opened (`open`, a beat behind on the way out and a beat ahead on the way in).
pub struct SwellPair {
    pub slide: Swell,
    pub open: Swell,
}

impl SwellPair {
    pub const fn new() -> Self {
        Self {
            slide: Swell::new(),
            open: Swell::new(),
        }
    }

    /// Back to nothing, at rest.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// The card comes out: the strip slides out now and opens a beat later.
    pub fn rise(&mut self) {
        self.slide.aim(1.0);
        self.open.aim_after(1.0);
    }

    /// The card goes back in: it closes now and the strip slides in a beat later.
    pub fn fall(&mut self) {
        self.open.aim(0.0);
        self.slide.aim_after(0.0);
    }

    /// A card already out is replaced: both values go to fully out now, a retarget from where
    /// they are.
    pub fn hold_out(&mut self) {
        self.slide.aim(1.0);
        self.open.aim(1.0);
    }

    /// One step of both to now; true while either still moves or waits to.
    pub fn step(&mut self) -> bool {
        let sliding = self.slide.step();
        let opening = self.open.step();
        sliding || opening
    }

    pub fn moving(&self) -> bool {
        self.slide.moving() || self.open.moving()
    }

    /// `(slide, open)` as they are now.
    pub fn values(&self) -> (f32, f32) {
        (self.slide.spring.value, self.open.spring.value)
    }
}

impl Default for SwellPair {
    fn default() -> Self {
        Self::new()
    }
}
