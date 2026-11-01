//! The user-facing grade: curves, lift/gamma/gain wheels, six HSL secondaries and a global
//! saturation, rendered as one self-contained filter stage with the LGPL filters only.
//!
//! The stage runs on 16-bit planar RGB so the maths is the same at 8 and 10 bit:
//! `scale` (Rec.709 tv -> full-range RGB) -> wheels (`lutrgb`) -> `curves` -> saturation
//! (`colorchannelmixer`, Rec.709 luma weights, so luma stays put) -> one `huesaturation` per
//! secondary -> `scale` (RGB -> Rec.709 tv). The two explicit `scale` filters pin the matrix and
//! range both ways; the implicit conversions FFmpeg inserts around an RGB-only filter assume the
//! frame's own tags and shift colour when they differ. The caller appends `format=<pix_fmt>`.
//!
//! Wheels, per channel on `v = val / maxval`: `out = clip((gain * (v + lift * (1 - v)))^(1 / gamma))`.
//! Neutral values (lift 0, gamma 1, gain 1) and identity curves emit nothing.

use serde::{Deserialize, Serialize};

use crate::{clamp_or, ColorError};

/// Most control points on one curve.
pub const MAX_CURVE_POINTS: usize = 16;
/// Most HSL secondaries in one grade.
pub const MAX_HSL: usize = 6;

const LIFT: (f64, f64) = (-0.5, 0.5);
const GAMMA: (f64, f64) = (0.2, 5.0);
const GAIN: (f64, f64) = (0.0, 4.0);
/// Rec.709 luma weights (`huesaturation` desaturates only partially and with thirds, so global
/// saturation uses `colorchannelmixer` instead).
const LUMA: [f64; 3] = [0.2126, 0.7152, 0.0722];

/// One control point of a curve; both coordinates in 0..=1.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// Tone curves. An empty channel is untouched; `master` applies to all three.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Curves {
    pub master: Vec<Point>,
    pub r: Vec<Point>,
    pub g: Vec<Point>,
    pub b: Vec<Point>,
}

/// Lift / gamma / gain per channel, `[r, g, b]`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Wheels {
    /// -0.5..=0.5, 0 neutral.
    pub lift: [f64; 3],
    /// 0.2..=5.0, 1 neutral.
    pub gamma: [f64; 3],
    /// 0.0..=4.0, 1 neutral.
    pub gain: [f64; 3],
}

impl Default for Wheels {
    fn default() -> Self {
        Wheels {
            lift: [0.0; 3],
            gamma: [1.0; 3],
            gain: [1.0; 3],
        }
    }
}

/// The colour band an HSL secondary selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HslRange {
    Reds,
    Yellows,
    Greens,
    Cyans,
    Blues,
    Magentas,
}

impl HslRange {
    /// The `huesaturation` `colors` flag.
    fn flag(self) -> &'static str {
        match self {
            HslRange::Reds => "r",
            HslRange::Yellows => "y",
            HslRange::Greens => "g",
            HslRange::Cyans => "c",
            HslRange::Blues => "b",
            HslRange::Magentas => "m",
        }
    }
}

fn one() -> f64 {
    1.0
}

/// A secondary correction on one colour band.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HslSecondary {
    pub range: HslRange,
    /// Degrees, -180..=180.
    #[serde(default)]
    pub hue_shift: f64,
    /// -1..=1, 0 neutral.
    #[serde(default)]
    pub saturation: f64,
    /// -1..=1, 0 neutral (`huesaturation` `intensity`).
    #[serde(default)]
    pub lightness: f64,
    /// 0..=1, 1 default (`huesaturation` `strength`: how hard the band is hit).
    #[serde(default = "one")]
    pub strength: f64,
}

/// A whole grade. Every part is optional; a default grade is neutral.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Grade {
    pub curves: Option<Curves>,
    pub wheels: Option<Wheels>,
    /// At most [`MAX_HSL`] entries, applied in order.
    pub hsl: Vec<HslSecondary>,
    /// 0..=2, 1 neutral; 0 is greyscale.
    pub saturation: Option<f64>,
}

fn invalid(msg: impl Into<String>) -> ColorError {
    ColorError::Invalid(msg.into())
}

fn clamp_triple(v: [f64; 3], (lo, hi): (f64, f64), default: f64) -> [f64; 3] {
    v.map(|x| clamp_or(x, lo, hi, default))
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-4
}

fn curve_is_identity(points: &[Point]) -> bool {
    points.iter().all(|p| near(p.x, p.y))
}

fn clamp_curve(points: &[Point]) -> Vec<Point> {
    let mut out: Vec<Point> = points
        .iter()
        .map(|p| Point {
            x: clamp_or(p.x, 0.0, 1.0, 0.0),
            y: clamp_or(p.y, 0.0, 1.0, 0.0),
        })
        .collect();
    out.sort_by(|a, b| a.x.total_cmp(&b.x));
    out
}

fn validate_curve(name: &str, points: &[Point]) -> Result<(), ColorError> {
    if points.len() > MAX_CURVE_POINTS {
        return Err(invalid(format!(
            "curve {name} has {} points; at most {MAX_CURVE_POINTS}",
            points.len()
        )));
    }
    for p in points {
        if !p.x.is_finite() || !p.y.is_finite() {
            return Err(invalid(format!("curve {name} has a non-finite point")));
        }
        if !(0.0..=1.0).contains(&p.x) || !(0.0..=1.0).contains(&p.y) {
            return Err(invalid(format!(
                "curve {name} point ({}, {}) is outside 0..=1",
                p.x, p.y
            )));
        }
    }
    if points.windows(2).any(|w| w[1].x <= w[0].x) {
        return Err(invalid(format!("curve {name} needs strictly increasing x")));
    }
    Ok(())
}

fn curve_text(points: &[Point]) -> String {
    points
        .iter()
        .map(|p| format!("{:.4}/{:.4}", p.x, p.y))
        .collect::<Vec<_>>()
        .join(" ")
}

impl Curves {
    fn clamped(&self) -> Curves {
        Curves {
            master: clamp_curve(&self.master),
            r: clamp_curve(&self.r),
            g: clamp_curve(&self.g),
            b: clamp_curve(&self.b),
        }
    }

    fn is_neutral(&self) -> bool {
        [&self.master, &self.r, &self.g, &self.b]
            .iter()
            .all(|c| curve_is_identity(c))
    }
}

impl Wheels {
    fn clamped(&self) -> Wheels {
        Wheels {
            lift: clamp_triple(self.lift, LIFT, 0.0),
            gamma: clamp_triple(self.gamma, GAMMA, 1.0),
            gain: clamp_triple(self.gain, GAIN, 1.0),
        }
    }

    fn channel_is_neutral(&self, i: usize) -> bool {
        near(self.lift[i], 0.0) && near(self.gamma[i], 1.0) && near(self.gain[i], 1.0)
    }

    fn is_neutral(&self) -> bool {
        (0..3).all(|i| self.channel_is_neutral(i))
    }

    /// `lutrgb` expression for channel `i` (of the clamped wheels).
    fn expr(&self, i: usize) -> String {
        if self.channel_is_neutral(i) {
            return "val".into();
        }
        let lifted = format!(
            "clip({:.5}*(val/maxval+({:.5})*(1-val/maxval)),0,1)",
            self.gain[i], self.lift[i]
        );
        if near(self.gamma[i], 1.0) {
            format!("{lifted}*maxval")
        } else {
            format!("pow({lifted},{:.6})*maxval", 1.0 / self.gamma[i])
        }
    }
}

impl HslSecondary {
    fn clamped(&self) -> HslSecondary {
        HslSecondary {
            range: self.range,
            hue_shift: clamp_or(self.hue_shift, -180.0, 180.0, 0.0),
            saturation: clamp_or(self.saturation, -1.0, 1.0, 0.0),
            lightness: clamp_or(self.lightness, -1.0, 1.0, 0.0),
            strength: clamp_or(self.strength, 0.0, 1.0, 1.0),
        }
    }

    fn is_neutral(&self) -> bool {
        self.strength < 1e-4
            || (self.hue_shift.abs() < 0.01
                && self.saturation.abs() < 5e-4
                && self.lightness.abs() < 5e-4)
    }
}

impl Grade {
    /// Every value pulled into its documented range; non-finite values become neutral.
    pub fn clamped(&self) -> Grade {
        Grade {
            curves: self.curves.as_ref().map(Curves::clamped),
            wheels: self.wheels.as_ref().map(Wheels::clamped),
            hsl: self.hsl.iter().map(HslSecondary::clamped).collect(),
            saturation: self.saturation.map(|s| clamp_or(s, 0.0, 2.0, 1.0)),
        }
    }

    /// `true` when [`Grade::filter`] would emit nothing.
    pub fn is_neutral(&self) -> bool {
        let g = self.clamped();
        g.curves.as_ref().is_none_or(Curves::is_neutral)
            && g.wheels.as_ref().is_none_or(Wheels::is_neutral)
            && g.hsl.iter().all(HslSecondary::is_neutral)
            && g.saturation.is_none_or(|s| near(s, 1.0))
    }

    /// Refuse what clamping cannot repair: non-finite numbers, more than [`MAX_CURVE_POINTS`]
    /// points on a curve or [`MAX_HSL`] secondaries, curve points outside 0..=1 and curves whose
    /// x is not strictly increasing as given. Out-of-range wheel, HSL and saturation values are
    /// not an error; [`Grade::clamped`] pulls them in.
    pub fn validate(&self) -> Result<(), ColorError> {
        if let Some(c) = &self.curves {
            for (name, pts) in [("master", &c.master), ("r", &c.r), ("g", &c.g), ("b", &c.b)] {
                validate_curve(name, pts)?;
            }
        }
        if let Some(w) = &self.wheels {
            for (name, v) in [("lift", w.lift), ("gamma", w.gamma), ("gain", w.gain)] {
                if v.iter().any(|x| !x.is_finite()) {
                    return Err(invalid(format!("wheels.{name} has a non-finite value")));
                }
            }
        }
        if self.hsl.len() > MAX_HSL {
            return Err(invalid(format!(
                "hsl has {} entries; at most {MAX_HSL}",
                self.hsl.len()
            )));
        }
        for (i, h) in self.hsl.iter().enumerate() {
            if ![h.hue_shift, h.saturation, h.lightness, h.strength]
                .iter()
                .all(|x| x.is_finite())
            {
                return Err(invalid(format!("hsl[{i}] has a non-finite value")));
            }
        }
        if self.saturation.is_some_and(|s| !s.is_finite()) {
            return Err(invalid("saturation is not finite"));
        }
        Ok(())
    }

    /// FFmpeg filters the stage needs, in no particular order; empty when neutral.
    pub fn requires(&self) -> Vec<&'static str> {
        if self.is_neutral() {
            return Vec::new();
        }
        let g = self.clamped();
        let mut out = vec!["scale", "format"];
        if g.wheels.as_ref().is_some_and(|w| !w.is_neutral()) {
            out.push("lutrgb");
        }
        if g.curves.as_ref().is_some_and(|c| !c.is_neutral()) {
            out.push("curves");
        }
        if g.saturation.is_some_and(|s| !near(s, 1.0)) {
            out.push("colorchannelmixer");
        }
        if g.hsl.iter().any(|h| !h.is_neutral()) {
            out.push("huesaturation");
        }
        out
    }

    /// The comma-joined stage (see the module docs), or `None` when neutral.
    pub fn filter(&self) -> Option<String> {
        if self.is_neutral() {
            return None;
        }
        let g = self.clamped();
        let mut stages: Vec<String> = Vec::new();
        if let Some(w) = g.wheels.filter(|w| !w.is_neutral()) {
            stages.push(format!(
                "lutrgb=r='{}':g='{}':b='{}'",
                w.expr(0),
                w.expr(1),
                w.expr(2)
            ));
        }
        if let Some(c) = g.curves.filter(|c| !c.is_neutral()) {
            let opts: Vec<String> = [("master", &c.master), ("r", &c.r), ("g", &c.g), ("b", &c.b)]
                .iter()
                .filter(|(_, pts)| !pts.is_empty() && !curve_is_identity(pts))
                .map(|(name, pts)| format!("{name}='{}'", curve_text(pts)))
                .collect();
            stages.push(format!("curves={}", opts.join(":")));
        }
        if let Some(s) = g.saturation.filter(|s| !near(*s, 1.0)) {
            // Saturation about Rec.709 luma: out = luma + s * (in - luma), per channel.
            const NAMES: [&str; 3] = ["r", "g", "b"];
            let mut coef: Vec<String> = Vec::with_capacity(9);
            for (i, out) in NAMES.iter().enumerate() {
                for (j, inp) in NAMES.iter().enumerate() {
                    let diag = if i == j { s } else { 0.0 };
                    coef.push(format!("{out}{inp}={:.5}", LUMA[j] * (1.0 - s) + diag));
                }
            }
            stages.push(format!("colorchannelmixer={}", coef.join(":")));
        }
        for h in g.hsl.iter().filter(|h| !h.is_neutral()) {
            stages.push(format!(
                "huesaturation=colors={}:hue={:.3}:saturation={:.4}:intensity={:.4}:strength={:.4}",
                h.range.flag(),
                h.hue_shift,
                h.saturation,
                h.lightness,
                h.strength
            ));
        }
        Some(format!(
            "scale=in_color_matrix=bt709:in_range=tv:out_range=pc,format=gbrp16le,{},\
             scale=out_color_matrix=bt709:out_range=tv",
            stages.join(",")
        ))
    }
}

// Unit tests here only cover what no e2e can reach: the exact text of a fixed grade (the e2e
// measures delivered pixels, not strings), and the refusals (serve cannot carry a NaN, and
// `colour.set` refusals are exercised end to end only for the cases JSON can express).
#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: f64, y: f64) -> Point {
        Point { x, y }
    }

    #[test]
    fn neutral_grades_emit_nothing() {
        assert_eq!(Grade::default().filter(), None);
        let g = Grade {
            wheels: Some(Wheels::default()),
            curves: Some(Curves {
                master: vec![pt(0.0, 0.0), pt(1.0, 1.0)],
                ..Curves::default()
            }),
            saturation: Some(1.0),
            hsl: vec![HslSecondary {
                range: HslRange::Reds,
                hue_shift: 0.0,
                saturation: 0.0,
                lightness: 0.0,
                strength: 1.0,
            }],
        };
        assert!(g.is_neutral());
        assert_eq!(g.filter(), None);
        assert!(g.requires().is_empty());
    }

    #[test]
    fn fixed_grade_has_exact_filter_text() {
        let g = Grade {
            wheels: Some(Wheels {
                gain: [1.3, 1.0, 0.7],
                gamma: [1.0, 2.0, 1.0],
                ..Wheels::default()
            }),
            curves: Some(Curves {
                master: vec![pt(0.0, 0.0), pt(0.5, 0.6), pt(1.0, 1.0)],
                ..Curves::default()
            }),
            saturation: Some(0.0),
            hsl: vec![HslSecondary {
                range: HslRange::Blues,
                hue_shift: -20.0,
                saturation: 0.25,
                lightness: 0.1,
                strength: 1.0,
            }],
        };
        assert_eq!(
            g.filter().unwrap(),
            "scale=in_color_matrix=bt709:in_range=tv:out_range=pc,format=gbrp16le,\
             lutrgb=r='clip(1.30000*(val/maxval+(0.00000)*(1-val/maxval)),0,1)*maxval':\
             g='pow(clip(1.00000*(val/maxval+(0.00000)*(1-val/maxval)),0,1),0.500000)*maxval':\
             b='clip(0.70000*(val/maxval+(0.00000)*(1-val/maxval)),0,1)*maxval',\
             curves=master='0.0000/0.0000 0.5000/0.6000 1.0000/1.0000',\
             colorchannelmixer=rr=0.21260:rg=0.71520:rb=0.07220:gr=0.21260:gg=0.71520:gb=0.07220:\
             br=0.21260:bg=0.71520:bb=0.07220,\
             huesaturation=colors=b:hue=-20.000:saturation=0.2500:intensity=0.1000:strength=1.0000,\
             scale=out_color_matrix=bt709:out_range=tv"
        );
        assert_eq!(
            g.requires(),
            vec![
                "scale",
                "format",
                "lutrgb",
                "curves",
                "colorchannelmixer",
                "huesaturation"
            ]
        );
    }

    #[test]
    fn out_of_range_values_clamp_and_refusals_hold() {
        let g = Grade {
            wheels: Some(Wheels {
                lift: [9.0, 0.0, 0.0],
                gamma: [0.0, 1.0, 1.0],
                gain: [f64::NAN, 1.0, 9.0],
            }),
            saturation: Some(7.0),
            ..Grade::default()
        };
        // NaN is a refusal, not a clamp.
        assert!(matches!(g.validate(), Err(ColorError::Invalid(_))));
        let c = Grade {
            wheels: Some(Wheels {
                lift: [9.0, 0.0, 0.0],
                gamma: [0.0, 1.0, 1.0],
                gain: [1.0, 1.0, 9.0],
            }),
            saturation: Some(7.0),
            ..Grade::default()
        };
        assert!(c.validate().is_ok());
        let w = c.clamped().wheels.unwrap();
        assert_eq!((w.lift[0], w.gamma[0], w.gain[2]), (0.5, 0.2, 4.0));
        assert_eq!(c.clamped().saturation, Some(2.0));

        let curve = |points: Vec<Point>| Grade {
            curves: Some(Curves {
                master: points,
                ..Curves::default()
            }),
            ..Grade::default()
        };
        let many: Vec<Point> = (0..=MAX_CURVE_POINTS)
            .map(|i| pt(i as f64 / 20.0, 0.5))
            .collect();
        assert!(curve(many).validate().is_err());
        assert!(curve(vec![pt(0.6, 0.6), pt(0.4, 0.4)]).validate().is_err());
        assert!(curve(vec![pt(0.5, 0.2), pt(0.5, 0.9)]).validate().is_err());
        assert!(curve(vec![pt(f64::NAN, 0.2)]).validate().is_err());
        assert!(curve(vec![pt(0.2, 1.5)]).validate().is_err());
        let hsl = Grade {
            hsl: vec![
                HslSecondary {
                    range: HslRange::Reds,
                    hue_shift: 0.0,
                    saturation: 0.1,
                    lightness: 0.0,
                    strength: 1.0,
                };
                MAX_HSL + 1
            ],
            ..Grade::default()
        };
        assert!(hsl.validate().is_err());
    }
}
