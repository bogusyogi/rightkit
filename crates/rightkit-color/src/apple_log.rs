//! Apple Log -> Rec.709 SDR as a generated 3D LUT.
//!
//! The curve is Apple's published "Apple Log Profile" transfer function (white paper, 2023):
//! with `R0=-0.05641088, Rt=0.01, c=47.28711236, beta=0.00964052, gamma=0.08550479,
//! delta=0.69336945` and `Pt = c (Rt-R0)^2`, scene-linear `R` encodes to
//! `P = c (R-R0)^2` for `R0 <= R < Rt` and `P = gamma log2(R+beta) + delta` above (0 below R0);
//! decoding inverts both branches. Scene light is in Rec.2020 primaries. No vendor LUT is
//! bundled: the table is generated from the math (deterministic, no licence encumbrance).
//!
//! Display rendering is CutRight's own: Rec.2020 -> Rec.709 primaries, an exposure gain, a
//! per-channel `tanh` shoulder that rolls the scene range (Apple Log reaches ~12x
//! scene white) into 0..1, then the Rec.709 OETF.

use crate::cube::Cube3d;

const R0: f64 = -0.056_410_88;
const RT: f64 = 0.01;
const C: f64 = 47.287_112_36;
const BETA: f64 = 0.009_640_52;
const GAMMA: f64 = 0.085_504_79;
const DELTA: f64 = 0.693_369_45;

/// Default LUT edge length (the largest size the LUT validator accepts, so shadows stay smooth).
pub const DEFAULT_SIZE: usize = 65;

/// Scene-linear -> Apple Log code value `P` (0..1).
pub fn encode(r: f64) -> f64 {
    if r >= RT {
        GAMMA * (r + BETA).log2() + DELTA
    } else if r >= R0 {
        C * (r - R0).powi(2)
    } else {
        0.0
    }
}

/// Apple Log code value `P` -> scene-linear (Rec.2020 primaries).
pub fn decode(p: f64) -> f64 {
    let pt = C * (RT - R0).powi(2);
    if p >= pt {
        2f64.powf((p - DELTA) / GAMMA) - BETA
    } else if p >= 0.0 {
        (p / C).sqrt() + R0
    } else {
        R0
    }
}

/// Linear light below the knee is left alone (18% grey stays 18%); above it a `tanh` shoulder
/// (C1-continuous, slope 1 at the knee, asymptote 1.0) rolls the scene range into 0..1.
const SHOULDER_KNEE: f64 = 0.6;

fn shoulder(x: f64, knee: f64) -> f64 {
    if x <= knee {
        x
    } else {
        knee + (1.0 - knee) * ((x - knee) / (1.0 - knee)).tanh()
    }
}

fn oetf_709(l: f64) -> f64 {
    let l = l.clamp(0.0, 1.0);
    if l < 0.018 {
        4.5 * l
    } else {
        1.099 * l.powf(0.45) - 0.099
    }
}

/// One RGB triplet of Apple Log code values -> Rec.709-encoded SDR (all 0..1).
pub fn to_rec709(p: [f64; 3], exposure_stops: f64) -> [f64; 3] {
    let lin = [decode(p[0]), decode(p[1]), decode(p[2])];
    // Rec.2020 -> Rec.709 linear primaries.
    let m = [
        [1.660_491, -0.587_641, -0.072_850],
        [-0.124_550, 1.132_900, -0.008_349],
        [-0.018_151, -0.100_579, 1.118_730],
    ];
    let gain = 2f64.powf(exposure_stops);
    let mut out = [0.0; 3];
    for (i, o) in out.iter_mut().enumerate() {
        let v = (m[i][0] * lin[0] + m[i][1] * lin[1] + m[i][2] * lin[2]).max(0.0) * gain;
        *o = oetf_709(shoulder(v, SHOULDER_KNEE));
    }
    out
}

/// Generate the Apple Log -> Rec.709 LUT (`size` points per axis, 2..=65).
pub fn rec709_lut(size: usize, exposure_stops: f64) -> Cube3d {
    let size = size.clamp(2, crate::cube::MAX_LUT_SIZE);
    let d = (size - 1) as f64;
    let mut data = Vec::with_capacity(size * size * size);
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                let o = to_rec709([r as f64 / d, g as f64 / d, b as f64 / d], exposure_stops);
                data.push([o[0] as f32, o[1] as f32, o[2] as f32]);
            }
        }
    }
    Cube3d {
        title: Some("CutRight Apple Log to Rec.709".into()),
        size,
        domain_min: [0.0; 3],
        domain_max: [1.0; 3],
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_is_continuous_and_invertible() {
        let pt = C * (RT - R0).powi(2);
        assert!(
            (GAMMA * (RT + BETA).log2() + DELTA - pt).abs() < 1e-4,
            "branches meet at Pt"
        );
        for r in [-0.05, 0.0, 0.005, 0.01, 0.18, 1.0, 5.0, 11.0] {
            let back = decode(encode(r));
            assert!((back - r).abs() < 1e-6 * (1.0 + r.abs()), "{r} -> {back}");
        }
        assert_eq!(encode(-1.0), 0.0);
        // 18% grey sits near code 0.49; code 1.0 is ~12x scene white.
        assert!((encode(0.18) - 0.488).abs() < 0.005);
        assert!((decode(1.0) - 12.0).abs() < 0.5);
    }

    #[test]
    fn lut_maps_grey_sanely_and_is_monotonic() {
        let g = to_rec709([encode(0.18); 3], 0.0);
        assert!((g[0] - oetf_709(0.18)).abs() < 1e-6);
        assert!(
            (g[0] - g[1]).abs() < 1e-6 && (g[1] - g[2]).abs() < 1e-6,
            "neutral stays neutral"
        );
        let mut prev = -1.0;
        for i in 0..=100 {
            let v = to_rec709([i as f64 / 100.0; 3], 0.0)[0];
            assert!(v >= prev);
            prev = v;
        }
        assert!(prev > 0.97 && prev <= 1.0);
        assert!(to_rec709([0.0; 3], 0.0)[0] < 0.01);
    }

    #[test]
    fn generated_lut_round_trips_through_the_validator() {
        let c = rec709_lut(17, 0.0);
        assert_eq!(Cube3d::parse(&c.to_text()).unwrap().size, 17);
        assert_eq!(rec709_lut(17, 0.0), c, "deterministic");
    }
}
