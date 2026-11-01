//! Styled burn-in: [`CaptionStyle`] + [`SafeZone`] -> `drawtext`/`drawbox`.
//!
//! The editing FFmpeg has no libass, so every look is layered `drawtext`:
//!
//! - **Metrics.** Known fonts use their actual glyph advances for line widths,
//!   box sizes and per-word offsets. Without a face, segmentation retains its
//!   character-count limits.
//! - **Layers per line**, bottom to top: outline+shadow layer (outline colour,
//!   `borderw`), text layer (text colour; a thin same-colour stroke makes the
//!   regular face read as bold). Lines are drawn one `drawtext` each at
//!   explicit integer positions.
//! - **Box**: `drawbox`; rounded corners are four disjoint stepped bands per
//!   corner (no alpha mask, no extra inputs, so the filters stay a plain chain).
//! - **Karaoke / emphasis**: extra single-word `drawtext` layers on top of the
//!   base text, in the highlight colour, at the word's exact column, enabled
//!   for the word's frame range (`between(n,a,b)`).
//!
//! All text goes through content-addressed `textfile=` with `expansion=none`.

use rightkit_timeline::title::{escape_filter_path, ADVANCE_PERMILLE};
use rightkit_timeline::{Fps, TextFile, Ticks, TimelineError, TitleAssets};

use crate::font::FontFace;
use crate::safezone::{Rect, SafeZone};
use crate::segment::{normalize_words, CaptionConfig, Cue, LineMetrics};
use crate::style::{CaptionStyle, Color, KaraokeMode, Orientation, StyleCatalog, Weight};
use crate::words::TimedWord;

/// Line height of the caption font (JetBrains Mono: 1320/1000 em).
const LINE_HEIGHT_PERMILLE: i64 = 1320;

/// Output frame and timing for a burn-in.
#[derive(Debug, Clone, Copy)]
pub struct CaptionTarget {
    pub width: i64,
    pub height: i64,
    pub fps: Fps,
    pub total_frames: i64,
}

/// Words drawn in the style's `emphasis_color`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Emphasis {
    /// Matched case-insensitively, ignoring punctuation.
    pub words: Vec<String>,
    /// Also emphasise any word containing a digit ("100%", "3x").
    pub numbers: bool,
}

impl Emphasis {
    fn key(s: &str) -> String {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    }

    pub fn matches(&self, word: &str) -> bool {
        (self.numbers && word.chars().any(|c| c.is_ascii_digit()))
            || self.words.iter().any(|w| Self::key(w) == Self::key(word))
    }
}

/// Integer pixel geometry of a style on a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub font_px: i64,
    /// Monospace fallback advance.
    pub adv: i64,
    pub line_h: i64,
    pub hpad: i64,
    pub vpad: i64,
    pub radius: i64,
    pub outline_px: i64,
    pub shadow_px: i64,
    pub bold_px: i64,
}

impl Geometry {
    pub fn new(style: &CaptionStyle, w: i64, h: i64) -> Self {
        let o = Orientation::of(w, h);
        let short = w.min(h);
        let raw = short * style.font.get(o) / 1000;
        let font_px = ((raw + 2) / 5 * 5).max(10);
        let pct = |p: i64| (font_px * p + 50) / 100;
        let (hpad, vpad, radius) = style.boxed.as_ref().map_or((0, 0, 0), |b| {
            (pct(b.hpad_pct), pct(b.vpad_pct), pct(b.radius_pct))
        });
        Self {
            font_px,
            adv: font_px * ADVANCE_PERMILLE / 1000,
            line_h: (font_px * LINE_HEIGHT_PERMILLE + 500) / 1000,
            hpad,
            vpad,
            radius,
            outline_px: style.outline.as_ref().map_or(0, |o| pct(o.pct).max(1)),
            shadow_px: style.shadow.as_ref().map_or(0, |s| pct(s.pct).max(1)),
            bold_px: if style.weight == Weight::Bold {
                (font_px / 40).max(1)
            } else {
                0
            },
        }
    }
}

/// Where the box bottom sits before safe-zone adjustment.
fn anchor_bottom(style: &CaptionStyle, zone: &SafeZone, w: i64, h: i64) -> i64 {
    let o = Orientation::of(w, h);
    (h * style.anchor.get(o) / 1000).min(zone.content_box(w, h).bottom())
}

/// Widest box (px) that stays inside the margin box and clear of side rails
/// that overlap the anchored caption band.
pub fn max_box_width(style: &CaptionStyle, zone: &SafeZone, w: i64, h: i64) -> i64 {
    let o = Orientation::of(w, h);
    let g = Geometry::new(style, w, h);
    let cb = zone.content_box(w, h);
    let mut cap = (w * style.max_width.get(o) / 1000).min(cb.w);
    let bottom = anchor_bottom(style, zone, w, h);
    let lines = style.max_lines.unwrap_or(2) as i64;
    let band = Rect {
        x: 0,
        y: bottom - (lines * g.line_h + 2 * g.vpad),
        w,
        h: lines * g.line_h + 2 * g.vpad,
    };
    let (cx, gap) = (w / 2, w / 100);
    for e in &zone.exclusions {
        let r = e.rect(w, h);
        if r.w >= w * 9 / 10 || r.h < h * 3 / 10 || !r.intersects(&band) {
            continue; // full-width bands move up; only tall side rails narrow the box
        }
        if r.x >= cx {
            cap = cap.min(2 * (r.x - cx - gap));
        } else if r.right() <= cx {
            cap = cap.min(2 * (cx - r.right() - gap));
        }
    }
    cap.max(1)
}

/// Segmentation limits for a style on a platform frame: the catalogue's
/// per-orientation defaults, capped by the style and by what fits the safe box.
pub fn style_config(
    style: &CaptionStyle,
    catalog: &StyleCatalog,
    zone: &SafeZone,
    w: i64,
    h: i64,
) -> CaptionConfig {
    let mut cfg = catalog.segmentation_for(Orientation::of(w, h));
    let g = Geometry::new(style, w, h);
    let fit = ((max_box_width(style, zone, w, h) - 2 * g.hpad) / g.adv).max(1) as usize;
    cfg.max_chars_per_line = cfg
        .max_chars_per_line
        .min(style.max_chars_per_line.unwrap_or(usize::MAX))
        .min(fit);
    if let Some(l) = style.max_lines {
        cfg.max_lines = l;
    }
    cfg
}

/// Text width inside the safe box, including room for strokes and shadows.
pub fn line_metrics<'a>(
    style: &CaptionStyle,
    zone: &SafeZone,
    w: i64,
    h: i64,
    face: &'a FontFace,
) -> LineMetrics<'a> {
    let g = Geometry::new(style, w, h);
    let edge = g.hpad.max(g.outline_px + g.bold_px + g.shadow_px + 1);
    LineMetrics {
        face,
        font_px: g.font_px as u32,
        max_width_px: (max_box_width(style, zone, w, h) - 2 * edge).max(0) as u64,
        uppercase: style.uppercase,
    }
}

/// Places a `box_w` x `box_h` box: centred, bottom at the style anchor, moved
/// up in 1% steps until clear of every exclusion, kept inside the margin box.
pub fn place_box(
    style: &CaptionStyle,
    zone: &SafeZone,
    w: i64,
    h: i64,
    box_w: i64,
    box_h: i64,
) -> Rect {
    let cb = zone.content_box(w, h);
    let mut bottom = anchor_bottom(style, zone, w, h);
    let step = (h / 100).max(1);
    loop {
        let r = Rect {
            x: (w - box_w) / 2,
            y: bottom - box_h,
            w: box_w,
            h: box_h,
        };
        let clear = !zone.exclusions.iter().any(|e| r.intersects(&e.rect(w, h)));
        if clear || r.y - step < cb.y {
            return Rect {
                y: r.y.max(cb.y),
                ..r
            };
        }
        bottom -= step;
    }
}

/// The box a cue's (display) lines occupy after safe-zone placement.
pub fn cue_rect(style: &CaptionStyle, zone: &SafeZone, w: i64, h: i64, lines: &[String]) -> Rect {
    let g = Geometry::new(style, w, h);
    let maxlen = lines
        .iter()
        .map(|l| len(&display(style, l)))
        .max()
        .unwrap_or(0);
    place_box(
        style,
        zone,
        w,
        h,
        maxlen * g.adv + 2 * g.hpad,
        lines.len() as i64 * g.line_h + 2 * g.vpad,
    )
}

fn display(style: &CaptionStyle, s: &str) -> String {
    if style.uppercase {
        s.to_uppercase()
    } else {
        s.to_string()
    }
}

fn len(s: &str) -> i64 {
    s.chars().count() as i64
}

struct Gen<'a> {
    font: String,
    assets: &'a TitleAssets<'a>,
    files: Vec<TextFile>,
}

impl Gen<'_> {
    /// Path (escaped for the filtergraph) of the content-addressed text file.
    fn file(&mut self, content: &str) -> Result<String, TimelineError> {
        let hash = blake3::hash(content.as_bytes()).to_hex();
        let path = self
            .assets
            .text_dir
            .join(format!("caption-{}.txt", &hash[..16]));
        let esc = escape_filter_path(&path)?;
        if !self.files.iter().any(|t| t.path == path) {
            self.files.push(TextFile {
                path,
                content: content.to_string(),
            });
        }
        Ok(esc)
    }

    #[allow(clippy::too_many_arguments)]
    fn text(
        &mut self,
        content: &str,
        px: i64,
        color: &str,
        stroke: Option<(&str, i64)>,
        shadow: Option<(&str, i64)>,
        x: i64,
        y: i64,
        enable: &str,
    ) -> Result<String, TimelineError> {
        let file = self.file(content)?;
        let mut f = format!(
            "drawtext=fontfile={}:textfile={file}:expansion=none:fontsize={px}:fontcolor={color}:x={x}:y={y}",
            self.font
        );
        if let Some((c, w)) = stroke.filter(|s| s.1 > 0) {
            f += &format!(":bordercolor={c}:borderw={w}");
        }
        if let Some((c, d)) = shadow.filter(|s| s.1 > 0) {
            f += &format!(":shadowcolor={c}:shadowx={d}:shadowy={d}");
        }
        f += &format!(":enable='{enable}'");
        Ok(f)
    }
}

/// Filled rectangle, with stepped (disjoint) rounded corners when `radius` > 1.
fn box_filters(r: Rect, color: &str, radius: i64, enable: &str) -> Vec<String> {
    let bx = |x: i64, y: i64, w: i64, h: i64| {
        format!("drawbox=x={x}:y={y}:w={w}:h={h}:color={color}:t=fill:enable='{enable}'")
    };
    let rad = radius.min(r.w / 2).min(r.h / 2);
    if rad < 2 {
        return vec![bx(r.x, r.y, r.w, r.h)];
    }
    let bands = rad.min(4);
    let mut out = vec![bx(r.x, r.y + rad, r.w, r.h - 2 * rad)];
    for i in 0..bands {
        let (r0, r1) = (i * rad / bands, (i + 1) * rad / bands);
        if r1 == r0 {
            continue;
        }
        // Circle inset at the band's centre row (distance dy from the circle centre row).
        let dy = rad as f64 - (r0 + r1) as f64 / 2.0;
        let inset = (rad as f64 - ((rad * rad) as f64 - dy * dy).max(0.0).sqrt()).round() as i64;
        out.push(bx(r.x + inset, r.y + r0, r.w - 2 * inset, r1 - r0));
        out.push(bx(r.x + inset, r.bottom() - r1, r.w - 2 * inset, r1 - r0));
    }
    out
}

/// Builds cues for a style on a platform frame (config from [`style_config`]).
pub fn build_styled_cues(
    words: &[TimedWord],
    style: &CaptionStyle,
    catalog: &StyleCatalog,
    zone: &SafeZone,
    w: i64,
    h: i64,
) -> Vec<Cue> {
    crate::segment::build_cues(words, &style_config(style, catalog, zone, w, h))
}

/// Font-aware style segmentation; measured widths own line wrapping.
pub fn build_styled_cues_with_font(
    words: &[TimedWord],
    style: &CaptionStyle,
    catalog: &StyleCatalog,
    zone: &SafeZone,
    w: i64,
    h: i64,
    face: Option<&FontFace>,
) -> Result<Vec<Cue>, String> {
    let Some(face) = face else {
        return Ok(build_styled_cues(words, style, catalog, zone, w, h));
    };
    let mut cfg = catalog.segmentation_for(Orientation::of(w, h));
    cfg.max_chars_per_line = cfg
        .max_chars_per_line
        .min(style.max_chars_per_line.unwrap_or(usize::MAX));
    if let Some(lines) = style.max_lines {
        cfg.max_lines = lines;
    }
    crate::segment::build_cues_with_metrics(
        words,
        &cfg,
        Some(&line_metrics(style, zone, w, h, face)),
    )
}

/// Styled burn-in filters (cue order) and the text files they read.
///
/// `words` are the same output-time words the `cues` were built from (karaoke
/// and emphasis need per-word timing). Cue skipping matches [`caption_filters`]
/// (cues past `total_frames` or overlapping after frame rounding are dropped).
///
/// [`caption_filters`]: crate::caption_filters
pub fn styled_caption_filters(
    words: &[TimedWord],
    cues: &[Cue],
    style: &CaptionStyle,
    zone: &SafeZone,
    emphasis: &Emphasis,
    target: &CaptionTarget,
    assets: &TitleAssets<'_>,
) -> Result<(Vec<String>, Vec<TextFile>), TimelineError> {
    styled_caption_filters_with_font(words, cues, style, zone, emphasis, target, assets, None)
}

/// Styled burn-in with measured line widths and word offsets for a known face.
#[allow(clippy::too_many_arguments)]
pub fn styled_caption_filters_with_font(
    words: &[TimedWord],
    cues: &[Cue],
    style: &CaptionStyle,
    zone: &SafeZone,
    emphasis: &Emphasis,
    target: &CaptionTarget,
    assets: &TitleAssets<'_>,
    face: Option<&FontFace>,
) -> Result<(Vec<String>, Vec<TextFile>), TimelineError> {
    let (w, h) = (target.width, target.height);
    let g = Geometry::new(style, w, h);
    let metrics = face.map(|face| line_metrics(style, zone, w, h, face));
    let width = |text: &str| -> Result<i64, TimelineError> {
        match face {
            Some(face) => face
                .measure(text)
                .map(|m| m.width_px(g.font_px as u32) as i64)
                .map_err(|e| TimelineError::Unsupported(e.to_string())),
            None => Ok(len(text) * g.adv),
        }
    };
    let norm = normalize_words(words);
    let mut gen = Gen {
        font: escape_filter_path(assets.font)?,
        assets,
        files: Vec::new(),
    };
    let frame = |ms: u64| target.fps.ticks_to_frame(Ticks::from_millis(ms as i64));
    let col = |s: &str, a: i64| Color::parse(s).expect("validated").ffmpeg(a);
    let text_c = col(&style.text_color, 100);
    let outline = style
        .outline
        .as_ref()
        .map(|o| (col(&o.color, 100), g.outline_px));
    let shadow = style
        .shadow
        .as_ref()
        .map(|s| (col(&s.color, s.alpha_pct), g.shadow_px));
    let bold = (g.bold_px > 0).then(|| (text_c.clone(), g.bold_px));
    let emph_c = style.emphasis_color.as_deref().map(|c| col(c, 100));
    let hi = style.karaoke.as_ref().map(|k| {
        let c = Color::parse(&k.highlight_color).expect("validated");
        let d = c.rgb.map(|v| (v as u16 * 11 / 20) as u8);
        (
            col(&k.highlight_color, 100),
            Color { rgb: d }.ffmpeg(100),
            k.mode,
        )
    });

    let mut filters = Vec::new();
    let mut prev_end = 0i64;
    for cue in cues {
        let lines = if let Some(metrics) = metrics.as_ref() {
            let mut lines = Vec::new();
            for line in &cue.lines {
                lines.extend(
                    metrics
                        .wrap(line, style.max_chars_per_line.unwrap_or(usize::MAX))
                        .map_err(TimelineError::Unsupported)?,
                );
            }
            lines
        } else {
            cue.lines.clone()
        };
        let lines: Vec<_> = lines.iter().map(|line| display(style, line)).collect();
        if lines.iter().all(|l| l.trim().is_empty()) {
            continue;
        }
        let start = frame(cue.start_ms);
        let end = frame(cue.end_ms).max(start + 1);
        if start >= target.total_frames || start < prev_end {
            continue;
        }
        let end = end.min(target.total_frames);
        prev_end = end;
        let enable = format!("between(n,{start},{})", end - 1);

        let widths = lines
            .iter()
            .map(|l| width(l))
            .collect::<Result<Vec<_>, _>>()?;
        let max_width = widths.iter().copied().max().unwrap_or(0);
        let rect = place_box(
            style,
            zone,
            w,
            h,
            max_width + 2 * g.hpad,
            lines.len() as i64 * g.line_h + 2 * g.vpad,
        );
        if let Some(b) = &style.boxed {
            let c = col(&b.color, b.alpha_pct);
            filters.extend(box_filters(rect, &c, g.radius, &enable));
        }
        let line_x = |k: usize| (w - widths[k]) / 2;
        let line_y = |k: usize| rect.y + g.vpad + k as i64 * g.line_h;

        for (k, l) in lines.iter().enumerate() {
            if l.trim().is_empty() {
                continue;
            }
            let (x, y) = (line_x(k), line_y(k));
            let b = bold.as_ref().map(|(c, w)| (c.as_str(), *w));
            if let Some((oc, ow)) = &outline {
                let sh = shadow.as_ref().map(|(c, d)| (c.as_str(), *d));
                filters.push(gen.text(
                    l,
                    g.font_px,
                    oc,
                    Some((oc, ow + g.bold_px)),
                    sh,
                    x,
                    y,
                    &enable,
                )?);
                filters.push(gen.text(l, g.font_px, &text_c, b, None, x, y, &enable)?);
            } else {
                let sh = shadow.as_ref().map(|(c, d)| (c.as_str(), *d));
                filters.push(gen.text(l, g.font_px, &text_c, b, sh, x, y, &enable)?);
            }
        }

        // Per-word layers: need (line, column) of each word of the cue.
        let cue_words = norm.get(cue.first_word..=cue.last_word.min(norm.len().saturating_sub(1)));
        let Some(cue_words) = cue_words else { continue };
        if emph_c.is_none() && hi.is_none() {
            continue;
        }
        let mut pos: Vec<(usize, i64)> = Vec::new(); // (line, col) per word
        let (mut line, mut c) = (0usize, 0i64);
        let mut aligned = true;
        for wd in cue_words {
            let wl = len(&display(style, &wd.text));
            if c > 0 && line < lines.len() && c + wl > len(&lines[line]) {
                line += 1;
                c = 0;
            }
            let Some(l) = lines.get(line) else {
                aligned = false;
                break;
            };
            let at: String = l.chars().skip(c as usize).take(wl as usize).collect();
            if at != display(style, &wd.text) {
                aligned = false;
                break;
            }
            pos.push((line, c));
            c += wl + 1;
        }
        if !aligned {
            continue;
        }
        let stroke = |c: &str| (g.bold_px > 0).then(|| (c.to_string(), g.bold_px));
        let word_layer = |gen: &mut Gen,
                          i: usize,
                          color: &str,
                          a: i64,
                          b: i64|
         -> Result<Option<String>, TimelineError> {
            if b < a {
                return Ok(None);
            }
            let (ln, cl) = pos[i];
            let st = stroke(color);
            Ok(Some(gen.text(
                &display(style, &cue_words[i].text),
                g.font_px,
                color,
                st.as_ref().map(|(c, w)| (c.as_str(), *w)),
                None,
                line_x(ln) + width(&lines[ln].chars().take(cl as usize).collect::<String>())?,
                line_y(ln),
                &format!("between(n,{a},{b})"),
            )?))
        };
        if let Some(ec) = &emph_c {
            for (i, wd) in cue_words.iter().enumerate() {
                if emphasis.matches(&wd.text) {
                    filters.extend(word_layer(&mut gen, i, ec, start, end - 1)?);
                }
            }
        }
        if let Some((hc, stayed_c, mode)) = &hi {
            // Progressive keeps earlier words lit in a dimmed fill while only the
            // active word carries the full highlight colour.
            let stayed = match mode {
                KaraokeMode::Progressive => Some(stayed_c.as_str()),
                KaraokeMode::Active => None,
            };
            for i in 0..cue_words.len() {
                let a = frame(cue_words[i].start_ms).max(start);
                let b = cue_words.get(i + 1).map_or(end - 1, |n| {
                    // Last frame the word is audible: the frame containing the
                    // millisecond just before the next word starts.
                    frame(n.start_ms.saturating_sub(1)).min(end - 1)
                });
                match mode {
                    KaraokeMode::Progressive => {
                        if let Some(sc) = stayed {
                            filters.extend(word_layer(&mut gen, i, sc, a, end - 1)?);
                        }
                        filters.extend(word_layer(&mut gen, i, hc, a, b)?);
                    }
                    KaraokeMode::Active => {
                        filters.extend(word_layer(&mut gen, i, hc, a, b)?);
                    }
                }
            }
        }
    }
    Ok((filters, gen.files))
}
