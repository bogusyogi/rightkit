//! Keyframed tracks and easing. Keyframes are placed at layer-local [`Ticks`].

use serde::{Deserialize, Serialize};

use crate::model::{Rect, Transform};
use crate::ticks::Ticks;

/// A keyframed property. An empty track means "use the property default".
pub type Track<T> = Vec<Keyframe<T>>;

/// Lower bound of the bounce accepted by [`Easing::CameraSpring`]. Values
/// outside it are clamped at evaluation, as ScreenRight's `springProgress` does.
pub const CAMERA_BOUNCE_MAX: f64 = 0.3;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Keyframe<T> {
    /// Layer-local time in ticks.
    #[serde(rename = "at_ticks")]
    pub at: Ticks,
    pub value: T,
    /// Easing of the segment that starts at this keyframe and ends at the next.
    #[serde(default)]
    pub easing: Easing,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Easing {
    #[default]
    Linear,
    /// Holds this keyframe's value until the next keyframe (a step).
    Hold,
    EaseIn,
    EaseOut,
    EaseInOut,
    /// CSS-style cubic bezier with control points `(x1, y1)` and `(x2, y2)`.
    /// `x1` and `x2` must lie in 0..=1.
    CubicBezier {
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
    },
    /// Step response of a damped mass-spring released from rest, travelling
    /// from 0 to 1 over the segment. The segment's real duration is the
    /// physical time base: `t = u * span` seconds. The damping ratio is
    /// `damping / (2 * sqrt(stiffness * mass))`. A ratio of 1 or more never
    /// overshoots.
    Spring {
        /// Spring constant; must be positive.
        stiffness: f64,
        /// Non-negative viscous damping coefficient.
        damping: f64,
        /// Positive mass.
        mass: f64,
    },
    /// Half-cosine ease: `0.5 - 0.5 * cos(pi * u)`. Span-independent.
    Cosine,
    /// ScreenRight's camera curve, a unit-time step response that depends on
    /// `u` only. `bounce` is clamped to `0..=0.3`. Zero is critically damped,
    /// `1 - (1 + 9.23u) e^(-9.23u)`. Above zero the response is underdamped
    /// with `zeta = 1 - bounce` and `omega = 6.9 / zeta`, and it overshoots by
    /// up to `e^(-pi zeta / sqrt(1 - zeta^2))`. The endpoint is pinned to
    /// exactly 1 at `u >= 1`, as the source does. Span-independent.
    CameraSpring { bounce: f64 },
}

impl Easing {
    /// Maps linear progress `u` in 0..=1 to eased progress over a 1000 ms
    /// segment. `Hold` never reaches this function from [`sample`]; it returns
    /// 0 for completeness. Use [`Easing::apply_over`] to supply the real span.
    pub fn apply(self, u: f64) -> f64 {
        self.apply_over(u, Ticks::from_ms(1000))
    }

    /// Maps linear progress `u` in 0..=1 to eased progress over a segment of
    /// `span`. Only [`Easing::Spring`] depends on the span.
    pub fn apply_over(self, u: f64, span: Ticks) -> f64 {
        match self {
            Easing::Linear => u,
            Easing::Hold => 0.0,
            Easing::EaseIn => u * u * u,
            Easing::EaseOut => 1.0 - (1.0 - u).powi(3),
            Easing::EaseInOut => {
                if u < 0.5 {
                    4.0 * u * u * u
                } else {
                    1.0 - (-2.0 * u + 2.0).powi(3) / 2.0
                }
            }
            Easing::CubicBezier { x1, y1, x2, y2 } => bezier_ease(x1, y1, x2, y2, u),
            Easing::Spring {
                stiffness,
                damping,
                mass,
            } => spring_step(stiffness, damping, mass, u * span.to_seconds_f64()),
            Easing::Cosine => 0.5 - 0.5 * (std::f64::consts::PI * u).cos(),
            Easing::CameraSpring { bounce } => camera_spring_step(bounce, u),
        }
    }
}

/// Step response of `m x'' + c x' + k x = k` from rest, at time `t` seconds.
///
/// The closed forms are exact, so sampling is deterministic and needs no
/// integrator. Critically damped and overdamped responses are clamped to
/// 0..=1, so floating-point error cannot overshoot them.
fn spring_step(stiffness: f64, damping: f64, mass: f64, t: f64) -> f64 {
    let omega = (stiffness / mass).sqrt();
    let zeta = damping / (2.0 * (stiffness * mass).sqrt());
    if (zeta - 1.0).abs() < 1e-12 {
        return (1.0 - (1.0 + omega * t) * (-omega * t).exp()).clamp(0.0, 1.0);
    }
    if zeta < 1.0 {
        let damped = omega * (1.0 - zeta * zeta).sqrt();
        let decay = (-zeta * omega * t).exp();
        return 1.0 - decay * ((damped * t).cos() + (zeta * omega / damped) * (damped * t).sin());
    }
    // Overdamped: two real roots, both negative.
    let root = omega * (zeta * zeta - 1.0).sqrt();
    let s1 = -zeta * omega + root;
    let s2 = -zeta * omega - root;
    let x = 1.0 + (s2 * (s1 * t).exp() - s1 * (s2 * t).exp()) / (s1 - s2);
    x.clamp(0.0, 1.0)
}

/// ScreenRight's camera step response over normalised progress `u`. Matches
/// `springProgress` in ScreenRight's `src/postprocess.mjs`, including the
/// endpoint pin: `u <= 0` gives 0 and `u >= 1` gives exactly 1.
fn camera_spring_step(bounce: f64, u: f64) -> f64 {
    if u <= 0.0 {
        return 0.0;
    }
    if u >= 1.0 {
        return 1.0;
    }
    let b = bounce.clamp(0.0, CAMERA_BOUNCE_MAX);
    if b == 0.0 {
        const CRITICAL_OMEGA: f64 = 9.23;
        return 1.0 - (1.0 + CRITICAL_OMEGA * u) * (-CRITICAL_OMEGA * u).exp();
    }
    const SETTLE: f64 = 6.9;
    let zeta = 1.0 - b;
    let omega = SETTLE / zeta;
    let damped = omega * (1.0 - zeta * zeta).sqrt();
    let decay = (-zeta * omega * u).exp();
    1.0 - decay * ((damped * u).cos() + (zeta * omega / damped) * (damped * u).sin())
}

/// Types that can be interpolated between two keyframes.
pub trait Lerp: Copy {
    fn lerp(a: Self, b: Self, u: f64) -> Self;
}

impl Lerp for f64 {
    fn lerp(a: Self, b: Self, u: f64) -> Self {
        a + (b - a) * u
    }
}

impl Lerp for Rect {
    fn lerp(a: Self, b: Self, u: f64) -> Self {
        Rect {
            x: f64::lerp(a.x, b.x, u),
            y: f64::lerp(a.y, b.y, u),
            width: f64::lerp(a.width, b.width, u),
            height: f64::lerp(a.height, b.height, u),
        }
    }
}

impl Lerp for Transform {
    fn lerp(a: Self, b: Self, u: f64) -> Self {
        Transform {
            dx: f64::lerp(a.dx, b.dx, u),
            dy: f64::lerp(a.dy, b.dy, u),
            scale: f64::lerp(a.scale, b.scale, u),
        }
    }
}

/// Samples a track at a layer-local time.
///
/// Before the first keyframe the first value holds; at or after the last
/// keyframe the last value holds. Between keyframes the earlier keyframe's
/// easing shapes the interpolation, and a `Hold` easing keeps its value
/// until the next keyframe. Returns `None` for an empty track.
pub fn sample<T: Lerp>(track: &[Keyframe<T>], local: Ticks) -> Option<T> {
    let first = track.first()?;
    if local <= first.at {
        return Some(first.value);
    }
    let last = track.last()?;
    if local >= last.at {
        return Some(last.value);
    }
    // `first.at < local < last.at`, so the partition point is in 1..len.
    let index = track.partition_point(|k| k.at <= local) - 1;
    let a = &track[index];
    let b = &track[index + 1];
    if a.easing == Easing::Hold {
        return Some(a.value);
    }
    let span = Ticks(b.at.0 - a.at.0);
    let u = (local.0 - a.at.0) as f64 / span.0 as f64;
    Some(T::lerp(a.value, b.value, a.easing.apply_over(u, span)))
}

fn bezier_coord(p1: f64, p2: f64, s: f64) -> f64 {
    let inv = 1.0 - s;
    3.0 * inv * inv * s * p1 + 3.0 * inv * s * s * p2 + s * s * s
}

/// Solves x(s) = u by bisection, then returns y(s). Bisection keeps the
/// result deterministic across hosts; x is monotonic because x1, x2 in 0..=1.
fn bezier_ease(x1: f64, y1: f64, x2: f64, y2: f64, u: f64) -> f64 {
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    for _ in 0..64 {
        let mid = (lo + hi) / 2.0;
        if bezier_coord(x1, x2, mid) < u {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    bezier_coord(y1, y2, (lo + hi) / 2.0)
}
