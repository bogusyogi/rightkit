//! The notch's own motion (the Mac's `NotchMotion`): springs and eases stepped on a 16 ms
//! timer while anything moves, and nothing between. Liquid-motion pass (the liquid-motion pass):
//! the hovered ring grows to 1.12 and the others dull to 45%; the notch gathers itself about
//! its end rings while the handle is hovered (1) or the notch is carried (1.12).

use std::f32::consts::TAU;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

/// A value on a spring: `step` moves it toward `target`. `(response, damping)` are SwiftUI's
/// `spring(response:dampingFraction:)`.
#[derive(Clone, Copy, Default)]
pub struct Spring {
    pub value: f32,
    speed: f32,
}

impl Spring {
    pub const fn at(value: f32) -> Self {
        Self { value, speed: 0.0 }
    }

    pub fn step(&mut self, target: f32, (response, damping): (f32, f32), seconds: f32) {
        let omega = TAU / response;
        // Small steps keep the sum stable whatever the frame took.
        let steps = (seconds / 0.004).ceil().max(1.0) as usize;
        let dt = seconds / steps as f32;
        for _ in 0..steps {
            let pull = omega * omega * (target - self.value) - 2.0 * damping * omega * self.speed;
            self.speed += pull * dt;
            self.value += self.speed * dt;
        }
    }

    pub fn resting_at(&self, target: f32) -> bool {
        (self.value - target).abs() < 0.001 && self.speed.abs() < 0.001
    }

    /// Within `distance` of `target` and slower than `speed`: for a value in pixels, where
    /// `resting_at`'s 0.001 is too fine to wait for.
    pub fn resting_within(&self, target: f32, distance: f32, speed: f32) -> bool {
        (self.value - target).abs() < distance && self.speed.abs() < speed
    }

    /// Puts it at `target` with no speed.
    pub fn settle(&mut self, target: f32) {
        *self = Self::at(target);
    }
}

/// The hovered ring growing to 1.12 about its own centre: `NotchMotion.emphasis`.
pub const EMPHASIS: (f32, f32) = (0.3, 0.7);
pub const EMPHASIS_SCALE: f32 = 1.12;
/// The other rings dulling to 45% and coming back: `NotchMotion.dull`, ease in-out 0.14 s.
pub const DULL_SECONDS: f32 = 0.14;
pub const DULLED: f32 = 0.45;
/// The notch gathering itself and letting go: `NotchMotion.gather`. Bouncy on purpose:
/// released, the ends puff back out past where they were before they settle.
pub const GATHER: (f32, f32) = (0.45, 0.62);
/// A hover card gliding from one ring to the next as one body: `NotchMotion.glide`.
pub const GLIDE: (f32, f32) = (0.5, 0.86);
/// The neck's anchor on a gliding card: softer than the body's spring and started
/// `NECK_TRAIL_DELAY` later, so the neck leans behind the card and catches up (the Mac's
/// `NotchMotion.neckTrail`, card-motion spec section 2).
pub const NECK_TRAIL: (f32, f32) = (0.62, 0.7);
pub const NECK_TRAIL_DELAY: f32 = 0.05;
/// A gliding card's old and new contents crossfading, a smoothstep over the same time (the
/// Mac's `NotchMotion.crossfade`, an ease in-out).
pub const CROSSFADE_SECONDS: f32 = 0.16;
pub const HUG_HOVER: f32 = 1.0;
pub const HUG_CARRIED: f32 = 1.12;
/// The handle's bulb coming out of the foot and going back in: `NotchMotion.bulb`.
pub const BULB: (f32, f32) = (0.42, 0.74);
/// A card's second value (opening, or sliding back in) starts this many seconds after its first:
/// `NotchMotion.noticeOpenDelay`.
pub const CARD_OPEN_DELAY: f32 = 0.16;
/// The bulb squeezing to 0.86 as the notch is taken hold of, and giving way: `NotchMotion.squeeze`.
pub const SQUEEZE: (f32, f32) = (0.28, 0.7);
pub const CARRIED_SQUEEZE: f32 = 0.86;
/// A click's dip: down to 0.9 on SwiftUI's `.snappy(duration: 0.09)`, back on
/// `.bouncy(duration: 0.34)` (springs of that response with their default bounce, 0.15 and 0.3).
pub const DIP: f32 = 0.9;
pub const DIP_DOWN: (f32, f32) = (0.09, 0.85);
pub const DIP_BACK: (f32, f32) = (0.34, 0.7);
const DIP_SECONDS: f32 = 0.09;

/// What a notch should show: the ring under the pointer, and how far it is gathered.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Targets {
    pub hovered: Option<usize>,
    pub hug: f32,
    /// 1 with the bulb out (the pointer in the handle's zone, or the notch carried), else 0.
    pub bulb: f32,
    /// 1 with the minimise button out (the pointer in its zone), else 0.
    pub mini: f32,
    pub carried: bool,
}

/// How the rings and the body are drawn this frame: per ring its scale and opacity, the body's
/// gather, and the handle's bulb (how far out, and its squeeze). Compared for equality, so a
/// frame identical to the last is not drawn again.
#[derive(Clone, Debug, PartialEq)]
pub struct Pose {
    pub rings: Vec<(f32, f32)>,
    pub hug: f32,
    pub bulb: f32,
    /// How far the minimise button is out of the start ring's cap, as `bulb` is at the end.
    pub mini: f32,
    pub squeeze: f32,
    /// The warning band's glow, 0..1 on a slow pulse (in steps, so identical frames compare equal);
    /// 0 while no gauge is past its warning level.
    pub warn: f32,
}

impl Default for Pose {
    fn default() -> Self {
        Self {
            rings: Vec::new(),
            hug: 0.0,
            bulb: 0.0,
            mini: 0.0,
            squeeze: 1.0,
            warn: 0.0,
        }
    }
}

impl Pose {
    /// Scale and opacity of ring `index` (plain when the pose has none for it).
    pub fn ring(&self, index: usize) -> (f32, f32) {
        self.rings.get(index).copied().unwrap_or((1.0, 1.0))
    }

    /// Nothing emphasised, dulled, gathered or out.
    pub fn at_rest(&self) -> bool {
        self.hug == 0.0
            && self.bulb == 0.0
            && self.mini == 0.0
            && self.warn == 0.0
            && self.squeeze == 1.0
            && self.rings.iter().all(|&ring| ring == (1.0, 1.0))
    }
}

struct Notch {
    key: isize,
    targets: Targets,
    scale: Vec<Spring>,
    /// 0 plain, 1 dulled, moving linearly; the opacity eases along it.
    dull: Vec<f32>,
    hug: Spring,
    bulb: Spring,
    mini: Spring,
    squeeze: Spring,
    /// What the warning glow last drew at (`warn_level`), to tell when a frame has to be drawn.
    warn_seen: f32,
    /// When the last click's dip went down, and whether its bounce back has yet to settle.
    dip_at: Option<Instant>,
    dipped: bool,
    last: Instant,
}

impl Notch {
    fn new(key: isize) -> Self {
        Self {
            key,
            targets: Targets::default(),
            scale: Vec::new(),
            dull: Vec::new(),
            hug: Spring::default(),
            bulb: Spring::default(),
            mini: Spring::default(),
            squeeze: Spring::at(1.0),
            warn_seen: 0.0,
            dip_at: None,
            dipped: false,
            last: Instant::now(),
        }
    }

    fn fit(&mut self, count: usize) {
        self.scale.resize(count, Spring::at(1.0));
        self.dull.resize(count, 0.0);
    }

    fn scale_target(&self, index: usize) -> f32 {
        if self.targets.hovered == Some(index) {
            EMPHASIS_SCALE
        } else {
            1.0
        }
    }

    fn dull_target(&self, index: usize) -> f32 {
        match self.targets.hovered {
            Some(hovered) if hovered != index => 1.0,
            _ => 0.0,
        }
    }

    /// The squeeze's target and spring: 0.86 carried, 0.9 in a click's dip, else 1.
    fn squeeze_goal(&self) -> (f32, (f32, f32)) {
        if self.targets.carried {
            (CARRIED_SQUEEZE, SQUEEZE)
        } else if self
            .dip_at
            .is_some_and(|at| at.elapsed().as_secs_f32() < DIP_SECONDS)
        {
            (DIP, DIP_DOWN)
        } else if self.dipped {
            (1.0, DIP_BACK)
        } else {
            (1.0, SQUEEZE)
        }
    }

    fn resting(&self) -> bool {
        self.dip_at.is_none()
            && self.bulb.resting_at(self.targets.bulb)
            && self.mini.resting_at(self.targets.mini)
            && self.squeeze.resting_at(self.squeeze_goal().0)
            && self.hug.resting_at(self.targets.hug)
            && (0..self.scale.len()).all(|i| {
                self.scale[i].resting_at(self.scale_target(i))
                    && self.dull[i] == self.dull_target(i)
            })
    }

    fn step(&mut self, now: Instant) {
        // A long gap (the timer was off) is not one huge step.
        let seconds = now.duration_since(self.last).as_secs_f32().min(0.1);
        self.last = now;
        for i in 0..self.scale.len() {
            let target = self.scale_target(i);
            self.scale[i].step(target, EMPHASIS, seconds);
            if self.scale[i].resting_at(target) {
                self.scale[i].settle(target);
            }
            let target = self.dull_target(i);
            let reach = seconds / DULL_SECONDS;
            self.dull[i] = if self.dull[i] < target {
                (self.dull[i] + reach).min(target)
            } else {
                (self.dull[i] - reach).max(target)
            };
        }
        self.hug.step(self.targets.hug, GATHER, seconds);
        if self.hug.resting_at(self.targets.hug) {
            self.hug.settle(self.targets.hug);
        }
        self.bulb.step(self.targets.bulb, BULB, seconds);
        if self.bulb.resting_at(self.targets.bulb) {
            self.bulb.settle(self.targets.bulb);
        }
        self.mini.step(self.targets.mini, BULB, seconds);
        if self.mini.resting_at(self.targets.mini) {
            self.mini.settle(self.targets.mini);
        }
        let (goal, spring) = self.squeeze_goal();
        if self
            .dip_at
            .is_some_and(|at| at.elapsed().as_secs_f32() >= DIP_SECONDS)
        {
            self.dip_at = None;
        }
        self.squeeze.step(goal, spring, seconds);
        if self.squeeze.resting_at(goal) {
            self.squeeze.settle(goal);
            if goal == 1.0 {
                self.dipped = false;
            }
        }
    }

    fn pose(&self) -> Pose {
        let ease = |t: f32| t * t * (3.0 - 2.0 * t);
        Pose {
            rings: (0..self.scale.len())
                .map(|i| {
                    (
                        self.scale[i].value,
                        1.0 - (1.0 - DULLED) * ease(self.dull[i]),
                    )
                })
                .collect(),
            hug: self.hug.value,
            bulb: self.bulb.value,
            mini: self.mini.value,
            squeeze: self.squeeze.value,
            warn: warn_level(),
        }
    }
}

static NOTCHES: Mutex<Vec<Notch>> = Mutex::new(Vec::new());

/// Sets what notch `key` (with `count` rings) should show. True when it now has to move: the
/// caller keeps the frame timer running until `step` says everything is at rest.
pub fn retarget(key: isize, count: usize, targets: Targets) -> bool {
    let mut notches = NOTCHES.lock().unwrap_or_else(PoisonError::into_inner);
    let index = match notches.iter().position(|n| n.key == key) {
        Some(index) => index,
        None => {
            notches.push(Notch::new(key));
            notches.len() - 1
        }
    };
    let notch = &mut notches[index];
    notch.fit(count);
    if notch.resting() {
        // Starting from rest: the first step measures from now.
        notch.last = Instant::now();
    }
    notch.targets = targets;
    !notch.resting()
}

/// One frame for every notch. Returns the notches that moved, and whether any still moves.
pub fn step() -> (Vec<isize>, bool) {
    let now = Instant::now();
    let mut notches = NOTCHES.lock().unwrap_or_else(PoisonError::into_inner);
    let mut moved = Vec::new();
    let glow = warn_level();
    for notch in notches.iter_mut() {
        let glows = glow != notch.warn_seen;
        if notch.resting() && !glows {
            continue;
        }
        notch.warn_seen = glow;
        notch.step(now);
        moved.push(notch.key);
    }
    let moving = notches.iter().any(|n| !n.resting());
    (moved, moving)
}

/// How notch `key` is drawn now.
pub fn pose(key: isize) -> Pose {
    NOTCHES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|n| n.key == key)
        .map(Notch::pose)
        .unwrap_or_default()
}

/// A click on notch `key`'s bulb: it dips and springs back. True: the frame timer must run.
pub fn dip(key: isize) -> bool {
    let mut notches = NOTCHES.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(notch) = notches.iter_mut().find(|n| n.key == key) else {
        return false;
    };
    if notch.resting() {
        notch.last = Instant::now();
    }
    notch.dip_at = Some(Instant::now());
    notch.dipped = true;
    true
}

/// Forgets a notch whose window is gone.
pub fn forget(key: isize) {
    NOTCHES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|n| n.key != key);
}

/// A gauge is past its warning level: the Send cell's band pulses.
static WARNING: AtomicBool = AtomicBool::new(false);
/// When the pulse started, so every notch glows in step.
static GLOW_FROM: Mutex<Option<Instant>> = Mutex::new(None);
/// One slow breath of the warning band, in seconds, and the steps it is drawn in.
const GLOW_PERIOD: f32 = 3.2;
const GLOW_STEPS: f32 = 8.0;

/// Turns the warning pulse on or off.
pub fn set_warning(on: bool) {
    if WARNING.swap(on, Ordering::Relaxed) == on {
        return;
    }
    *GLOW_FROM.lock().unwrap_or_else(PoisonError::into_inner) = on.then(Instant::now);
}

/// Whether the warning pulse is on, so the frame timer has to keep running.
pub fn pulsing() -> bool {
    WARNING.load(Ordering::Relaxed)
        && !PANELS_HIDDEN.load(Ordering::Relaxed)
        && crate::layout::minimised()
}

/// Every notch is hidden (switch, full-screen app) and no toast reveals one: nothing is on
/// screen to pulse, so the frame timer stays off.
static PANELS_HIDDEN: AtomicBool = AtomicBool::new(false);

/// Records whether every notch is hidden; the caller restarts the pulse timer when it clears.
pub fn set_panels_hidden(hidden: bool) {
    PANELS_HIDDEN.store(hidden, Ordering::Relaxed);
}

/// The glow now: 0 with no warning, else a slow cosine breath in `GLOW_STEPS` steps.
fn warn_level() -> f32 {
    if !pulsing() {
        return 0.0;
    }
    let from = *GLOW_FROM.lock().unwrap_or_else(PoisonError::into_inner);
    let seconds = from.map_or(0.0, |at| at.elapsed().as_secs_f32());
    let breath = 0.5 - 0.5 * (TAU * seconds / GLOW_PERIOD).cos();
    // Never exactly 0 while the warning lasts, so "no warning" stays the one rest state.
    ((breath * GLOW_STEPS).round() / GLOW_STEPS).max(1.0 / GLOW_STEPS)
}
