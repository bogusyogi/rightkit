//! HDR (PQ / HLG) and Apple Log -> SDR Rec.709 filter fragments.
//!
//! HDR path (FFmpeg's documented chain): `zscale` to linear light, primaries to bt709, `tonemap`
//! (hable / mobius / reinhard) on linear RGB, `zscale` to the bt709 transfer + matrix. The Apple Log
//! path decodes through the generated LUT ([`crate::apple_log`]); its fragment needs the cube
//! path and is assembled in [`crate::plan`].

use serde::{Deserialize, Serialize};

use crate::{clamp_or, space::ColorSpaceKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Tonemap {
    #[default]
    Hable,
    Mobius,
    Reinhard,
}

impl Tonemap {
    fn name(self) -> &'static str {
        match self {
            Tonemap::Hable => "hable",
            Tonemap::Mobius => "mobius",
            Tonemap::Reinhard => "reinhard",
        }
    }
}

/// Typed HDR -> SDR parameters. Every value is clamped when the fragment is built.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SdrConversion {
    pub tonemap: Tonemap,
    /// Source mastering peak in nits (100..=10000). 1000 suits most PQ/HLG phone footage.
    pub peak_nits: f64,
    /// Highlight desaturation (`tonemap=desat`), 0..=4. 0 keeps colour.
    pub desat: f64,
    /// Mobius transition / reinhard parameter, 0.01..=0.99.
    pub param: f64,
    /// Exposure of the Apple Log decode, in stops (-2..=2).
    pub apple_log_exposure_stops: f64,
}

impl Default for SdrConversion {
    fn default() -> Self {
        SdrConversion {
            tonemap: Tonemap::Hable,
            peak_nits: 1000.0,
            desat: 0.0,
            param: 0.3,
            apple_log_exposure_stops: 0.0,
        }
    }
}

impl SdrConversion {
    pub(crate) fn clamped(&self) -> SdrConversion {
        let d = SdrConversion::default();
        SdrConversion {
            tonemap: self.tonemap,
            peak_nits: clamp_or(self.peak_nits, 100.0, 10_000.0, d.peak_nits),
            desat: clamp_or(self.desat, 0.0, 4.0, d.desat),
            param: clamp_or(self.param, 0.01, 0.99, d.param),
            apple_log_exposure_stops: clamp_or(self.apple_log_exposure_stops, -2.0, 2.0, 0.0),
        }
    }

    /// The HDR -> SDR chain for PQ / HLG sources, ending in 8-bit-or-`pix_fmt` bt709 YUV.
    /// `None` for every other kind.
    pub fn hdr_fragment(&self, kind: ColorSpaceKind, pix_fmt: &str) -> Option<String> {
        let tin = match kind {
            ColorSpaceKind::Rec2020Pq => "smpte2084",
            ColorSpaceKind::Rec2020Hlg => "arib-std-b67",
            _ => return None,
        };
        let c = self.clamped();
        let peak_rel = c.peak_nits / 100.0; // relative to the 100-nit reference white of npl=100
        let tm = match c.tonemap {
            Tonemap::Hable => format!(
                "tonemap=tonemap=hable:peak={peak_rel:.3}:desat={:.3}",
                c.desat
            ),
            _ => format!(
                "tonemap=tonemap={}:param={:.3}:peak={peak_rel:.3}:desat={:.3}",
                c.tonemap.name(),
                c.param,
                c.desat
            ),
        };
        Some(format!(
            "zscale=tin={tin}:pin=bt2020:min=bt2020nc:t=linear:npl=100,format=gbrpf32le,\
             zscale=p=bt709,{tm},\
             zscale=t=bt709:m=bt709:r=tv:dither=error_diffusion,format={pix_fmt}"
        ))
    }
}
