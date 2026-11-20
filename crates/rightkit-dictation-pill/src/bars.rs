//! The level bars of a recording capsule: per-slot targets from the microphone level (with a
//! breathing idle), per-frame smoothing that does not depend on the display refresh rate, and a
//! mic-independent sweep for the processing state.
//!
//! Every number that shapes the motion is a [`BarTunings`] field the host supplies; drawing the
//! bars (widths, heights, colours) is the host's painter.

use crate::anim::Mode;

/// Every constant of the bar motion.
#[derive(Clone, Copy, Debug)]
pub struct BarTunings {
    /// First slot of the recording group, and how many slots it has.
    pub recording_first: usize,
    pub recording_count: usize,
    /// Breathing period of slot `i` in seconds: `base + step * ((i * mul) % modulo)`.
    pub breath_period_base: f32,
    pub breath_period_step: f32,
    pub breath_period_mul: usize,
    pub breath_period_mod: usize,
    /// Phase offset per slot, in periods.
    pub breath_phase_step: f32,
    /// How much the outer slots of the recording group swell less than the middle ones.
    pub breath_edge_weight: f32,
    /// Breathing target: `floor + gain * swell * weight`.
    pub breath_floor: f32,
    pub breath_gain: f32,
    /// Live level shaping: `((level - floor) / range).max(0).powf(exponent)`.
    pub level_floor: f32,
    pub level_range: f32,
    pub level_exponent: f32,
    /// Slot envelope across the bar row: `base + gain * sin(position * PI).powf(exponent)`.
    pub envelope_base: f32,
    pub envelope_gain: f32,
    pub envelope_exponent: f32,
    /// Flutter on a live level: rate `base + step * (i % modulo)`, phase `phase_step * i`.
    pub flutter_rate_base: f32,
    pub flutter_rate_step: f32,
    pub flutter_rate_mod: usize,
    pub flutter_phase_step: f32,
    pub flutter_depth: f32,
    /// Lowest value a recording slot takes.
    pub slot_floor: f32,
    /// Smoothing factors per reference tick when rising and when falling.
    pub rise_k: f32,
    pub fall_k: f32,
    /// The tick length the factors are for, and the bounds on how many ticks one frame counts as.
    pub reference_tick_ms: f32,
    pub min_steps: f32,
    pub max_steps: f32,
    /// A frame gap longer than this counts as this long.
    pub max_dt_ms: f32,
    /// Processing sweep: `floor + gain * ((sin(now * rate - i * phase_step) + 1) / 2).powf(exponent)`.
    pub sweep_rate: f32,
    pub sweep_phase_step: f32,
    pub sweep_floor: f32,
    pub sweep_gain: f32,
    pub sweep_exponent: f32,
}

/// Breathing period of slot `index` in seconds.
pub fn breath_period(t: &BarTunings, index: usize) -> f32 {
    t.breath_period_base
        + t.breath_period_step * ((index * t.breath_period_mul) % t.breath_period_mod) as f32
}

/// Silence breathing target of slot `index`: each bar swells and relaxes on its own period and
/// phase, and the middle bars of the recording group swell the most.
pub fn breath_target(t: &BarTunings, index: usize, now_sec: f32) -> f32 {
    let period = breath_period(t, index);
    let phase = std::f32::consts::TAU * (now_sec / period + index as f32 * t.breath_phase_step);
    let swell = 0.5 - 0.5 * phase.cos();
    let weight = match index.checked_sub(t.recording_first) {
        Some(k) if k < t.recording_count => {
            let centre = (t.recording_count as f32 - 1.0) * 0.5;
            1.0 - t.breath_edge_weight * ((k as f32 - centre).abs() / centre)
        }
        _ => 1.0,
    };
    t.breath_floor + t.breath_gain * swell * weight
}

/// Recording branch: per-slot target before smoothing. A silent input breathes when `breathing`
/// (the "show animations" setting is on); any live level follows the audio.
pub fn recording_target<const N: usize>(
    t: &BarTunings,
    level: f32,
    index: usize,
    now_sec: f32,
    breathing: bool,
) -> f32 {
    let base = ((level.max(0.0) - t.level_floor) / t.level_range)
        .max(0.0)
        .powf(t.level_exponent);
    if base <= 0.0 && breathing {
        return breath_target(t, index, now_sec);
    }
    let position = index as f32 / (N - 1) as f32;
    let envelope = t.envelope_base
        + t.envelope_gain
            * (position * std::f32::consts::PI)
                .sin()
                .max(0.0)
                .powf(t.envelope_exponent);
    let flutter = if base > 0.0 {
        (now_sec
            * (t.flutter_rate_base + (index % t.flutter_rate_mod) as f32 * t.flutter_rate_step)
            + index as f32 * t.flutter_phase_step)
            .sin()
            * t.flutter_depth
    } else {
        0.0
    };
    (base * envelope + flutter * base).clamp(t.slot_floor, 1.0)
}

/// One display tick of the recording buffer. Smoothing is per reference tick; `dt_ms`
/// renormalises so the feel does not depend on the refresh rate.
pub fn integrate_recording<const N: usize>(
    t: &BarTunings,
    buf: &mut [f32; N],
    level: f32,
    now_sec: f32,
    dt_ms: f32,
    breathing: bool,
) {
    let steps = (dt_ms / t.reference_tick_ms).clamp(t.min_steps, t.max_steps);
    for (index, slot) in buf.iter_mut().enumerate() {
        let target = recording_target::<N>(t, level, index, now_sec, breathing);
        let k0 = if target > *slot { t.rise_k } else { t.fall_k };
        let k = 1.0 - (1.0 - k0).powf(steps);
        *slot += (target - *slot) * k;
    }
}

/// Processing branch: a mic-independent sine sweep.
pub fn processing_levels<const N: usize>(t: &BarTunings, now_sec: f32) -> [f32; N] {
    let mut out = [0.0f32; N];
    for (index, slot) in out.iter_mut().enumerate() {
        let phase = now_sec * t.sweep_rate - index as f32 * t.sweep_phase_step;
        *slot = t.sweep_floor + t.sweep_gain * ((phase.sin() + 1.0) / 2.0).powf(t.sweep_exponent);
    }
    out
}

/// Bar buffer driven once per painted frame.
#[derive(Clone, Debug)]
pub struct BarState<const N: usize> {
    pub buf: [f32; N],
    mode: Option<Mode>,
    last_ms: Option<f64>,
}

impl<const N: usize> BarState<N> {
    pub fn new() -> BarState<N> {
        BarState {
            buf: [0.0; N],
            mode: None,
            last_ms: None,
        }
    }

    /// `breathing`: a silent recording input breathes (false when animations are off).
    pub fn step(
        &mut self,
        tunings: &BarTunings,
        mode: Mode,
        level: f32,
        now_ms: f64,
        secs: f32,
        breathing: bool,
    ) {
        if self.mode != Some(mode) {
            self.mode = Some(mode);
            self.buf = [0.0; N];
            self.last_ms = None;
        }
        let dt_ms = self
            .last_ms
            .map(|l| (now_ms - l).max(0.0) as f32)
            .unwrap_or(tunings.reference_tick_ms)
            .min(tunings.max_dt_ms);
        match mode {
            Mode::Recording => {
                integrate_recording::<N>(tunings, &mut self.buf, level, secs, dt_ms, breathing)
            }
            Mode::Processing => self.buf = processing_levels::<N>(tunings, secs),
            _ => {
                if self.buf.iter().any(|v| *v != 0.0) {
                    self.buf = [0.0; N];
                }
            }
        }
        self.last_ms = Some(now_ms);
    }
}

impl<const N: usize> Default for BarState<N> {
    fn default() -> Self {
        BarState::new()
    }
}
