//! Animation primitives: easing, closed-form springs, a fade, and the pose of an aux card.
//!
//! These are maths and bookkeeping only. How fast a spring is, how long a fade runs and what the
//! entrance of a card looks like are the painters' decisions (`look::Motion`); nothing here
//! carries a tuned value.

#![allow(dead_code)]

/// A progress curve the host chooses (`look::Motion::fade_curve`, `crossfade_curve`): it maps
/// progress `0..=1` to eased progress and should return `0` at `0` and `1` at `1` (validated at
/// install). The crate carries no curve of its own; [`smoothstep`] and [`cubic_bezier`] are plain
/// maths a host may use to build one.
pub type Curve = fn(f32) -> f32;

/// Run a host curve on progress clamped to `0..=1`; a non-finite answer counts as `0`.
pub(crate) fn apply_curve(curve: Curve, t: f32) -> f32 {
    let t = if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let eased = curve(t);
    if eased.is_finite() {
        eased
    } else {
        0.0
    }
}

/// `smoothstep`: the function `l * l * (3 - 2 * l)` on a clamped `0..=1` (maths, not a choice the
/// crate makes: nothing in the crate calls it).
pub fn smoothstep(t: f32) -> f32 {
    let l = t.clamp(0.0, 1.0);
    l * l * (3.0 - 2.0 * l)
}

/// CSS-style cubic bezier through (0,0), (x1,y1), (x2,y2), (1,1).
pub fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let bez = |a: f32, b: f32, u: f32| {
        let inv = 1.0 - u;
        3.0 * inv * inv * u * a + 3.0 * inv * u * u * b + u * u * u
    };
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let mut u = t;
    for _ in 0..16 {
        let x = bez(x1, x2, u);
        if (x - t).abs() < 1e-4 {
            break;
        }
        if x < t {
            lo = u;
        } else {
            hi = u;
        }
        u = (lo + hi) * 0.5;
    }
    bez(y1, y2, u)
}

// --- Springs ---------------------------------------------------------------------------------
//
// A damped spring described by `response` (the undamped period in seconds, omega = 2 pi /
// response) and `damping` (the damping ratio). Everything below is the exact
// closed-form solution of the damped oscillator, evaluated at an absolute time, so it is
// frame-rate independent (no integrator error, no per-frame state) and a retarget only needs the
// position and velocity at the instant it happens.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpringParams {
    pub response: f32,
    pub damping: f32,
}

/// A spring never runs longer than this: past it the value is exactly the target.
pub const SPRING_MAX_SECS: f32 = 1.5;

/// Displacement and velocity `t_sec` after a damped spring (rest position 0) was released from
/// displacement `x0` with velocity `v0`. The damping ratio is clamped to `0.05..=1.0` (never
/// overdamped, so no `cosh` can overflow) and the response to at least 20 ms. Non-finite input
/// yields `(0.0, 0.0)`, so the result is always finite.
pub fn spring_eval(x0: f32, v0: f32, t_sec: f32, p: SpringParams) -> (f32, f32) {
    let (x0, v0, t) = (f64::from(x0), f64::from(v0), f64::from(t_sec));
    if !x0.is_finite() || !v0.is_finite() || !t.is_finite() {
        return (0.0, 0.0);
    }
    if t <= 0.0 {
        return (x0 as f32, v0 as f32);
    }
    let zeta = f64::from(p.damping).clamp(0.05, 1.0);
    let w = std::f64::consts::TAU / f64::from(p.response.max(0.02));
    let a = zeta * w;
    let decay = (-a * t).exp();
    let (x, v) = if zeta >= 0.9999 {
        let c = v0 + w * x0;
        (decay * (x0 + c * t), decay * (v0 - w * c * t))
    } else {
        let wd = w * (1.0 - zeta * zeta).sqrt();
        let (s, c) = (wd * t).sin_cos();
        (
            decay * (x0 * c + ((v0 + a * x0) / wd) * s),
            decay * (v0 * c - ((a * v0 + w * w * x0) / wd) * s),
        )
    };
    if x.is_finite() && v.is_finite() {
        (x as f32, v as f32)
    } else {
        (0.0, 0.0)
    }
}

/// `0 -> 1` unit step response of a spring `t_sec` after the step (the entrance progress).
pub fn spring_unit(t_sec: f32, p: SpringParams) -> f32 {
    1.0 + spring_eval(-1.0, 0.0, t_sec, p).0
}

/// One value chasing a target on a spring. Retargeting keeps the current velocity, so an
/// interrupted motion (hover in then out, a mode change mid-resize) never hitches.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpringAnim {
    from: f32,
    vel: f32,
    target: f32,
    start_ms: f64,
    params: SpringParams,
    /// Settled when within `eps` of the target and slower than `8 * eps` per second.
    eps: f32,
}

impl SpringAnim {
    pub const fn at(value: f32, params: SpringParams, eps: f32) -> SpringAnim {
        SpringAnim {
            from: value,
            vel: 0.0,
            target: value,
            start_ms: 0.0,
            params,
            eps,
        }
    }

    pub fn target(&self) -> f32 {
        self.target
    }

    /// Where the motion in flight started (the position at the last retarget).
    pub fn start_value(&self) -> f32 {
        self.from
    }

    /// `(position, velocity, settled)` at `now_ms`.
    fn sample(&self, now_ms: f64) -> (f32, f32, bool) {
        let t = ((now_ms - self.start_ms) / 1000.0).max(0.0) as f32;
        if t >= SPRING_MAX_SECS {
            return (self.target, 0.0, true);
        }
        let (x, v) = spring_eval(self.from - self.target, self.vel, t, self.params);
        if x.abs() <= self.eps && v.abs() <= self.eps * 8.0 {
            (self.target, 0.0, true)
        } else {
            (self.target + x, v, false)
        }
    }

    pub fn value(&self, now_ms: f64) -> f32 {
        self.sample(now_ms).0
    }

    pub fn done(&self, now_ms: f64) -> bool {
        self.sample(now_ms).2
    }

    /// Chase `target` from the current position at the current velocity. A repeat of the same
    /// target is a no-op, so the motion in flight is not restarted.
    pub fn retarget(&mut self, target: f32, now_ms: f64, params: SpringParams) {
        if (self.target - target).abs() < f32::EPSILON {
            return;
        }
        let (position, velocity, _) = self.sample(now_ms);
        *self = SpringAnim {
            from: position,
            vel: velocity,
            target,
            start_ms: now_ms,
            params,
            eps: self.eps,
        };
    }

    /// Jump to `value` with no motion.
    pub fn snap(&mut self, value: f32) {
        self.from = value;
        self.vel = 0.0;
        self.target = value;
    }

    /// Milliseconds after the last retarget at which the motion is settled (at most
    /// `SPRING_MAX_SECS`), found by scanning the closed form in 4 ms steps.
    pub fn settle_after_ms(&self) -> f64 {
        let mut k = 0.0f64;
        while k < f64::from(SPRING_MAX_SECS) * 1000.0 {
            if self.done(self.start_ms + k) {
                return k;
            }
            k += 4.0;
        }
        f64::from(SPRING_MAX_SECS) * 1000.0
    }
}

/// Pose of an aux card (hint or clip) at one instant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AuxPose {
    /// Whole-card opacity.
    pub alpha: f32,
    /// Scale about the card centre.
    pub scale: f32,
    /// Opacity of the card's content over its shell (a content swap fades in).
    pub content_alpha: f32,
    /// Milliseconds since an entrance from rest began, for list stagger; `f32::MAX` when not
    /// entering.
    pub rows_ms: f32,
}

impl AuxPose {
    pub const SETTLED: AuxPose = AuxPose {
        alpha: 1.0,
        scale: 1.0,
        content_alpha: 1.0,
        rows_ms: f32::MAX,
    };
}

/// Entrance and exit of an aux card as one spring on `0..=1` (`1` = fully shown). Leaving mid
/// entrance (or entering mid exit) reverses with the velocity it had. The spring parameters, the
/// entrance scale and the row-stagger window come from the host's `Motion`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Presence {
    spring: SpringAnim,
    /// False when the OS "show animations" setting is off: a short fade, no scale.
    motion: bool,
    entered_ms: Option<f64>,
}

impl Presence {
    fn params(motion: bool) -> SpringParams {
        let m = crate::config::motion();
        if motion {
            m.card_spring
        } else {
            m.reduced_spring
        }
    }

    pub fn entering(now_ms: f64, motion: bool) -> Presence {
        let mut spring = SpringAnim::at(
            0.0,
            Presence::params(motion),
            crate::config::motion().card_spring_eps,
        );
        spring.retarget(1.0, now_ms, Presence::params(motion));
        Presence {
            spring,
            motion,
            entered_ms: Some(now_ms),
        }
    }

    pub fn leave(&mut self, now_ms: f64) {
        self.spring
            .retarget(0.0, now_ms, Presence::params(self.motion));
        self.entered_ms = None;
    }

    pub fn is_leaving(&self) -> bool {
        self.spring.target() < 0.5
    }

    pub fn animating(&self, now_ms: f64) -> bool {
        !self.spring.done(now_ms)
    }

    /// Time the exit needs from the `leave` call.
    pub fn exit_after_ms(&self) -> f64 {
        self.spring.settle_after_ms()
    }

    pub fn pose(&self, now_ms: f64) -> AuxPose {
        let m = crate::config::motion();
        let p = self.spring.value(now_ms);
        let rows_ms = match self.entered_ms {
            Some(start)
                if self.motion
                    && !self.is_leaving()
                    && now_ms - start < f64::from(m.card_rows_window_ms) =>
            {
                (now_ms - start) as f32
            }
            _ => f32::MAX,
        };
        AuxPose {
            alpha: p.clamp(0.0, 1.0),
            scale: if self.motion {
                m.card_scale_base + m.card_scale_gain * p.clamp(-0.6, 1.4)
            } else {
                1.0
            },
            content_alpha: 1.0,
            rows_ms,
        }
    }
}

/// A scalar eased between two values over a fixed time, along the host's `Motion::fade_curve`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fade {
    pub from: f32,
    pub to: f32,
    pub start_ms: f64,
    pub dur_ms: f32,
}

impl Fade {
    pub const fn at(value: f32) -> Fade {
        Fade {
            from: value,
            to: value,
            start_ms: 0.0,
            dur_ms: 0.0,
        }
    }

    pub fn value(&self, now_ms: f64) -> f32 {
        if self.dur_ms <= 0.0 {
            return self.to;
        }
        let t = ((now_ms - self.start_ms) / f64::from(self.dur_ms)) as f32;
        self.from + (self.to - self.from) * apply_curve(crate::config::motion().fade_curve, t)
    }

    pub fn done(&self, now_ms: f64) -> bool {
        self.dur_ms <= 0.0 || now_ms >= self.start_ms + f64::from(self.dur_ms)
    }

    /// Retarget from the current value (an interrupted fade continues from where it is).
    pub fn retarget(&mut self, to: f32, now_ms: f64, dur_ms: f32) {
        if (self.to - to).abs() < f32::EPSILON {
            return;
        }
        let current = self.value(now_ms);
        *self = Fade {
            from: current,
            to,
            start_ms: now_ms,
            dur_ms,
        };
    }
}

/// What the capsule is showing (the render mode, driven by `PillEvent`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    Idle,
    Recording,
    Processing,
    Status,
    Error,
}

/// Entrance progress using the host's spring and stagger, with `f32::MAX` fully visible.
pub fn row_progress(rows_ms: f32, index: usize, step_ms: f32, cap_ms: f32, spring: SpringParams) -> f32 {
    if rows_ms >= f32::MAX {
        return 1.0;
    }
    let delay = (index as f32 * step_ms).min(cap_ms);
    spring_unit((rows_ms - delay) / 1000.0, spring).clamp(0.0, 1.0)
}

/// Smoothly rise to `1` at `peak` and then fall back to `0`. Invalid peaks have no envelope.
pub fn peak_envelope(progress: f32, peak: f32) -> f32 {
    if !progress.is_finite() || !(0.0 < peak && peak < 1.0) {
        return 0.0;
    }
    let progress = progress.clamp(0.0, 1.0);
    let phase = if progress < peak {
        progress / peak
    } else {
        1.0 - (progress - peak) / (1.0 - peak)
    };
    phase * phase * (3.0 - 2.0 * phase)
}
