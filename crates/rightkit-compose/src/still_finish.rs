//! Keyless still finishing (feature `still-finish`; donor: GenRight `media.still_finish`). Composites a PNG logo onto each input
//! PNG at a corner, then writes every result under a naming template. Pure Rust on the existing
//! `image` crate (png feature only): no network, no provider, no ffmpeg, no new dependency.
//!
//! Safety rules: inputs and the logo are only read; an output path that already exists is refused
//! (so an input can never be overwritten); a template may not carry path separators, drive
//! prefixes, `..`, or Windows reserved names, and the rendered name is checked as one file name.
//! Every output is composited before the first write, so a bad input writes nothing.

use image::{imageops, DynamicImage, ImageFormat, Rgba, RgbaImage};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// A still-finishing failure: a stable `code` and a human message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StillFinishError {
    /// Stable, snake_case code (`invalid_input`, `exists`, `io`, ...).
    pub code: String,
    /// Human-readable message.
    pub message: String,
}

impl StillFinishError {
    /// An error with `code` and `message`.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for StillFinishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for StillFinishError {}

/// Result of every still-finishing call.
pub type StillFinishResult<T> = Result<T, StillFinishError>;

const MAX_INPUTS: usize = 500;
const MAX_TEMPLATE_LEN: usize = 120;
const MAX_MARGIN: u64 = 10_000;
const DEFAULT_TEMPLATE: &str = "{stem}-{index}";
const DEFAULT_MARGIN: u64 = 24;

/// Where the logo sits on each still.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Center,
}

impl Corner {
    pub fn parse(word: &str) -> Option<Corner> {
        Some(match word {
            "top-left" => Corner::TopLeft,
            "top-right" => Corner::TopRight,
            "bottom-left" => Corner::BottomLeft,
            "bottom-right" => Corner::BottomRight,
            "center" => Corner::Center,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Corner::TopLeft => "top-left",
            Corner::TopRight => "top-right",
            Corner::BottomLeft => "bottom-left",
            Corner::BottomRight => "bottom-right",
            Corner::Center => "center",
        }
    }
}

/// One piece of a parsed naming template.
#[derive(Debug, PartialEq, Eq)]
pub enum Part {
    Lit(String),
    Index,
    Stem,
    Date,
}

fn invalid(message: impl Into<String>) -> StillFinishError {
    StillFinishError::new("invalid_input", message)
}

/// Parse `{index}`, `{stem}` and `{date}` tokens. Literal text may not contain path separators,
/// drive colons, wildcard or control characters; any other brace is an error.
pub fn parse_template(template: &str) -> StillFinishResult<Vec<Part>> {
    if template.is_empty() || template.chars().count() > MAX_TEMPLATE_LEN {
        return Err(invalid(format!(
            "template must be 1 to {MAX_TEMPLATE_LEN} characters"
        )));
    }
    let mut parts = Vec::new();
    let mut lit = String::new();
    let mut chars = template.chars();
    while let Some(c) = chars.next() {
        match c {
            '{' => {
                let mut token = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(x) => token.push(x),
                        None => return Err(invalid("template: unclosed '{'")),
                    }
                }
                if !lit.is_empty() {
                    parts.push(Part::Lit(std::mem::take(&mut lit)));
                }
                parts.push(match token.as_str() {
                    "index" => Part::Index,
                    "stem" => Part::Stem,
                    "date" => Part::Date,
                    other => {
                        return Err(invalid(format!(
                            "template: unknown token {{{other}}}; allowed: {{index}}, {{stem}}, {{date}}"
                        )))
                    }
                });
            }
            '}' => return Err(invalid("template: stray '}'")),
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => {
                return Err(invalid(format!(
                    "template: character {c:?} is not allowed in a file name"
                )))
            }
            c if c.is_control() => {
                return Err(invalid("template: control characters are not allowed"))
            }
            c => lit.push(c),
        }
    }
    if !lit.is_empty() {
        parts.push(Part::Lit(lit));
    }
    Ok(parts)
}

/// Refuse anything that is not one plain file-name component.
fn check_component(base: &str) -> StillFinishResult<()> {
    let reserved = {
        let upper = base.to_ascii_uppercase();
        let stem = upper.split('.').next().unwrap_or("");
        matches!(stem, "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.as_bytes()[3].is_ascii_digit()
                && stem.as_bytes()[3] != b'0')
    };
    let bad = base.is_empty()
        || base.starts_with('.')
        || base.ends_with('.')
        || base.starts_with(' ')
        || base.ends_with(' ')
        || base
            .chars()
            .any(|c| matches!(c, '/' | '\\' | ':') || c.is_control())
        || reserved;
    if bad {
        return Err(invalid(format!(
            "rendered name {base:?} is not a plain, safe file name"
        )));
    }
    Ok(())
}

/// Render one output file name (with `.png`) for input number `index` (1-based).
pub fn render_name(
    parts: &[Part],
    index: usize,
    width: usize,
    stem: &str,
    date: &str,
) -> StillFinishResult<String> {
    let mut base = String::new();
    for part in parts {
        match part {
            Part::Lit(s) => base.push_str(s),
            Part::Index => base.push_str(&format!("{index:0width$}")),
            Part::Stem => base.push_str(stem),
            Part::Date => base.push_str(date),
        }
    }
    if base.to_ascii_lowercase().ends_with(".png") {
        base.truncate(base.len() - 4);
    }
    check_component(&base)?;
    Ok(format!("{base}.png"))
}

/// Proleptic Gregorian date (`YYYY-MM-DD`) for a count of days since 1970-01-01 (rightkit-time `civil_from_days`).
pub fn civil_date(days: i64) -> String {
    let (y, m, d) = rightkit_time::civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

fn utc_today() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    civil_date((secs / 86_400) as i64)
}

fn valid_date(word: &str) -> bool {
    let b = word.as_bytes();
    if b.len() != 10 {
        return false;
    }
    // Check the shape on bytes first: only then is slicing at fixed offsets safe.
    let shape_ok = b.iter().enumerate().all(|(i, c)| match i {
        4 | 7 => *c == b'-',
        _ => c.is_ascii_digit(),
    });
    if !shape_ok {
        return false;
    }
    let month: u32 = word[5..7].parse().unwrap_or(0);
    let day: u32 = word[8..10].parse().unwrap_or(0);
    (1..=12).contains(&month) && (1..=31).contains(&day)
}

/// Top-left position of a `lw` x `lh` logo on a `bw` x `bh` still.
pub fn place(
    corner: Corner,
    bw: u32,
    bh: u32,
    lw: u32,
    lh: u32,
    margin: u32,
) -> StillFinishResult<(u32, u32)> {
    if lw > bw || lh > bh {
        return Err(StillFinishError::new(
            "logo_too_large",
            format!("logo {lw}x{lh} is larger than the still {bw}x{bh}"),
        ));
    }
    let (bw, bh, lw, lh, m) = (
        u64::from(bw),
        u64::from(bh),
        u64::from(lw),
        u64::from(lh),
        u64::from(margin),
    );
    if corner != Corner::Center && (lw + 2 * m > bw || lh + 2 * m > bh) {
        return Err(StillFinishError::new(
            "logo_too_large",
            format!("logo {lw}x{lh} with margin {m} does not fit the still {bw}x{bh}"),
        ));
    }
    let (x, y) = match corner {
        Corner::TopLeft => (m, m),
        Corner::TopRight => (bw - lw - m, m),
        Corner::BottomLeft => (m, bh - lh - m),
        Corner::BottomRight => (bw - lw - m, bh - lh - m),
        Corner::Center => ((bw - lw) / 2, (bh - lh) / 2),
    };
    Ok((x as u32, y as u32))
}

/// Alpha-over the logo (scaled to `scale` of the still's width, opacity multiplied into its alpha)
/// onto the still. Deterministic: same inputs give the same pixels.
pub fn composite(
    base: &RgbaImage,
    logo: &RgbaImage,
    corner: Corner,
    margin: u32,
    opacity: f64,
    scale: f64,
) -> StillFinishResult<RgbaImage> {
    let (bw, bh) = base.dimensions();
    let tw = ((f64::from(bw) * scale).round() as u32).max(1);
    let th = ((f64::from(logo.height()) * f64::from(tw) / f64::from(logo.width())).round() as u32)
        .max(1);
    let scaled = imageops::resize(logo, tw, th, imageops::FilterType::Lanczos3);
    let (x0, y0) = place(corner, bw, bh, tw, th, margin)?;
    let mut out = base.clone();
    for ly in 0..th {
        for lx in 0..tw {
            let f = scaled.get_pixel(lx, ly);
            let fa = f[3] as f32 / 255.0 * opacity as f32;
            if fa <= 0.0 {
                continue;
            }
            let b = *out.get_pixel(x0 + lx, y0 + ly);
            let ba = b[3] as f32 / 255.0;
            let oa = fa + ba * (1.0 - fa);
            let mut px = [0u8; 4];
            for c in 0..3 {
                let v = (f[c] as f32 * fa + b[c] as f32 * ba * (1.0 - fa)) / oa;
                px[c] = v.round().clamp(0.0, 255.0) as u8;
            }
            px[3] = (oa * 255.0).round().clamp(0.0, 255.0) as u8;
            out.put_pixel(x0 + lx, y0 + ly, Rgba(px));
        }
    }
    Ok(out)
}

fn load_png(path: &Path, what: &str) -> StillFinishResult<RgbaImage> {
    if !path.is_file() {
        return Err(StillFinishError::new(
            "not_found",
            format!("{what} is not a file: {}", path.display()),
        ));
    }
    let bytes = fs::read(path).map_err(|e| {
        StillFinishError::new(
            "io_failed",
            format!("cannot read {what} {}: {e}", path.display()),
        )
    })?;
    let img = image::load_from_memory_with_format(&bytes, ImageFormat::Png).map_err(|e| {
        invalid(format!(
            "{what} is not a readable PNG: {}: {e}",
            path.display()
        ))
    })?;
    Ok(DynamicImage::to_rgba8(&img))
}

fn str_field<'a>(input: &'a Value, key: &str) -> StillFinishResult<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid(format!("missing field: {key}")))
}

fn f64_field(
    input: &Value,
    key: &str,
    default: f64,
    ok: impl Fn(f64) -> bool,
    rule: &str,
) -> StillFinishResult<f64> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v
            .as_f64()
            .filter(|x| x.is_finite() && ok(*x))
            .ok_or_else(|| invalid(format!("{key} must be a number {rule}"))),
    }
}

/// `media.still_finish` `{inputs:[paths], logo, output_dir, position?, margin?, opacity?, scale?,
/// template?, date?}` → `{count, written, dry_run, date, position, margin, opacity, scale, template,
/// outputs:[{index, input, output, name, sha256, bytes, width, height}]}`.
pub fn run(input: &Value, dry_run: bool) -> StillFinishResult<Value> {
    let raw_inputs = input
        .get("inputs")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("missing field: inputs (array of PNG paths)"))?;
    if raw_inputs.is_empty() || raw_inputs.len() > MAX_INPUTS {
        return Err(invalid(format!("inputs must hold 1 to {MAX_INPUTS} paths")));
    }
    let mut paths = Vec::with_capacity(raw_inputs.len());
    for v in raw_inputs {
        let s = v
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid("inputs must be non-empty path strings"))?;
        paths.push(PathBuf::from(s));
    }
    let logo_path = PathBuf::from(str_field(input, "logo")?);
    let out_dir = PathBuf::from(str_field(input, "output_dir")?);
    let corner = match input.get("position") {
        None | Some(Value::Null) => Corner::BottomRight,
        Some(v) => v.as_str().and_then(Corner::parse).ok_or_else(|| {
            invalid("position must be top-left, top-right, bottom-left, bottom-right or center")
        })?,
    };
    let margin = match input.get("margin") {
        None | Some(Value::Null) => DEFAULT_MARGIN,
        Some(v) => v
            .as_u64()
            .filter(|m| *m <= MAX_MARGIN)
            .ok_or_else(|| invalid(format!("margin must be an integer from 0 to {MAX_MARGIN}")))?,
    };
    let opacity = f64_field(
        input,
        "opacity",
        1.0,
        |x| (0.0..=1.0).contains(&x),
        "from 0 to 1",
    )?;
    let scale = f64_field(
        input,
        "scale",
        0.2,
        |x| x > 0.0 && x <= 1.0,
        "greater than 0 and at most 1",
    )?;
    let template = match input.get("template") {
        None | Some(Value::Null) => DEFAULT_TEMPLATE,
        Some(v) => v
            .as_str()
            .ok_or_else(|| invalid("template must be a string"))?,
    };
    let parts = parse_template(template)?;
    let date = match input.get("date") {
        None | Some(Value::Null) => utc_today(),
        Some(v) => v
            .as_str()
            .filter(|s| valid_date(s))
            .map(str::to_string)
            .ok_or_else(|| invalid("date must be YYYY-MM-DD"))?,
    };

    let logo = load_png(&logo_path, "logo")?;
    let width = raw_inputs.len().to_string().len();

    // Plan every name before reading any pixels, so a bad template writes nothing.
    let mut seen = HashSet::new();
    let mut names = Vec::with_capacity(paths.len());
    for (i, p) in paths.iter().enumerate() {
        let stem = p
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| invalid(format!("input has no usable file name: {}", p.display())))?;
        let name = render_name(&parts, i + 1, width, stem, &date)?;
        if !seen.insert(name.to_ascii_lowercase()) {
            return Err(StillFinishError::new(
                "duplicate_output",
                format!("two inputs render to {name:?}; add {{index}} to the template"),
            ));
        }
        let target = out_dir.join(&name);
        if target.exists() {
            return Err(StillFinishError::new(
                "output_exists",
                format!("{} already exists; outputs are never replaced and inputs are never overwritten", target.display()),
            ));
        }
        names.push(name);
    }

    // Composite everything in memory first.
    let mut staged: Vec<(PathBuf, Vec<u8>, Value)> = Vec::with_capacity(paths.len());
    for (i, (p, name)) in paths.iter().zip(names.iter()).enumerate() {
        let base = load_png(p, "input")?;
        let img = composite(&base, &logo, corner, margin as u32, opacity, scale)?;
        let (width_px, height_px) = img.dimensions();
        let mut buf = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(img)
            .write_to(&mut buf, ImageFormat::Png)
            .map_err(|e| StillFinishError::new("encode_failed", e.to_string()))?;
        let bytes = buf.into_inner();
        let target = out_dir.join(name);
        let record = json!({
            "index": i + 1,
            "input": p.to_string_lossy(),
            "output": target.to_string_lossy(),
            "name": name,
            "sha256": rightkit_crypto::hex_encode(&rightkit_crypto::sha256(&bytes)),
            "bytes": bytes.len(),
            "width": width_px,
            "height": height_px,
        });
        staged.push((target, bytes, record));
    }

    if !dry_run {
        fs::create_dir_all(&out_dir).map_err(|e| {
            StillFinishError::new(
                "io_failed",
                format!("cannot create {}: {e}", out_dir.display()),
            )
        })?;
        for (target, bytes, _) in &staged {
            // NoReplace: never replaces a file that appeared after planning.
            let options = rightkit_fs::AtomicWriteOptions {
                replace: rightkit_fs::ReplaceMode::NoReplace,
                ..Default::default()
            };
            rightkit_fs::atomic_write_with_options(target, options, |file| file.write_all(bytes))
                .map_err(|e| match e {
                rightkit_fs::FsError::AlreadyExists(_) => StillFinishError::new(
                    "output_exists",
                    format!("cannot create {}: {e}", target.display()),
                ),
                e => StillFinishError::new(
                    "io_failed",
                    format!("cannot write {}: {e}", target.display()),
                ),
            })?;
        }
    }

    let outputs: Vec<Value> = staged.into_iter().map(|(_, _, record)| record).collect();
    Ok(json!({
        "count": outputs.len(),
        "written": !dry_run,
        "dry_run": dry_run,
        "date": date,
        "position": corner.as_str(),
        "margin": margin,
        "opacity": opacity,
        "scale": scale,
        "template": template,
        "outputs": outputs,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_accepts_the_three_tokens_and_literals() {
        let parts = parse_template("{stem}-{index}_{date}").unwrap();
        assert_eq!(
            parts,
            vec![
                Part::Stem,
                Part::Lit("-".into()),
                Part::Index,
                Part::Lit("_".into()),
                Part::Date
            ]
        );
    }

    #[test]
    fn template_refuses_traversal_and_bad_tokens() {
        for bad in [
            "../x", "..\\x", "a/b", "a\\b", "C:x", "{foo}", "{stem", "a}b", "", "a\u{7}b",
        ] {
            assert!(parse_template(bad).is_err(), "{bad:?} must be refused");
        }
        let long = "x".repeat(MAX_TEMPLATE_LEN + 1);
        assert!(parse_template(&long).is_err());
    }

    #[test]
    fn rendered_names_are_single_safe_components() {
        let parts = parse_template("{stem}").unwrap();
        assert!(render_name(&parts, 1, 1, "..", "2026-10-17").is_err());
        assert!(render_name(&parts, 1, 1, ".hidden", "2026-10-17").is_err());
        assert!(render_name(&parts, 1, 1, "CON", "2026-10-17").is_err());
        assert!(render_name(&parts, 1, 1, "com1", "2026-10-17").is_err());
        assert!(render_name(&parts, 1, 1, "a\\b", "2026-10-17").is_err());
        assert_eq!(
            render_name(&parts, 1, 1, "shot", "2026-10-17").unwrap(),
            "shot.png"
        );
    }

    #[test]
    fn index_is_padded_to_the_batch_width_and_png_suffix_is_not_doubled() {
        let parts = parse_template("{stem}-{index}").unwrap();
        assert_eq!(
            render_name(&parts, 7, 3, "shot", "2026-10-17").unwrap(),
            "shot-007.png"
        );
        let parts = parse_template("{stem}.PNG").unwrap();
        assert_eq!(
            render_name(&parts, 1, 1, "shot", "2026-10-17").unwrap(),
            "shot.png"
        );
    }

    #[test]
    fn date_token_renders_the_given_day() {
        let parts = parse_template("{date}-{index}").unwrap();
        assert_eq!(
            render_name(&parts, 2, 1, "x", "2024-02-29").unwrap(),
            "2024-02-29-2.png"
        );
        assert!(valid_date("2024-02-29"));
        assert!(!valid_date("2024-13-01"));
        assert!(!valid_date("2024/02/29"));
    }

    #[test]
    fn civil_date_matches_known_days() {
        assert_eq!(civil_date(0), "1970-01-01");
        assert_eq!(civil_date(11_017), "2000-03-01");
        assert_eq!(civil_date(19_782), "2024-02-29");
    }

    #[test]
    fn corners_and_centre_land_inside_the_margin() {
        assert_eq!(place(Corner::TopLeft, 10, 10, 2, 2, 1).unwrap(), (1, 1));
        assert_eq!(place(Corner::TopRight, 10, 10, 2, 2, 1).unwrap(), (7, 1));
        assert_eq!(place(Corner::BottomLeft, 10, 10, 2, 2, 1).unwrap(), (1, 7));
        assert_eq!(place(Corner::BottomRight, 10, 10, 2, 2, 1).unwrap(), (7, 7));
        assert_eq!(place(Corner::Center, 10, 10, 2, 2, 99).unwrap(), (4, 4));
        assert_eq!(
            place(Corner::BottomRight, 10, 10, 2, 2, 5)
                .unwrap_err()
                .code,
            "logo_too_large"
        );
        assert_eq!(
            place(Corner::Center, 4, 4, 5, 2, 0).unwrap_err().code,
            "logo_too_large"
        );
    }

    #[test]
    fn composite_blends_opacity_and_leaves_other_pixels_alone() {
        let base = RgbaImage::from_pixel(4, 4, Rgba([0, 0, 255, 255]));
        let logo = RgbaImage::from_pixel(1, 1, Rgba([255, 255, 255, 255]));
        let out = composite(&base, &logo, Corner::TopLeft, 0, 0.5, 0.25).unwrap();
        let px = out.get_pixel(0, 0);
        assert!(
            (127..=129).contains(&px[0]),
            "half-opaque white over blue: {px:?}"
        );
        assert_eq!(px[3], 255);
        assert_eq!(out.get_pixel(1, 0), &Rgba([0, 0, 255, 255]));
        assert_eq!(
            base.get_pixel(0, 0),
            &Rgba([0, 0, 255, 255]),
            "the base is not mutated"
        );
    }

    #[test]
    fn composite_is_deterministic() {
        let base = RgbaImage::from_fn(9, 7, |x, y| Rgba([(x * 20) as u8, (y * 30) as u8, 90, 255]));
        let logo = RgbaImage::from_fn(5, 3, |x, _| Rgba([200, 10, (x * 60) as u8, 180]));
        let a = composite(&base, &logo, Corner::BottomRight, 1, 0.8, 0.5).unwrap();
        let b = composite(&base, &logo, Corner::BottomRight, 1, 0.8, 0.5).unwrap();
        assert_eq!(a.as_raw(), b.as_raw());
    }
}
