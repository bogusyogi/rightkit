//! Typst rendering for titles, captions and math, with optional LaTeX input.
//!
//! - [`FontFace::parse`] reads sfnt metrics; [`FontFace::measure`] returns advances & missing glyphs.
//! - [`Typesetter::new`]`(FontSource, ppi)` then [`Typesetter::render`]`(&Request)`.
//! - A [`Request`] source is Typst markup, Typst math, or (feature `latex`) LaTeX via mitex.
//! - Output is SVG by default, or PNG with feature `png` (typst-render).
//! - `latex_to_typst` (feature `latex`) converts LaTeX math to Typst math on its own.
//!
//! The Typst world has no file access and no package fetching: `#image`, `#read`, `#import` of
//! packages and the like fail with a compile error. Fonts are explicit ([`FontSource`]); none
//! are bundled unless feature `embedded-fonts` is on. Callers pass caption strings; there is no
//! transcript dependency.
//!
//! NOTE: written against the typst 0.15.1 `World` API from the decision record
//! (`docs/design/motion-owner.md`) without a compile; CI is the first compiler.

pub mod font;
mod world;

pub use font::{FontError, FontFace, Measurement};

use thiserror::Error;

/// Where fonts come from. Empty font sets are refused at render time.
#[derive(Debug, Clone, Default)]
pub enum FontSource {
    /// No fonts (render fails with [`TypesetError::NoFonts`]).
    #[default]
    None,
    /// Raw font files (TTF, OTF, TTC); each file may hold several faces.
    Bytes(Vec<Vec<u8>>),
    /// Typst's embedded fonts. Needs feature `embedded-fonts`.
    #[cfg(feature = "embedded-fonts")]
    Embedded,
}

/// What the request source is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Typst markup (titles, captions).
    Typst(String),
    /// Typst math body, rendered as a display equation (`$ ... $`).
    Math(String),
    /// LaTeX math body, converted by mitex. Needs feature `latex`.
    #[cfg(feature = "latex")]
    Latex(String),
}

/// Output encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Output {
    #[default]
    Svg,
    /// Needs feature `png`.
    #[cfg(feature = "png")]
    Png,
}

/// A render request. The page is auto-sized and transparent.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub source: Source,
    pub output: Output,
    /// Base text size in points.
    pub font_size_pt: f64,
    /// Preferred font family, if any.
    pub font_family: Option<String>,
    /// Page margin in points.
    pub margin_pt: f64,
}

impl Request {
    pub fn new(source: Source) -> Self {
        Self {
            source,
            output: Output::default(),
            font_size_pt: 24.0,
            font_family: None,
            margin_pt: 4.0,
        }
    }

    /// The full Typst document this request compiles.
    pub fn to_typst(&self) -> Result<String, TypesetError> {
        let mut doc = format!(
            "#set page(width: auto, height: auto, margin: {}pt, fill: none)\n#set text(size: {}pt",
            self.margin_pt, self.font_size_pt
        );
        if let Some(family) = &self.font_family {
            let escaped = family.replace('\\', "\\\\").replace('"', "\\\"");
            doc.push_str(&format!(", font: \"{escaped}\""));
        }
        doc.push_str(")\n");
        match &self.source {
            Source::Typst(s) => doc.push_str(s),
            Source::Math(s) => doc.push_str(&format!("$ {s} $")),
            #[cfg(feature = "latex")]
            Source::Latex(s) => doc.push_str(&format!("$ {} $", latex_to_typst(s)?)),
        }
        Ok(doc)
    }
}

/// A rendered result.
#[derive(Debug, Clone, PartialEq)]
pub enum Rendered {
    Svg(String),
    #[cfg(feature = "png")]
    Png {
        bytes: Vec<u8>,
        width: u32,
        height: u32,
    },
}

/// Typeset errors.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TypesetError {
    #[error("no fonts were provided")]
    NoFonts,
    #[error("no font could be read from the supplied bytes")]
    UnreadableFonts,
    #[error("latex conversion failed: {0}")]
    Latex(String),
    #[error("typst compile failed: {0}")]
    Compile(String),
    #[error("document produced no pages")]
    NoPages,
    #[error("png encode failed: {0}")]
    Encode(String),
}

/// Convert LaTeX math to Typst math (mitex). The result is a math body, not a document.
/// Commands that rely on the mitex Typst package are not resolvable here (no package fetching)
/// and surface as a compile error at render time.
#[cfg(feature = "latex")]
pub fn latex_to_typst(latex: &str) -> Result<String, TypesetError> {
    mitex::convert_math(latex, None).map_err(TypesetError::Latex)
}

/// Compiles requests against a fixed font set.
pub struct Typesetter {
    world: world::FontSet,
    ppi: f32,
}

impl Typesetter {
    /// `ppi` sets PNG resolution (pixels per inch; Typst points are 1/72 inch). SVG ignores it.
    /// A non-finite or non-positive value falls back to 144.
    pub fn new(fonts: FontSource, ppi: f32) -> Result<Self, TypesetError> {
        let ppi = if ppi.is_finite() && ppi > 0.0 {
            ppi
        } else {
            144.0
        };
        Ok(Self {
            world: world::FontSet::load(fonts)?,
            ppi,
        })
    }

    /// Compile and render one request.
    pub fn render(&self, request: &Request) -> Result<Rendered, TypesetError> {
        if self.world.is_empty() {
            return Err(TypesetError::NoFonts);
        }
        let text = request.to_typst()?;
        let doc = self.world.compile(text)?;
        let page = doc.pages().first().ok_or(TypesetError::NoPages)?;
        match request.output {
            Output::Svg => Ok(Rendered::Svg(typst_svg::svg(
                page,
                &typst_svg::SvgOptions::default(),
            ))),
            #[cfg(feature = "png")]
            Output::Png => {
                let opts = typst_render::RenderOptions {
                    pixel_per_pt: typst::utils::Scalar::new(f64::from(self.ppi) / 72.0),
                    ..Default::default()
                };
                let pixmap = typst_render::render(page, &opts);
                let bytes = pixmap
                    .encode_png()
                    .map_err(|e| TypesetError::Encode(e.to_string()))?;
                Ok(Rendered::Png {
                    bytes,
                    width: pixmap.width(),
                    height: pixmap.height(),
                })
            }
        }
    }

    /// Configured PNG resolution.
    pub fn ppi(&self) -> f32 {
        self.ppi
    }
}
