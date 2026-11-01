//! Title rendering: typed templates -> FFmpeg `drawtext` filters.
//!
//! # Text transport: `textfile=` with `expansion=none`
//!
//! Title text never appears in the filtergraph. The builder wraps the text itself
//! and hands the caller exact file contents ([`TextFile`]) to write into
//! [`TitleAssets::text_dir`]; each `drawtext` reads its text through `textfile=`
//! with `expansion=none`. That removes three escaping layers (filtergraph,
//! option, text expansion) for arbitrary UTF-8 (`% \ : ' , ; [ ]`, emoji, newlines)
//! and keeps the argument vector independent of user text. Only filesystem paths
//! remain in the graph; they go through [`escape_filter_path`].
//!
//! File names are `title-<blake3 of content, 16 hex>.txt`: identical text maps to
//! one identical file, so retries are idempotent.
//!
//! # Layout
//!
//! Every dimension is an integer fraction (permille) of the output width or
//! height, so layout is exact integer arithmetic and identical on every host.
//! Titles are set in a monospace font (the engine passes JetBrains Mono), so the
//! builder knows the advance width ([`ADVANCE_PERMILLE`] of the font size) and
//! wraps text to the preset's maximum width itself.

use std::path::{Path, PathBuf};

use crate::error::TimelineError;
use crate::ffmpeg::Preset;
use crate::model::TitleTemplate;
use crate::plan::OverlayItem;

/// Advance width of the title font (JetBrains Mono: 600/1000 em).
pub const ADVANCE_PERMILLE: i64 = 600;
/// Line height of the title font (JetBrains Mono: 1320/1000 em), used only to
/// stack the two lower-third boxes; everything else uses `text_h`.
const LINE_HEIGHT_PERMILLE: i64 = 1320;

/// Where the caller supplies the font and a directory for the title text files.
#[derive(Debug, Clone, Copy)]
pub struct TitleAssets<'a> {
    /// TrueType font used for every title (monospace; the engine passes
    /// `<resource_dir>/runtime/fonts/JetBrainsMono-Regular.ttf`).
    pub font: &'a Path,
    /// Directory the caller writes [`TextFile`]s into before running FFmpeg.
    pub text_dir: &'a Path,
}

/// A text file the render needs: write `content` (UTF-8, exact bytes) to `path`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextFile {
    pub path: PathBuf,
    pub content: String,
}

/// Fixed per-template, per-preset layout (permille of output width/height).
struct Style {
    /// Font size, permille of output height.
    font: i64,
    /// Widest the box may be, permille of output width.
    max_width: i64,
    /// Left margin of the box (left-aligned styles), permille of width.
    left: i64,
    /// Vertical anchor, permille of height: the box bottom (`Bottom`) or centre (`Middle`).
    anchor: i64,
    /// Box opacity in percent (0 = no box, text gets a shadow instead).
    box_percent: i64,
}

#[derive(Clone, Copy)]
enum Place {
    Bottom,
    Middle,
}

impl Style {
    fn font_px(&self, h: i64) -> i64 {
        (h * self.font + 500) / 1000
    }

    /// Box padding around the text: 40% of the font size.
    fn pad(&self, h: i64) -> i64 {
        self.font_px(h) * 40 / 100
    }
}

struct Layout {
    // Lower-third title, subtitle (smaller), then the two single-block styles.
    title: Style,
    subtitle: Style,
    centered: Style,
    caption: Style,
}

fn layout(preset: Preset) -> Layout {
    match preset {
        // 1920x1080: action/title safe margins of 5% and more.
        Preset::YouTube => Layout {
            title: Style {
                font: 52,
                max_width: 620,
                left: 60,
                anchor: 900,
                box_percent: 60,
            },
            subtitle: Style {
                font: 34,
                max_width: 620,
                left: 60,
                anchor: 900,
                box_percent: 45,
            },
            centered: Style {
                font: 70,
                max_width: 800,
                left: 0,
                anchor: 500,
                box_percent: 40,
            },
            caption: Style {
                font: 40,
                max_width: 800,
                left: 0,
                anchor: 900,
                box_percent: 60,
            },
        },
        // 1080x1920: keeps clear of the bottom ~22% (caption, buttons) and the
        // top ~12% that Reels and Shorts overlay with their own UI.
        Preset::Reels => Layout {
            title: Style {
                font: 30,
                max_width: 780,
                left: 70,
                anchor: 760,
                box_percent: 60,
            },
            subtitle: Style {
                font: 21,
                max_width: 780,
                left: 70,
                anchor: 760,
                box_percent: 45,
            },
            centered: Style {
                font: 40,
                max_width: 800,
                left: 0,
                anchor: 450,
                box_percent: 40,
            },
            caption: Style {
                font: 26,
                max_width: 800,
                left: 0,
                anchor: 740,
                box_percent: 60,
            },
        },
        // 1080x1920 TikTok: bottom ~25% (handle, description, sound) and a right
        // action rail (x > 83%) are covered; left-aligned blocks end by 76% of the
        // width and centred blocks by 83%.
        Preset::TikTok => Layout {
            title: Style {
                font: 30,
                max_width: 700,
                left: 60,
                anchor: 720,
                box_percent: 60,
            },
            subtitle: Style {
                font: 21,
                max_width: 700,
                left: 60,
                anchor: 720,
                box_percent: 45,
            },
            centered: Style {
                font: 40,
                max_width: 660,
                left: 0,
                anchor: 440,
                box_percent: 40,
            },
            caption: Style {
                font: 26,
                max_width: 660,
                left: 0,
                anchor: 710,
                box_percent: 60,
            },
        },
        // 1080x1080 square (R1): caption box bottom at 88% of the frame,
        // 80% of the width; centred blocks sit mid-frame like Reels.
        Preset::Square => Layout {
            title: Style {
                font: 30,
                max_width: 780,
                left: 70,
                anchor: 760,
                box_percent: 60,
            },
            subtitle: Style {
                font: 21,
                max_width: 780,
                left: 70,
                anchor: 760,
                box_percent: 45,
            },
            centered: Style {
                font: 40,
                max_width: 800,
                left: 0,
                anchor: 450,
                box_percent: 40,
            },
            caption: Style {
                font: 26,
                max_width: 864,
                left: 0,
                anchor: 880,
                box_percent: 80,
            },
        },
    }
}

/// Greedy word wrap to `max_chars` characters per line. Explicit newlines are kept;
/// words longer than a line are split.
pub fn wrap_text(text: &str, max_chars: usize) -> String {
    let max_chars = max_chars.max(1);
    let mut out: Vec<String> = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        let mut len = 0usize;
        for word in para.split_whitespace() {
            let mut word: Vec<char> = word.chars().collect();
            loop {
                let sep = usize::from(len > 0);
                if len + sep + word.len() <= max_chars {
                    if sep == 1 {
                        line.push(' ');
                    }
                    line.extend(word.iter());
                    len += sep + word.len();
                    break;
                }
                if len > 0 {
                    out.push(std::mem::take(&mut line));
                    len = 0;
                    continue;
                }
                let rest = word.split_off(max_chars);
                out.push(word.iter().collect());
                word = rest;
            }
        }
        out.push(line);
    }
    out.join("\n")
}

/// Escapes a filesystem path for use as a `drawtext` option value inside a
/// filtergraph: two levels (option, then filtergraph), no shell involved.
pub fn escape_filter_path(path: &Path) -> Result<String, TimelineError> {
    let s = path
        .to_str()
        .ok_or_else(|| TimelineError::Unsupported("title asset path is not valid UTF-8".into()))?;
    // Windows accepts forward slashes everywhere; it avoids backslash escaping.
    let s = if cfg!(windows) {
        s.replace('\\', "/")
    } else {
        s.to_string()
    };
    let mut level1 = String::new();
    for c in s.chars() {
        if matches!(c, '\\' | '\'' | ':') {
            level1.push('\\');
        }
        level1.push(c);
    }
    let mut level2 = String::new();
    for c in level1.chars() {
        if matches!(c, '\\' | '\'' | ',' | ';' | '[' | ']' | ' ') {
            level2.push('\\');
        }
        level2.push(c);
    }
    Ok(level2)
}

struct Block<'a> {
    text: &'a str,
    style: &'a Style,
    place: Place,
    /// Left-aligned at `style.left`, else centred.
    left_aligned: bool,
    /// Extra pixels above the anchor (stacks the lower-third title over the subtitle).
    lift: i64,
}

struct Ctx<'a> {
    w: i64,
    h: i64,
    item: &'a OverlayItem,
    assets: &'a TitleAssets<'a>,
}

impl Ctx<'_> {
    /// Wrapped text for a block: the largest line count fits the style's width.
    fn wrap(&self, text: &str, style: &Style) -> String {
        let font_px = style.font_px(self.h).max(1);
        let avail = self.w * style.max_width / 1000 - 2 * style.pad(self.h);
        let max_chars = avail * 1000 / (font_px * ADVANCE_PERMILLE);
        wrap_text(text, max_chars.max(1) as usize)
    }

    fn alpha(&self) -> Option<String> {
        let s = self.item.start_frame;
        let e = s + self.item.frames - 1;
        let (fi, fo) = (self.item.fade_in_frames, self.item.fade_out_frames);
        let mut parts = Vec::new();
        if fi > 0 {
            parts.push(format!("(n-{s}+1)/{}", fi + 1));
        }
        if fo > 0 {
            parts.push(format!("({e}-n+1)/{}", fo + 1));
        }
        match parts.len() {
            0 => None,
            1 => Some(format!("min(1,{})", parts[0])),
            _ => Some(format!("min(1,min({},{}))", parts[0], parts[1])),
        }
    }

    fn filter(&self, b: &Block<'_>, files: &mut Vec<TextFile>) -> Result<String, TimelineError> {
        let content = self.wrap(b.text, b.style);
        let hash = rightkit_fs::digest::blake3_bytes(content.as_bytes());
        let path = self
            .assets
            .text_dir
            .join(format!("title-{}.txt", &hash[..16]));
        let (font_px, pad) = (b.style.font_px(self.h), b.style.pad(self.h));
        let x = if let Some(p) = self.item.position {
            format!("max(0,min(w-text_w,{}-text_w/2))", self.w * p.x / 1000)
        } else if b.left_aligned {
            format!("{}", self.w * b.style.left / 1000 + pad)
        } else {
            "(w-text_w)/2".to_string()
        };
        let y = if let Some(p) = self.item.position {
            format!(
                "max(0,min(h-text_h,{}-text_h/2-{}))",
                self.h * p.y / 1000,
                b.lift
            )
        } else {
            match b.place {
                Place::Bottom => {
                    format!("{}-text_h", self.h * b.style.anchor / 1000 - pad - b.lift)
                }
                Place::Middle => format!("{}-text_h/2", self.h * b.style.anchor / 1000),
            }
        };
        let mut f = format!(
            "drawtext=fontfile={}:textfile={}:expansion=none:fontsize={font_px}:fontcolor=white:text_align={}",
            escape_filter_path(self.assets.font)?,
            escape_filter_path(&path)?,
            if b.left_aligned { "left" } else { "center" },
        );
        if b.style.box_percent > 0 {
            f.push_str(&format!(
                ":box=1:boxcolor=black@0.{:02}:boxborderw={pad}",
                b.style.box_percent
            ));
        } else {
            let d = (font_px / 24).max(1);
            f.push_str(&format!(":shadowcolor=black@0.6:shadowx={d}:shadowy={d}"));
        }
        // Position clamps contain commas, which must stay inside drawtext's
        // expression instead of becoming separators in the filter graph.
        f.push_str(&format!(":x='{x}':y='{y}'"));
        if let Some(a) = self.alpha() {
            f.push_str(&format!(":alpha='{a}'"));
        }
        f.push_str(&format!(
            ":enable='between(n,{},{})'",
            self.item.start_frame,
            self.item.start_frame + self.item.frames - 1
        ));
        if !files.iter().any(|t| t.path == path) {
            files.push(TextFile { path, content });
        }
        Ok(f)
    }
}

/// `drawtext` filters (draw order) and the text files they read for one title.
/// Empty when the title starts at or after `total_frames`.
pub fn title_filters(
    item: &OverlayItem,
    preset: Preset,
    total_frames: i64,
    assets: &TitleAssets<'_>,
) -> Result<(Vec<String>, Vec<TextFile>), TimelineError> {
    if item.start_frame >= total_frames {
        return Ok((Vec::new(), Vec::new()));
    }
    let (w, h) = (preset.width() as i64, preset.height() as i64);
    let lay = layout(preset);
    let cx = Ctx { w, h, item, assets };
    let mut files = Vec::new();
    let mut filters = Vec::new();
    match &item.template {
        TitleTemplate::LowerThird { title, subtitle } => {
            let mut lift = 0;
            if let Some(sub) = subtitle {
                let lines = cx.wrap(sub, &lay.subtitle).split('\n').count() as i64;
                // Subtitle box height (estimate) + a small gap above it.
                let lh = lay.subtitle.font_px(h) * LINE_HEIGHT_PERMILLE / 1000;
                lift = lines * lh + 2 * lay.subtitle.pad(h) + h / 200;
                filters.push(cx.filter(
                    &Block {
                        text: sub,
                        style: &lay.subtitle,
                        place: Place::Bottom,
                        left_aligned: true,
                        lift: 0,
                    },
                    &mut files,
                )?);
            }
            // Title is pushed first in draw order so it never hides the subtitle box.
            filters.insert(
                0,
                cx.filter(
                    &Block {
                        text: title,
                        style: &lay.title,
                        place: Place::Bottom,
                        left_aligned: true,
                        lift,
                    },
                    &mut files,
                )?,
            );
        }
        TitleTemplate::Centered { text } => filters.push(cx.filter(
            &Block {
                text,
                style: &lay.centered,
                place: Place::Middle,
                left_aligned: false,
                lift: 0,
            },
            &mut files,
        )?),
        TitleTemplate::Caption { text } => filters.push(cx.filter(
            &Block {
                text,
                style: &lay.caption,
                place: Place::Bottom,
                left_aligned: false,
                lift: 0,
            },
            &mut files,
        )?),
    }
    Ok((filters, files))
}
