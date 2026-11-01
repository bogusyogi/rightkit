//! The hint card above the pill (`HintSurface` in `AuxiliaryViews.swift`): prompt tip,
//! screenshot-destination choice, and the catalog feature tips.
//!
//! Layout is pure; the Windows painter at the bottom replays it onto an overlay.

#![allow(dead_code)]

use std::time::Instant;

use super::visuals::{
    self, aux_entrance, aux_text_button_width, push_aux_card, push_aux_icon, push_aux_text_button,
    text_centered, text_width, tok, transform_ops, wrap_text, Align, DrawOp, Face, Fade, Icon,
    VRect, AUX_ICON_SIZE,
};

/// `HintSurface.cornerRadius`.
pub const CORNER_RADIUS: f32 = 16.0;
/// `HintSurface.featureTextMaxWidth`.
pub const FEATURE_TEXT_MAX_WIDTH: f32 = 355.0;
/// Transparent margin around the card inside the overlay window (stroke antialiasing and the
/// 0.965 -> 1 entrance scale).
pub const WINDOW_MARGIN: f32 = 3.0;
/// React `HintPopup`: prompt tip hides itself after 6.2 s, feature tips after 8 s. The screenshot
/// choice stays until answered.
pub const PROMPT_HIDE_MS: u64 = 6200;
pub const FEATURE_HIDE_MS: u64 = 8000;

pub const R_CLOSE: u32 = 1;
pub const R_CLIPBOARD: u32 = 2;
pub const R_DISK: u32 = 3;
/// Whole-card region so clicks on the card text never fall through to the app below.
pub const R_CARD: u32 = 4;
pub const REGION_COUNT: usize = 5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HintContent {
    /// "Say prompt at the end to shape your dictation for AI".
    Prompt,
    /// "Save screenshot to \[Clipboard\] \[Disk\]".
    Screenshot,
    /// Catalog tip (`title` bold, `body` muted, at most two lines).
    Feature { title: String, body: String },
}

impl HintContent {
    /// Auto-hide delay; `None` for the interactive screenshot choice.
    pub fn hide_after_ms(&self) -> Option<u64> {
        match self {
            HintContent::Prompt => Some(PROMPT_HIDE_MS),
            HintContent::Screenshot => None,
            HintContent::Feature { .. } => Some(FEATURE_HIDE_MS),
        }
    }
}

#[derive(Clone, Debug)]
pub struct HintState {
    pub content: HintContent,
    pub kind: String,
    pub shown_ms: f64,
    pub hot: [Fade; REGION_COUNT],
    pub hide_at: Option<Instant>,
}

impl HintState {
    pub fn new(content: HintContent, kind: &str, shown_ms: f64, hide_at: Option<Instant>) -> Self {
        HintState {
            content,
            kind: kind.to_string(),
            shown_ms,
            hot: [Fade::at(0.0); REGION_COUNT],
            hide_at,
        }
    }

    pub fn set_hot(&mut self, region: u32, hot: bool, now_ms: f64) {
        if let Some(fade) = self.hot.get_mut(region as usize) {
            fade.retarget(if hot { 1.0 } else { 0.0 }, now_ms, 100.0);
        }
    }

    pub fn hot_values(&self, now_ms: f64) -> [f32; REGION_COUNT] {
        let mut out = [0.0f32; REGION_COUNT];
        for (slot, fade) in out.iter_mut().zip(self.hot.iter()) {
            *slot = fade.value(now_ms);
        }
        out
    }

    pub fn needs_frames(&self, now_ms: f64) -> bool {
        now_ms - self.shown_ms < 400.0 || self.hot.iter().any(|h| !h.done(now_ms))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct HintFrame {
    pub ops: Vec<DrawOp>,
    /// Overlay window size in logical px.
    pub window: (f32, f32),
    /// The card rectangle in window coordinates (the Swift view bounds).
    pub card: VRect,
    pub regions: Vec<(u32, VRect)>,
}

/// Whether the feature text needs a pinned, wrapped width (`featureTextNeedsWrap`).
pub fn feature_needs_wrap(title: &str, body: &str) -> bool {
    let t = text_width(Face::Hanken700, 12.0, title).ceil();
    let b = text_width(Face::Hanken600, 12.0, body).ceil();
    t.max(b) > FEATURE_TEXT_MAX_WIDTH
}

enum Item {
    Text {
        text: String,
        face: Face,
        size: f32,
        color: visuals::Rgba,
        w: f32,
    },
    Keyword {
        text: String,
        w: f32,
    },
    Button {
        title: &'static str,
        region: u32,
        w: f32,
    },
    Close,
    Feature {
        title: String,
        lines: Vec<String>,
        w: f32,
        h: f32,
    },
}

impl Item {
    fn width(&self) -> f32 {
        match self {
            Item::Text { w, .. } | Item::Keyword { w, .. } | Item::Button { w, .. } => *w,
            Item::Close => AUX_ICON_SIZE,
            Item::Feature { w, .. } => *w,
        }
    }

    fn height(&self) -> f32 {
        match self {
            Item::Text { face, size, .. } => face.line_height(*size),
            Item::Keyword { .. } => 25.0,
            Item::Button { .. } => 24.0,
            Item::Close => AUX_ICON_SIZE,
            Item::Feature { h, .. } => *h,
        }
    }
}

fn items_for(content: &HintContent) -> Vec<Item> {
    let bold12 = |text: &str| Item::Text {
        text: text.to_string(),
        face: Face::Hanken700,
        size: 12.0,
        color: tok::AUX_FOREGROUND,
        w: text_width(Face::Hanken700, 12.0, text),
    };
    match content {
        HintContent::Prompt => {
            let copy = "at the end to shape your dictation for AI";
            vec![
                bold12("Say"),
                Item::Keyword {
                    text: "prompt".to_string(),
                    w: text_width(Face::Hanken700, 11.5, "prompt") + 20.0,
                },
                Item::Text {
                    text: copy.to_string(),
                    face: Face::Hanken600,
                    size: 12.0,
                    color: tok::AUX_MUTED,
                    w: text_width(Face::Hanken600, 12.0, copy),
                },
                Item::Close,
            ]
        }
        HintContent::Screenshot => vec![
            bold12("Save screenshot to"),
            Item::Button {
                title: "Clipboard",
                region: R_CLIPBOARD,
                w: aux_text_button_width("Clipboard"),
            },
            Item::Button {
                title: "Disk",
                region: R_DISK,
                w: aux_text_button_width("Disk"),
            },
            Item::Close,
        ],
        HintContent::Feature { title, body } => {
            let wrap = feature_needs_wrap(title, body);
            let lines = if wrap {
                wrap_text(body, Face::Hanken600, 12.0, FEATURE_TEXT_MAX_WIDTH, 2)
            } else {
                vec![body.clone()]
            };
            let widest = lines
                .iter()
                .map(|l| text_width(Face::Hanken600, 12.0, l))
                .fold(text_width(Face::Hanken700, 12.0, title), f32::max);
            let w = if wrap {
                FEATURE_TEXT_MAX_WIDTH
            } else {
                widest.ceil()
            };
            let lh = Face::Hanken600.line_height(12.0);
            let h = lh + 2.0 + lh * lines.len() as f32;
            vec![
                Item::Feature {
                    title: title.clone(),
                    lines,
                    w,
                    h,
                },
                Item::Close,
            ]
        }
    }
}

/// Lay out and draw the hint card. `hot` is the per-region hover amount; `entrance_ms` the time
/// since the card appeared (`f32::MAX` for fully settled).
pub fn build_hint(content: &HintContent, hot: &[f32; REGION_COUNT], entrance_ms: f32) -> HintFrame {
    let items = items_for(content);
    let spacing = 6.0;
    let content_w: f32 = items.iter().map(Item::width).sum::<f32>()
        + spacing * (items.len().saturating_sub(1)) as f32;
    let row_h = items.iter().map(Item::height).fold(0.0f32, f32::max);
    // padding: leading 12, trailing 5, vertical 4; frame(minHeight: 34).
    let card_w = 12.0 + content_w + 5.0;
    let card_h = (row_h + 8.0).max(34.0);
    let m = WINDOW_MARGIN;
    let card = VRect::new(m, m, card_w, card_h);
    let window = (card_w + 2.0 * m, card_h + 2.0 * m);
    let cy = card.cy();

    let mut ops: Vec<DrawOp> = Vec::new();
    let mut regions: Vec<(u32, VRect)> = Vec::new();
    push_aux_card(&mut ops, card, CORNER_RADIUS);

    let mut x = card.x + 12.0;
    for item in &items {
        match item {
            Item::Text {
                text,
                face,
                size,
                color,
                w,
            } => {
                ops.push(text_centered(
                    text,
                    *face,
                    *size,
                    *color,
                    x,
                    cy,
                    *w + 2.0,
                    Align::Left,
                ));
            }
            Item::Keyword { text, w } => {
                let rect = VRect::new(x, cy - 12.5, *w, 25.0);
                ops.push(DrawOp::FillRRect {
                    rect,
                    radius: 12.5,
                    color: tok::AUX_ACCENT.with_alpha(0.16),
                });
                ops.push(text_centered(
                    text,
                    Face::Hanken700,
                    11.5,
                    tok::AUX_FOREGROUND,
                    x + 10.0,
                    cy,
                    *w - 20.0 + 2.0,
                    Align::Left,
                ));
            }
            Item::Button { title, region, w } => {
                let rect = VRect::new(x, cy - 12.0, *w, 24.0);
                push_aux_text_button(
                    &mut ops,
                    rect,
                    title,
                    hot.get(*region as usize).copied().unwrap_or(0.0),
                );
                regions.push((*region, rect));
            }
            Item::Close => {
                let rect = VRect::new(x, cy - AUX_ICON_SIZE * 0.5, AUX_ICON_SIZE, AUX_ICON_SIZE);
                push_aux_icon(
                    &mut ops,
                    rect.cx(),
                    rect.cy(),
                    Icon::XMark,
                    tok::ERROR,
                    hot[R_CLOSE as usize],
                );
                regions.push((R_CLOSE, rect));
            }
            Item::Feature {
                title, lines, h, ..
            } => {
                let lh = Face::Hanken600.line_height(12.0);
                let top = cy - h * 0.5;
                ops.push(visuals::text_at_top(
                    title,
                    Face::Hanken700,
                    12.0,
                    tok::AUX_FOREGROUND,
                    x,
                    top,
                    item.width() + 2.0,
                    Align::Left,
                ));
                for (i, line) in lines.iter().enumerate() {
                    ops.push(visuals::text_at_top(
                        line,
                        Face::Hanken600,
                        12.0,
                        tok::AUX_MUTED,
                        x,
                        top + lh + 2.0 + i as f32 * lh,
                        item.width() + 2.0,
                        Align::Left,
                    ));
                }
            }
        }
        x += item.width() + spacing;
    }

    if entrance_ms.is_finite() && entrance_ms < 1000.0 {
        let (alpha, k) = aux_entrance(entrance_ms);
        ops = transform_ops(&ops, card.cx(), card.cy(), k, alpha, 0.0);
    }
    HintFrame {
        ops,
        window,
        card,
        regions,
    }
}

// ---------------------------------------------------------------------------------------------
// Painter (Windows only)
// ---------------------------------------------------------------------------------------------

#[cfg(target_os = "windows")]
pub(super) mod painter {
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::Sender;
    use std::sync::Arc;
    use std::time::Instant;

    use super::super::{gfx, Cmd, Shared, Surf};
    use super::{build_hint, REGION_COUNT};

    pub struct HintPainter {
        pub shared: Arc<Shared>,
        pub tx: Sender<Cmd>,
    }

    impl gfx::PainterBody for HintPainter {
        fn body(&mut self, ctx: &mut gfx::Ctx) {
            let now = Instant::now();
            let snapshot = {
                let visual = self.shared.visual.lock().unwrap_or_else(|e| e.into_inner());
                let now_ms = visual.pill.ms(now);
                visual.hint.as_ref().map(|hint| {
                    let hot: [f32; REGION_COUNT] = hint.hot_values(now_ms);
                    (
                        hint.content.clone(),
                        hot,
                        (now_ms - hint.shown_ms) as f32,
                        hint.needs_frames(now_ms),
                    )
                })
            };
            let needs = match snapshot {
                Some((content, hot, entrance, needs)) => {
                    let frame = build_hint(&content, &hot, entrance);
                    gfx::replay(ctx, &frame.ops);
                    needs
                }
                None => false,
            };
            if !needs && self.shared.pump[Surf::Hint as usize].swap(false, Ordering::SeqCst) {
                let _ = self.tx.send(Cmd::PumpIdle(Surf::Hint));
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn settled(content: &HintContent) -> HintFrame {
        build_hint(content, &[0.0; REGION_COUNT], f32::MAX)
    }

    #[test]
    fn card_has_swift_padding_and_a_34pt_floor() {
        let frame = settled(&HintContent::Prompt);
        assert!(frame.card.h >= 34.0);
        // 26 pt close icon + 4 pt padding top and bottom = 34.
        assert!((frame.card.h - 34.0).abs() < 0.01);
        assert_eq!(frame.window.0, frame.card.w + 2.0 * WINDOW_MARGIN);
        let close = frame
            .regions
            .iter()
            .find(|(id, _)| *id == R_CLOSE)
            .expect("close region")
            .1;
        // trailing padding 5 pt after the 26 pt close icon.
        assert!((frame.card.right() - close.right() - 5.0).abs() < 0.01);
        assert!((close.w - 26.0).abs() < 0.01);
    }

    #[test]
    fn screenshot_hint_has_two_buttons_and_a_close() {
        let frame = settled(&HintContent::Screenshot);
        let ids: Vec<u32> = frame.regions.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![R_CLIPBOARD, R_DISK, R_CLOSE]);
        let clip = frame.regions[0].1;
        let disk = frame.regions[1].1;
        assert!(
            (disk.x - clip.right() - 6.0).abs() < 0.01,
            "HStack spacing 6"
        );
        assert!((clip.h - 24.0).abs() < 0.01);
        let texts: Vec<&str> = frame
            .ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["Save screenshot to", "Clipboard", "Disk"]);
    }

    #[test]
    fn prompt_hint_draws_say_keyword_and_muted_copy() {
        let frame = settled(&HintContent::Prompt);
        let texts: Vec<(String, f32)> = frame
            .ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::Text { text, size, .. } => Some((text.clone(), *size)),
                _ => None,
            })
            .collect();
        assert_eq!(texts[0], ("Say".to_string(), 12.0));
        assert_eq!(texts[1], ("prompt".to_string(), 11.5));
        assert_eq!(texts[2].0, "at the end to shape your dictation for AI");
    }

    #[test]
    fn feature_hint_wraps_at_355_when_the_body_is_long() {
        let short = HintContent::Feature {
            title: "Snippets".to_string(),
            body: "Short body.".to_string(),
        };
        assert!(!feature_needs_wrap("Snippets", "Short body."));
        let short_frame = settled(&short);
        let long_body = "Use AI Polish, then undo or redo from the transcript pill, and keep going until the sentence is long enough to need a second line to fit.";
        assert!(feature_needs_wrap("Polish and undo", long_body));
        let long = HintContent::Feature {
            title: "Polish and undo".to_string(),
            body: long_body.to_string(),
        };
        let long_frame = settled(&long);
        assert!(
            long_frame.card.h > short_frame.card.h,
            "two lines are taller"
        );
        // 12 leading + 355 text + 6 spacing + 26 icon + 5 trailing.
        assert!((long_frame.card.w - (12.0 + 355.0 + 6.0 + 26.0 + 5.0)).abs() < 0.01);
    }

    #[test]
    fn catalog_hint_text_reaches_the_card() {
        let tip = visuals::feature_hint("voice_commands").expect("catalog entry");
        let frame = settled(&HintContent::Feature {
            title: tip.title.clone(),
            body: tip.body.clone(),
        });
        assert!(frame
            .ops
            .iter()
            .any(|op| matches!(op, DrawOp::Text { text, .. } if *text == tip.title)));
    }

    #[test]
    fn entrance_fades_and_scales_in_then_settles() {
        let start = build_hint(&HintContent::Prompt, &[0.0; REGION_COUNT], 0.0);
        let first_fill = start
            .ops
            .iter()
            .find_map(|op| match op {
                DrawOp::FillRRect { color, .. } => Some(color.a),
                _ => None,
            })
            .unwrap();
        assert!(first_fill < 0.01, "starts transparent");
        let done = build_hint(&HintContent::Prompt, &[0.0; REGION_COUNT], 400.0);
        let done_fill = done
            .ops
            .iter()
            .find_map(|op| match op {
                DrawOp::FillRRect { color, .. } => Some(color.a),
                _ => None,
            })
            .unwrap();
        assert!(done_fill > 0.99);
    }

    #[test]
    fn hide_timers_follow_the_react_container() {
        assert_eq!(HintContent::Prompt.hide_after_ms(), Some(6200));
        assert_eq!(HintContent::Screenshot.hide_after_ms(), None);
        assert_eq!(
            HintContent::Feature {
                title: String::new(),
                body: String::new()
            }
            .hide_after_ms(),
            Some(8000)
        );
    }
}
