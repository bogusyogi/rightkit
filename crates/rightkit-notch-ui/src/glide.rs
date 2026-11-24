//! A hover card moving to another ring as one body (the Mac's card-motion option 4): the body
//! (centre, length, extent) glides on `motion::GLIDE`; the neck's anchor (the ring's own place along
//! the notch) follows on `motion::NECK_TRAIL`, `motion::NECK_TRAIL_DELAY` later, so the neck leans
//! behind and catches up; the old contents fade out as the new fade in over
//! `motion::CROSSFADE_SECONDS`. Each number is a spring of its own in pixels, so a change of mind
//! turns it round with the speed it has.
//!
//! `P` is the host's card content (whatever it draws a card from); it is only carried, never read.

use crate::motion;
use crate::render::Body;
use crate::swell::CARD_STEP_MAX;
use std::time::{Duration, Instant};

/// The two windows' union, `(left, top, width, height)`.
pub fn union_window(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> (i32, i32, i32, i32) {
    let (left, top) = (a.0.min(b.0), a.1.min(b.1));
    let (right, bottom) = ((a.0 + a.2).max(b.0 + b.2), (a.1 + a.3).max(b.1 + b.3));
    (left, top, right - left, bottom - top)
}

/// A card gliding from one body to another.
#[derive(Clone)]
pub struct Glide<P> {
    /// The window it is drawn in: both cards' windows, and so every place between.
    window: (i32, i32, i32, i32),
    centre: motion::Spring,
    length: motion::Spring,
    across: motion::Spring,
    tail: motion::Spring,
    /// Where it is going.
    pub to: Body,
    /// Where the card window ends up (its top-left).
    pub target: (i32, i32),
    /// Contents on their way out, each with the opacity it had when this fade began.
    old: Vec<(P, f32)>,
    /// The opacity the card on show had when this fade began (0 for a fresh glide).
    new_from: f32,
    fade_began: Instant,
    neck_begins: Instant,
    last: Instant,
}

impl<P: Clone> Glide<P> {
    /// A glide from `from` to `to`; `old` is the card that was on show and fades out.
    pub fn new(from: Body, to: Body, old: P) -> Self {
        let now = Instant::now();
        Self {
            window: union_window(from.window, to.window),
            centre: motion::Spring::at(from.centre),
            length: motion::Spring::at(from.length),
            across: motion::Spring::at(from.across),
            tail: motion::Spring::at(from.tail),
            target: (to.window.0, to.window.1),
            to,
            old: vec![(old, 1.0)],
            new_from: 0.0,
            fade_began: now,
            neck_begins: now + Duration::from_secs_f32(motion::NECK_TRAIL_DELAY),
            last: now,
        }
    }

    /// Turns round toward `to` from where it is, with the speed it has. `shown` is the card
    /// that was on show: it fades out from the opacity it had, as does everything still
    /// fading out, and the new card fades in from nothing.
    pub fn retarget(&mut self, to: Body, shown: P) {
        let was = self.new_level();
        let mut old = self.olds();
        old.push((shown, was));
        old.retain(|(_, level)| *level > 0.01);
        self.window = union_window(self.window, to.window);
        self.target = (to.window.0, to.window.1);
        self.to = to;
        self.old = old;
        self.new_from = 0.0;
        self.fade_began = Instant::now();
        // The neck's delay is for the first move only: a sweep from ring to ring keeps it
        // following without stalling at each turn.
    }

    /// One step; true once everything has arrived and the contents have finished crossing.
    pub fn step(&mut self) -> bool {
        let now = Instant::now();
        let seconds = now
            .duration_since(self.last)
            .as_secs_f32()
            .min(CARD_STEP_MAX);
        self.centre.step(self.to.centre, motion::GLIDE, seconds);
        self.length.step(self.to.length, motion::GLIDE, seconds);
        self.across.step(self.to.across, motion::GLIDE, seconds);
        // The neck starts after its delay, and steps only for the time since.
        if now > self.neck_begins {
            let since = now.duration_since(self.last.max(self.neck_begins));
            let since = since.as_secs_f32().min(CARD_STEP_MAX);
            self.tail.step(self.to.tail, motion::NECK_TRAIL, since);
        }
        self.last = now;
        let (distance, speed) = (0.05, 0.5);
        self.centre.resting_within(self.to.centre, distance, speed)
            && self.length.resting_within(self.to.length, distance, speed)
            && self.across.resting_within(self.to.across, distance, speed)
            && self.tail.resting_within(self.to.tail, distance, speed)
            && now > self.neck_begins
            && self.progress() >= 1.0
    }

    /// The body now.
    pub fn pose(&self) -> Body {
        Body {
            window: self.window,
            centre: self.centre.value,
            length: self.length.value,
            across: self.across.value,
            tail: self.tail.value,
        }
    }

    /// How far the crossfade has gone, 0 to 1, on a smoothstep.
    pub fn progress(&self) -> f32 {
        let t = (self.fade_began.elapsed().as_secs_f32() / motion::CROSSFADE_SECONDS).min(1.0);
        t * t * (3.0 - 2.0 * t)
    }

    /// The opacity of the card on show.
    pub fn new_level(&self) -> f32 {
        self.new_from + (1.0 - self.new_from) * self.progress()
    }

    /// The contents on their way out, each at its opacity now.
    pub fn olds(&self) -> Vec<(P, f32)> {
        let progress = self.progress();
        self.old
            .iter()
            .map(|(panel, from)| (panel.clone(), from * (1.0 - progress)))
            .collect()
    }
}
