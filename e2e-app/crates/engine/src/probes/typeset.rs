//! rightkit-typeset: render real titles through Typst with an explicit font, check the SVG's dimensions against the
//! font's own measured advances, and confirm the no-fonts and no-file-access refusals. The font is rightkit-pdf's
//! bundled Source Serif 4 (the crate bundles none), so this probe adds no asset of its own.
use rightkit_pdf::font::FontCatalog;
use rightkit_pdf::model::PdfFontResolver;
use rightkit_typeset::{FontFace, FontSource, Rendered, Request, Source, TypesetError, Typesetter};
use serde_json::{json, Value};

use super::Ctx;

const SIZE_PT: f64 = 24.0;
const MARGIN_PT: f64 = 4.0;

/// Width and height in points from the root `viewBox="0 0 W H"` of a Typst SVG.
fn svg_size(svg: &str) -> Result<(f64, f64), String> {
    let start = svg.find("viewBox=\"").ok_or("the SVG has no viewBox")? + "viewBox=\"".len();
    let rest = &svg[start..];
    let end = rest.find('"').ok_or("unterminated viewBox")?;
    let n: Vec<f64> = rest[..end]
        .split_whitespace()
        .filter_map(|t| t.parse().ok())
        .collect();
    if n.len() == 4 && n[0] == 0.0 && n[1] == 0.0 {
        Ok((n[2], n[3]))
    } else {
        Err(format!("unexpected viewBox {:?}", &rest[..end]))
    }
}

fn render(typesetter: &Typesetter, family: &str, title: &str) -> Result<(f64, f64, usize), String> {
    let mut request = Request::new(Source::Typst(title.into()));
    request.font_size_pt = SIZE_PT;
    request.margin_pt = MARGIN_PT;
    request.font_family = Some(family.into());
    match typesetter
        .render(&request)
        .map_err(|e| format!("render {title:?}: {e}"))?
    {
        Rendered::Svg(svg) => {
            if !svg.contains("<svg") || !(svg.contains("<use") || svg.contains("<path")) {
                return Err(format!("the SVG for {title:?} holds no glyph shapes"));
            }
            let (w, h) = svg_size(&svg)?;
            Ok((w, h, svg.len()))
        }
        #[allow(unreachable_patterns)]
        _ => Err("expected SVG output (feature `png` is off)".into()),
    }
}

pub fn probe(_ctx: &Ctx) -> Result<Value, String> {
    let font = FontCatalog::shared()
        .resolve("source-serif-4")
        .map_err(|e| format!("needs the bundled Source Serif 4 (rightkit-pdf assets/SourceSerif4-Variable.woff2, resolved by FontCatalog as `source-serif-4`): {e:?}"))?;
    let family = font.family.clone();

    // The font's own metrics: the reference the render is checked against.
    let face = FontFace::parse(font.bytes.clone()).map_err(|e| format!("parse font: {e}"))?;
    let short = "RightKit end to end title";
    let long = "RightKit end to end title with a considerably longer second half";
    let m_short = face.measure(short).map_err(|e| e.to_string())?;
    let m_long = face.measure(long).map_err(|e| e.to_string())?;
    if !m_short.missing.is_empty() || m_long.advance_units <= m_short.advance_units {
        return Err(format!(
            "font measurement is wrong: missing {:?}, {} vs {} units",
            m_short.missing, m_short.advance_units, m_long.advance_units
        ));
    }
    if face.measure("\u{6f22}").map_err(|e| e.to_string())?.missing != vec!['\u{6f22}'] {
        return Err("a CJK character was not reported missing from Source Serif 4".into());
    }
    let expected_short =
        m_short.advance_units as f64 * SIZE_PT / f64::from(m_short.units_per_em) + 2.0 * MARGIN_PT;

    let typesetter = Typesetter::new(FontSource::Bytes(vec![font.bytes.clone()]), 144.0)
        .map_err(|e| format!("typesetter: {e}"))?;
    let (w_short, h_short, bytes_short) = render(&typesetter, &family, short)?;
    let (w_long, h_long, bytes_long) = render(&typesetter, &family, long)?;
    if w_long <= w_short || bytes_long <= bytes_short {
        return Err(format!(
            "a longer title did not render wider: {w_short}pt -> {w_long}pt"
        ));
    }
    if (h_long - h_short).abs() > 1.0 || h_short < SIZE_PT || h_short > SIZE_PT * 3.0 {
        return Err(format!(
            "one line of {SIZE_PT}pt text rendered {h_short}pt then {h_long}pt tall"
        ));
    }
    let drift = (w_short - expected_short).abs() / expected_short;
    if drift > 0.15 {
        return Err(format!(
            "rendered width {w_short}pt is {:.1}% off the font's measured {expected_short:.1}pt",
            drift * 100.0
        ));
    }

    // Refusals.
    match Typesetter::new(FontSource::None, 144.0)
        .map_err(|e| e.to_string())?
        .render(&Request::new(Source::Typst("x".into())))
    {
        Err(TypesetError::NoFonts) => {}
        other => return Err(format!("no fonts: expected NoFonts, got {other:?}")),
    }
    let mut file_access = Request::new(Source::Typst("#image(\"x.png\")".into()));
    file_access.font_family = Some(family.clone());
    match typesetter.render(&file_access) {
        Err(TypesetError::Compile(_)) => {}
        other => {
            return Err(format!(
                "#image: expected a compile error (no file access), got {other:?}"
            ))
        }
    }
    match Typesetter::new(FontSource::Bytes(vec![vec![1, 2, 3]]), 144.0) {
        Err(TypesetError::UnreadableFonts) => {}
        Ok(_) => return Err("garbage font bytes were accepted".into()),
        Err(e) => {
            return Err(format!(
                "garbage font bytes: expected UnreadableFonts, got {e}"
            ))
        }
    }

    Ok(json!({
        "family": family,
        "shortTitle": { "widthPt": w_short, "heightPt": h_short, "measuredWidthPt": expected_short },
        "longTitle": { "widthPt": w_long, "heightPt": h_long },
        "svgBytes": [bytes_short, bytes_long],
    }))
}
