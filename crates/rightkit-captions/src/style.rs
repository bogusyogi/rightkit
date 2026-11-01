//! Caption style presets, shipped as JSON (`data/caption_styles.json`).
//!
//! Sizes are integers: font in permille of the SHORT frame side (so a 1080p
//! landscape and a 1080x1920 portrait frame share pixel sizes), pads, outline,
//! shadow and radius in percent of the font pixel size, positions in permille
//! of the frame. Every style renders with `drawtext`/`drawbox` only.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::segment::CaptionConfig;

const DATA: &str = include_str!("../data/caption_styles.json");

/// Frame orientation; a square frame uses the landscape values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Landscape,
    Portrait,
}

impl Orientation {
    pub fn of(w: i64, h: i64) -> Self {
        if h > w {
            Orientation::Portrait
        } else {
            Orientation::Landscape
        }
    }
}

/// A value per orientation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pair {
    pub landscape: i64,
    pub portrait: i64,
}

impl Pair {
    pub fn get(&self, o: Orientation) -> i64 {
        match o {
            Orientation::Landscape => self.landscape,
            Orientation::Portrait => self.portrait,
        }
    }
}

/// `#RRGGBB` plus an opacity percent (the JSON carries them separately).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub rgb: [u8; 3],
}

impl Color {
    pub fn parse(s: &str) -> Option<Self> {
        let h = s.strip_prefix('#')?;
        if h.len() != 6 || !h.is_ascii() {
            return None;
        }
        let v = u32::from_str_radix(h, 16).ok()?;
        Some(Self {
            rgb: [(v >> 16) as u8, (v >> 8) as u8, v as u8],
        })
    }

    /// FFmpeg colour option value (`0xRRGGBB` or `0xRRGGBB@0.NN`).
    pub fn ffmpeg(&self, alpha_pct: i64) -> String {
        let [r, g, b] = self.rgb;
        if alpha_pct >= 100 {
            format!("0x{r:02X}{g:02X}{b:02X}")
        } else {
            format!("0x{r:02X}{g:02X}{b:02X}@0.{:02}", alpha_pct.clamp(0, 99))
        }
    }
}

fn de_color<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let s = String::deserialize(d)?;
    Color::parse(&s)
        .map(|_| s.to_uppercase())
        .ok_or_else(|| serde::de::Error::custom(format!("bad colour {s:?}, want #RRGGBB")))
}

fn de_opt_color<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    match Option::<String>::deserialize(d)? {
        None => Ok(None),
        Some(s) => Color::parse(&s)
            .map(|_| Some(s.to_uppercase()))
            .ok_or_else(|| serde::de::Error::custom(format!("bad colour {s:?}, want #RRGGBB"))),
    }
}

fn full() -> i64 {
    100
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoxStyle {
    #[serde(deserialize_with = "de_color")]
    pub color: String,
    pub alpha_pct: i64,
    pub hpad_pct: i64,
    pub vpad_pct: i64,
    /// Corner radius; 0 = square. Rounded corners are stepped `drawbox` bands.
    pub radius_pct: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outline {
    #[serde(deserialize_with = "de_color")]
    pub color: String,
    pub pct: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shadow {
    #[serde(deserialize_with = "de_color")]
    pub color: String,
    #[serde(default = "full")]
    pub alpha_pct: i64,
    pub pct: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KaraokeMode {
    /// Only the word being spoken is highlighted.
    Active,
    /// Words stay highlighted from their start to the end of the cue.
    Progressive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Karaoke {
    #[serde(deserialize_with = "de_color")]
    pub highlight_color: String,
    pub mode: KaraokeMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Weight {
    Regular,
    /// Synthesised from the one shipped regular face with a thin same-colour
    /// stroke (the runtime ships a single font file).
    Bold,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptionStyle {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Font size, permille of the short frame side.
    pub font: Pair,
    pub weight: Weight,
    #[serde(default)]
    pub uppercase: bool,
    #[serde(deserialize_with = "de_color")]
    pub text_color: String,
    #[serde(default, deserialize_with = "de_opt_color")]
    pub emphasis_color: Option<String>,
    #[serde(default)]
    pub outline: Option<Outline>,
    #[serde(default)]
    pub shadow: Option<Shadow>,
    #[serde(default, rename = "box")]
    pub boxed: Option<BoxStyle>,
    #[serde(default)]
    pub karaoke: Option<Karaoke>,
    /// Box bottom edge, permille of frame height (clamped into the safe zone).
    pub anchor: Pair,
    /// Widest box, permille of frame width.
    pub max_width: Pair,
    #[serde(default)]
    pub max_chars_per_line: Option<usize>,
    #[serde(default)]
    pub max_lines: Option<usize>,
}

impl CaptionStyle {
    pub fn validate(&self) -> Result<(), String> {
        // Size fields are integers, so non-finite values fail deserialisation.
        for (name, pair) in [("font", self.font), ("max_width", self.max_width)] {
            if pair.landscape <= 0 || pair.portrait <= 0 {
                return Err(format!("caption style {name} must be positive"));
            }
        }
        if self.max_lines == Some(0) || self.max_chars_per_line == Some(0) {
            return Err("caption style line limits must be positive".into());
        }
        if self.outline.as_ref().is_some_and(|o| o.pct <= 0)
            || self.shadow.as_ref().is_some_and(|s| s.pct <= 0)
        {
            return Err("caption style outline and shadow sizes must be positive".into());
        }
        if let Some(boxed) = &self.boxed {
            if boxed.hpad_pct <= 0 || boxed.vpad_pct <= 0 || boxed.radius_pct < 0 {
                return Err(
                    "caption style box padding must be positive and radius non-negative".into(),
                );
            }
        }
        Ok(())
    }
}

/// The shipped style catalogue.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct StyleCatalog {
    pub schema: String,
    pub segmentation: Segmentation,
    pub styles: Vec<CaptionStyle>,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct Segmentation {
    pub landscape: CaptionConfig,
    pub portrait: CaptionConfig,
}

impl StyleCatalog {
    /// Built-ins plus project-owned styles, independent of the app library.
    pub fn with_project(dir: &Path) -> Result<Self, String> {
        let mut catalog = Self::builtin();
        let root = dir.join("caption-styles");
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(catalog),
            Err(e) => return Err(format!("reading {}: {e}", root.display())),
        };
        let mut paths = entries
            .map(|entry| entry.map(|e| e.path()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("reading {}: {e}", root.display()))?;
        paths.sort();
        for path in paths {
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let bytes =
                std::fs::read(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
            let style: CaptionStyle = serde_json::from_slice(&bytes)
                .map_err(|e| format!("invalid caption style {}: {e}", path.display()))?;
            style
                .validate()
                .map_err(|e| format!("invalid caption style {}: {e}", path.display()))?;
            if style.id.is_empty()
                || style.id.len() > 64
                || !style
                    .id
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
                || path.file_stem().and_then(|s| s.to_str()) != Some(style.id.as_str())
            {
                return Err(format!("invalid caption style id in {}", path.display()));
            }
            if catalog.get(&style.id).is_some() {
                return Err(format!("caption style {:?} reuses a built-in id", style.id));
            }
            catalog.styles.push(style);
        }
        Ok(catalog)
    }

    pub fn builtin() -> Self {
        let mut catalog: Self =
            serde_json::from_str(DATA).expect("data/caption_styles.json is valid");
        // Automatic landscape placement selects this real style when sampled
        // lower-band edges would be obscured by the ordinary caption anchor.
        let mut top = catalog
            .get("classic-box")
            .expect("classic box exists")
            .clone();
        top.id = "screen-safe-top".into();
        top.name = "Screen-safe top".into();
        top.description = "Boxed captions in the upper safe band for lower-screen UI.".into();
        top.anchor.landscape = 260;
        catalog.styles.push(top);
        catalog
    }

    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    pub fn get(&self, id: &str) -> Option<&CaptionStyle> {
        self.styles.iter().find(|s| s.id == id)
    }

    /// Segmentation limits for a frame, before the style's own caps.
    pub fn segmentation_for(&self, o: Orientation) -> CaptionConfig {
        match o {
            Orientation::Landscape => self.segmentation.landscape,
            Orientation::Portrait => self.segmentation.portrait,
        }
    }
}
