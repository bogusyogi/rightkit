//! Shot matching: make clip B look like reference clip A.
//!
//! The old CutRight algorithm (`ShotMatchTarget`: a target mean luma and saturation scale
//! applied with `eq`) is kept as [`ShotMatch::LumaSaturation`]. The new default is a per-channel
//! mean/std transfer on the gamma-encoded SDR signal: `out = in * gain + offset` with
//! `gain = std_A / std_B`, `offset = mean_A - gain * mean_B`, both clamped and scaled by a
//! `strength` in 0..=1, rendered as `lutrgb` so no frame buffer logic lives in Rust.

use serde::{Deserialize, Serialize};

use crate::{clamp_or, sample::FrameStats};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShotMatchLimits {
    pub min_gain: f64,
    pub max_gain: f64,
    pub max_offset: f64,
    pub strength: f64,
}

impl Default for ShotMatchLimits {
    fn default() -> Self {
        ShotMatchLimits {
            min_gain: 0.75,
            max_gain: 1.33,
            max_offset: 0.12,
            strength: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ShotMatch {
    /// Per-channel affine transform on 0..1 gamma-encoded RGB.
    ChannelTransfer { gain: [f64; 3], offset: [f64; 3] },
    /// Legacy target: mean luma (0..1) and a saturation multiplier (old `ShotMatchTarget`).
    LumaSaturation {
        mean_luma: f64,
        saturation_scale: f64,
    },
}

impl ShotMatch {
    fn bounded(&self) -> ShotMatch {
        match *self {
            ShotMatch::ChannelTransfer { gain, offset } => {
                let l = ShotMatchLimits::default();
                let mut g = [1.0; 3];
                let mut o = [0.0; 3];
                for i in 0..3 {
                    g[i] = clamp_or(gain[i], l.min_gain, l.max_gain, 1.0);
                    o[i] = clamp_or(offset[i], -l.max_offset, l.max_offset, 0.0);
                }
                ShotMatch::ChannelTransfer { gain: g, offset: o }
            }
            ShotMatch::LumaSaturation {
                mean_luma,
                saturation_scale,
            } => ShotMatch::LumaSaturation {
                mean_luma: clamp_or(mean_luma, 0.0, 1.0, 0.5),
                saturation_scale: clamp_or(saturation_scale, 0.0, 3.0, 1.0),
            },
        }
    }

    pub fn is_identity(&self) -> bool {
        match self.bounded() {
            ShotMatch::ChannelTransfer { gain, offset } => {
                gain.iter().all(|g| (g - 1.0).abs() < 1e-3) && offset.iter().all(|o| o.abs() < 1e-3)
            }
            ShotMatch::LumaSaturation {
                mean_luma,
                saturation_scale,
            } => (mean_luma - 0.5).abs() < 1e-4 && (saturation_scale - 1.0).abs() < 1e-4,
        }
    }

    /// Filter fragment, or `None` when the transform is the identity.
    pub fn filter(&self) -> Option<String> {
        if self.is_identity() {
            return None;
        }
        Some(match self.bounded() {
            ShotMatch::ChannelTransfer { gain, offset } => {
                let ch = |i: usize| {
                    format!(
                        "clip(val*{:.5}+({:.5})*maxval,0,maxval)",
                        gain[i], offset[i]
                    )
                };
                format!("lutrgb=r='{}':g='{}':b='{}'", ch(0), ch(1), ch(2))
            }
            // Brightness offset on luma and a saturation scale on chroma about mid-grey,
            // with `lutyuv`: the same arithmetic as the GPL-only `eq` filter this replaced.
            ShotMatch::LumaSaturation {
                mean_luma,
                saturation_scale,
            } => {
                let chroma =
                    format!("clip((val-maxval/2)*{saturation_scale:.4}+maxval/2,0,maxval)");
                format!(
                    "lutyuv=y='clip(val+({:.4})*maxval,0,maxval)':u='{chroma}':v='{chroma}'",
                    (mean_luma - 0.5).clamp(-0.5, 0.5),
                )
            }
        })
    }
}

/// Transform that makes `clip` statistically resemble `reference` (both measured on the same
/// SDR space, see [`crate::sample_stats`]).
pub fn match_clip_to_reference(
    reference: &FrameStats,
    clip: &FrameStats,
    limits: &ShotMatchLimits,
) -> ShotMatch {
    let strength = clamp_or(limits.strength, 0.0, 1.0, 1.0);
    let lo = clamp_or(limits.min_gain, 0.25, 1.0, 0.75);
    let hi = clamp_or(limits.max_gain, 1.0, 4.0, 1.33);
    let max_off = clamp_or(limits.max_offset, 0.0, 0.5, 0.12);
    let a = [reference.r, reference.g, reference.b];
    let b = [clip.r, clip.g, clip.b];
    let mut gain = [1.0; 3];
    let mut offset = [0.0; 3];
    if reference.pixels > 0 && clip.pixels > 0 {
        for i in 0..3 {
            let g = if b[i].std > 1e-3 {
                (a[i].std / b[i].std).clamp(lo, hi)
            } else {
                1.0
            };
            let o = (a[i].mean - g * b[i].mean).clamp(-max_off, max_off);
            gain[i] = 1.0 + (g - 1.0) * strength;
            offset[i] = o * strength;
        }
    }
    ShotMatch::ChannelTransfer { gain, offset }
}
