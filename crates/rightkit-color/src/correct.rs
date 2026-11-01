//! Exposure and white balance: bounded auto-correction from sampled frame statistics, with
//! manual overrides. Port of `ColorCorrection` (`video-media/src/color.rs`) with the exposure /
//! temperature / tint semantics made explicit and every output clamped.
//!
//! - Exposure is a linear-light gain in EV applied to the gamma-encoded signal as a channel gain
//!   `2^(ev/2.4)` (`colorchannelmixer`), reduced when it would clip the 95th percentile.
//! - White balance is FFmpeg's `colortemperature` filter (`temperature_k`; 6500 is neutral,
//!   lower warms the picture, higher cools it) plus a green-magenta `tint` (-1 magenta .. +1
//!   green, a green channel gain of `1 + 0.2 tint`).
//! - Auto white balance is a mid-tone grey-world estimate, deliberately partial and bounded:
//!   skin-dominated talking-head frames are not neutral, so it only removes a fraction of a cast.

use serde::{Deserialize, Serialize};

use crate::{clamp_or, sample::FrameStats};

pub const NEUTRAL_TEMPERATURE_K: f64 = 6500.0;
const ABS_MAX_EV: f64 = 2.0;
const ABS_TEMP: (f64, f64) = (3000.0, 12000.0);
const ASSUMED_GAMMA: f64 = 2.4;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorCorrection {
    pub exposure_ev: f64,
    pub temperature_k: f64,
    pub tint: f64,
}

impl Default for ColorCorrection {
    fn default() -> Self {
        ColorCorrection {
            exposure_ev: 0.0,
            temperature_k: NEUTRAL_TEMPERATURE_K,
            tint: 0.0,
        }
    }
}

impl ColorCorrection {
    /// Hard bounds, applied again when the fragment is built.
    pub fn clamped(&self) -> ColorCorrection {
        ColorCorrection {
            exposure_ev: clamp_or(self.exposure_ev, -ABS_MAX_EV, ABS_MAX_EV, 0.0),
            temperature_k: clamp_or(
                self.temperature_k,
                ABS_TEMP.0,
                ABS_TEMP.1,
                NEUTRAL_TEMPERATURE_K,
            ),
            tint: clamp_or(self.tint, -1.0, 1.0, 0.0),
        }
    }

    pub fn is_neutral(&self) -> bool {
        let c = self.clamped();
        c.exposure_ev.abs() < 0.005
            && (c.temperature_k - NEUTRAL_TEMPERATURE_K).abs() < 1.0
            && c.tint.abs() < 0.005
    }

    /// Filter chain fragment (white balance first, then exposure/tint), or `None` if neutral.
    pub fn filter(&self) -> Option<String> {
        if self.is_neutral() {
            return None;
        }
        let c = self.clamped();
        let mut parts = Vec::new();
        if (c.temperature_k - NEUTRAL_TEMPERATURE_K).abs() >= 1.0 {
            parts.push(format!(
                "colortemperature=temperature={:.0}:pl=1",
                c.temperature_k
            ));
        }
        let gain = 2f64.powf(c.exposure_ev / ASSUMED_GAMMA);
        let green = gain * (1.0 + 0.2 * c.tint);
        if (gain - 1.0).abs() > 1e-4 || (green - gain).abs() > 1e-4 {
            parts.push(format!(
                "colorchannelmixer=rr={gain:.4}:gg={green:.4}:bb={gain:.4}"
            ));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(","))
        }
    }
}

/// Bounds and strengths for [`auto_correction`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CorrectionLimits {
    /// Median (gamma-encoded) luma to aim for.
    pub target_median_luma: f64,
    pub max_exposure_ev: f64,
    /// Fraction of the measured exposure error to correct (0..=1).
    pub exposure_strength: f64,
    pub min_temperature_k: f64,
    pub max_temperature_k: f64,
    pub max_tint: f64,
    /// Fraction of the measured colour cast to correct (0..=1).
    pub wb_strength: f64,
}

impl Default for CorrectionLimits {
    fn default() -> Self {
        CorrectionLimits {
            target_median_luma: 0.45,
            max_exposure_ev: 1.0,
            exposure_strength: 0.8,
            min_temperature_k: 5000.0,
            max_temperature_k: 8500.0,
            max_tint: 0.5,
            wb_strength: 0.6,
        }
    }
}

/// Manual values win over the automatic estimate (and are still clamped).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CorrectionOverrides {
    pub exposure_ev: Option<f64>,
    pub temperature_k: Option<f64>,
    pub tint: Option<f64>,
}

/// `ln(B gain / R gain)` produced by `colortemperature=temperature=T:pl=1` on neutral grey,
/// measured with the qualified FFmpeg 8.1.3 build. Monotonic in T; inverted by interpolation.
const TEMP_CURVE: [(f64, f64); 12] = [
    (3000.0, -0.80),
    (3500.0, -0.594),
    (4000.0, -0.429),
    (5000.0, -0.214),
    (5500.0, -0.137),
    (6000.0, -0.074),
    (6500.0, -0.020),
    (7000.0, 0.050),
    (8000.0, 0.142),
    (9000.0, 0.196),
    (10000.0, 0.234),
    (12000.0, 0.288),
];

/// `d` is the wanted `ln(B/R)` gain relative to neutral; the filter itself is not exactly
/// neutral at 6500 K, so its own offset there is added before inverting the curve.
fn temperature_for_blue_red_log_gain(d: f64) -> f64 {
    let d = d + TEMP_CURVE[6].1;
    let first = TEMP_CURVE[0];
    let last = TEMP_CURVE[TEMP_CURVE.len() - 1];
    if d <= first.1 {
        return first.0;
    }
    if d >= last.1 {
        return last.0;
    }
    for w in TEMP_CURVE.windows(2) {
        let (t0, d0) = w[0];
        let (t1, d1) = w[1];
        if d >= d0 && d <= d1 {
            return t0 + (t1 - t0) * (d - d0) / (d1 - d0);
        }
    }
    NEUTRAL_TEMPERATURE_K
}

fn lin(v: f64) -> f64 {
    v.max(1e-4).powf(ASSUMED_GAMMA)
}

/// Estimate exposure / white balance for a clip from its sampled statistics.
pub fn auto_correction(
    stats: &FrameStats,
    limits: &CorrectionLimits,
    overrides: &CorrectionOverrides,
) -> ColorCorrection {
    let max_ev = clamp_or(limits.max_exposure_ev, 0.0, ABS_MAX_EV, 1.0);
    let mut c = ColorCorrection::default();

    if stats.pixels > 0 {
        let target = clamp_or(limits.target_median_luma, 0.2, 0.8, 0.45);
        let strength = clamp_or(limits.exposure_strength, 0.0, 1.0, 0.8);
        let median = stats.luma.p50.max(0.01);
        let mut ev = (lin(target) / lin(median)).log2() * strength;
        if ev > 0.0 {
            // Do not push the 95th percentile of any channel past white.
            let hi = stats.r.p95.max(stats.g.p95).max(stats.b.p95).max(0.01);
            let room_ev = (0.98 / hi).log2().max(0.0) * ASSUMED_GAMMA;
            ev = ev.min(room_ev);
        }
        c.exposure_ev = ev.clamp(-max_ev, max_ev);

        if stats.midtone_pixels > 0 {
            let [mr, mg, mb] = stats.midtone_mean;
            if mr > 0.01 && mg > 0.01 && mb > 0.01 {
                let wb = clamp_or(limits.wb_strength, 0.0, 1.0, 0.6);
                let t_lo = clamp_or(limits.min_temperature_k, ABS_TEMP.0, ABS_TEMP.1, 5000.0);
                let t_hi = clamp_or(limits.max_temperature_k, t_lo, ABS_TEMP.1, 8500.0);
                // Need B gain / R gain = mr / mb to level the cast.
                let d = (mr / mb).ln() * wb;
                c.temperature_k = temperature_for_blue_red_log_gain(d).clamp(t_lo, t_hi);
                let green_excess = (mg / (mr * mb).sqrt()).ln();
                let max_tint = clamp_or(limits.max_tint, 0.0, 1.0, 0.5);
                c.tint = (-green_excess / 1.2f64.ln() * wb).clamp(-max_tint, max_tint);
            }
        }
    }

    if let Some(v) = overrides.exposure_ev {
        c.exposure_ev = v;
    }
    if let Some(v) = overrides.temperature_k {
        c.temperature_k = v;
    }
    if let Some(v) = overrides.tint {
        c.tint = v;
    }
    c.clamped()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample::{stats_from_rgb24, ChannelStats};

    fn flat(r: u8, g: u8, b: u8) -> FrameStats {
        stats_from_rgb24(&[r, g, b].repeat(64))
    }

    #[test]
    fn neutral_has_no_filter() {
        assert_eq!(ColorCorrection::default().filter(), None);
    }

    #[test]
    fn dark_clip_is_brightened_within_bounds() {
        let c = auto_correction(
            &flat(60, 60, 60),
            &CorrectionLimits::default(),
            &CorrectionOverrides::default(),
        );
        assert!(c.exposure_ev > 0.5 && c.exposure_ev <= 1.0, "{c:?}");
        assert!((c.temperature_k - 6500.0).abs() < 50.0 && c.tint.abs() < 0.05);
        let f = c.filter().unwrap();
        assert!(
            f.contains("colorchannelmixer") && !f.contains("colortemperature"),
            "{f}"
        );
    }

    #[test]
    fn bright_clip_is_darkened_and_clipping_is_respected() {
        let c = auto_correction(
            &flat(220, 220, 220),
            &CorrectionLimits::default(),
            &CorrectionOverrides::default(),
        );
        assert!(c.exposure_ev < -0.3, "{c:?}");
        // A dark clip with bright highlights may not be pushed into clipping.
        let mut px = [40u8, 40, 40].repeat(80);
        px.extend([250u8, 250, 250].repeat(20));
        let c = auto_correction(
            &stats_from_rgb24(&px),
            &CorrectionLimits::default(),
            &CorrectionOverrides::default(),
        );
        assert!(c.exposure_ev < 0.2, "{c:?}");
    }

    #[test]
    fn warm_cast_is_cooled_and_cool_cast_warmed_and_both_are_bounded() {
        let l = CorrectionLimits::default();
        let o = CorrectionOverrides::default();
        let warm = auto_correction(&flat(150, 120, 90), &l, &o);
        assert!(
            warm.temperature_k > 6600.0 && warm.temperature_k <= l.max_temperature_k,
            "{warm:?}"
        );
        let cool = auto_correction(&flat(90, 120, 150), &l, &o);
        assert!(
            cool.temperature_k < 6400.0 && cool.temperature_k >= l.min_temperature_k,
            "{cool:?}"
        );
        let extreme = auto_correction(&flat(255, 128, 2), &l, &o);
        assert!(
            extreme.temperature_k <= l.max_temperature_k
                && extreme.exposure_ev.abs() <= l.max_exposure_ev
        );
        let green = auto_correction(&flat(100, 150, 100), &l, &o);
        assert!(green.tint < -0.05 && green.tint >= -l.max_tint, "{green:?}");
    }

    #[test]
    fn overrides_win_but_stay_clamped() {
        let o = CorrectionOverrides {
            exposure_ev: Some(99.0),
            temperature_k: Some(5200.0),
            tint: Some(f64::NAN),
        };
        let c = auto_correction(&flat(60, 60, 60), &CorrectionLimits::default(), &o);
        assert_eq!((c.exposure_ev, c.temperature_k, c.tint), (2.0, 5200.0, 0.0));
        let empty = FrameStats {
            luma: ChannelStats::default(),
            ..FrameStats::default()
        };
        assert!(auto_correction(
            &empty,
            &CorrectionLimits::default(),
            &CorrectionOverrides::default()
        )
        .is_neutral());
    }

    #[test]
    fn temperature_inverse_matches_the_curve() {
        for (t, d) in TEMP_CURVE {
            assert!((temperature_for_blue_red_log_gain(d - TEMP_CURVE[6].1) - t).abs() < 1.0);
        }
    }
}
