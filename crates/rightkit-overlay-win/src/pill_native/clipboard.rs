//! The clip card above the pill (`ClipboardSurface` in `AuxiliaryViews.swift`):
//! `single` = "Clip" (the current transcript, copy/close), `recent` = "mini clip" (the last
//! three transcripts, click a row to copy).

#![allow(dead_code)]

use std::time::Instant;

use super::visuals::{
    aux_entrance, aux_text_button_width, push_aux_card, push_aux_icon, push_aux_text_button,
    text_at_top, text_centered, text_width, tok, transform_ops, wrap_text, Align, DrawOp, Face,
    Fade, Icon, Rgba, VRect, AUX_ICON_SIZE,
};

pub const RECENT_WIDTH: f32 = 320.0;
pub const TRANSCRIPT_MAX_WIDTH: f32 = 460.0;
pub const TRANSCRIPT_MAX_LINES: usize = 9;
const TRANSCRIPT_PADDING: f32 = 10.0;
const RAIL_SPACING: f32 = 5.0;
const SINGLE_LEADING_PAD: f32 = 7.0;
const SINGLE_TRAILING_PAD: f32 = 5.0;
const SHELL_OUTER_PAD: f32 = 1.0;
pub const CARD_RADIUS: f32 = 18.0;
pub const RECENT_LIMIT: usize = 3;
/// Margin around the shell inside the overlay window.
pub const WINDOW_MARGIN: f32 = 3.0;
/// Windows fallback popup: a captured transcript auto-closes after 5 s unless touched.
pub const SINGLE_AUTODISMISS_MS: u64 = 5000;
/// `markCopied`: close 0.26 s after copy (single), reset the "Copied" label after 1.2 s (recent).
pub const CLOSE_AFTER_COPY_MS: u64 = 260;
pub const COPIED_RESET_MS: u64 = 1200;

pub const R_CLOSE: u32 = 1;
pub const R_COPY: u32 = 2;
pub const R_POLISH: u32 = 3;
pub const R_EDIT: u32 = 4;
pub const R_ROW0: u32 = 10;
pub const R_VIEW_MORE: u32 = 20;
pub const R_OPEN_HUB: u32 = 21;
/// Whole-card region: hovering anywhere on the card counts as "the user is using it".
pub const R_CARD: u32 = 23;
pub const REGION_COUNT: usize = 24;

/// In-place editing needs keyboard input, which the overlay API does not carry. The pencil is
/// drawn (parity) and opens the Hub History row instead; flip this when the overlay grows key
/// events and the single clip gets a real text field. `ClipState::begin_edit`, `commit_edit` and
/// `cancel_edit` are the model the driver wires to it (RightKit issue for the overlay side).
pub const INLINE_EDIT_SUPPORTED: bool = false;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipItem {
    pub delivery_id: String,
    pub transcript: String,
    pub raw_transcript: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ClipState {
    pub items: Vec<ClipItem>,
    pub single: bool,
    /// Text the single card currently shows (polished or raw).
    pub draft: String,
    /// `Some(original)` while the single card is being edited in place: `draft` holds the text
    /// being typed and `original` is what Esc restores (Mac `beginSingleClipEditing`).
    pub editing: Option<String>,
    pub copied_id: Option<String>,
    pub shown_ms: f64,
    pub hot: [Fade; REGION_COUNT],
    pub autodismiss_at: Option<Instant>,
    pub copied_reset_at: Option<Instant>,
    pub close_at: Option<Instant>,
}

impl ClipState {
    pub fn new(items: Vec<ClipItem>, single: bool, shown_ms: f64, now: Instant) -> ClipState {
        let draft = if single {
            items
                .first()
                .map(|i| i.transcript.clone())
                .unwrap_or_default()
        } else {
            String::new()
        };
        ClipState {
            items,
            single,
            draft,
            editing: None,
            copied_id: None,
            shown_ms,
            hot: [Fade::at(0.0); REGION_COUNT],
            autodismiss_at: if single {
                Some(now + std::time::Duration::from_millis(SINGLE_AUTODISMISS_MS))
            } else {
                None
            },
            copied_reset_at: None,
            close_at: None,
        }
    }

    pub fn current(&self) -> Option<&ClipItem> {
        if self.single {
            self.items.first()
        } else {
            None
        }
    }

    /// The raw transcript when it differs from the polished one (`AI polish` undo/redo).
    pub fn raw_alternative(&self) -> Option<&str> {
        let item = self.current()?;
        let raw = item.raw_transcript.as_deref()?;
        if raw.trim() != item.transcript.trim() {
            Some(raw)
        } else {
            None
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

    pub fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    /// Pencil click: start editing the single card's text. Pauses the 5 s auto-dismiss, since a
    /// card being typed into must not vanish. `false` when there is nothing to edit.
    pub fn begin_edit(&mut self) -> bool {
        if self.editing.is_some() || self.current().is_none() {
            return false;
        }
        self.editing = Some(self.draft.clone());
        self.autodismiss_at = None;
        true
    }

    /// The text field's current content.
    pub fn set_edit_text(&mut self, text: String) {
        if self.editing.is_some() {
            self.draft = text;
        }
    }

    /// Check button or Enter: leave edit mode. Returns `(delivery_id, text)` for the history
    /// update (`update_history_item`) when the text really changed; an unchanged or blank result
    /// restores the original and returns `None`.
    pub fn commit_edit(&mut self) -> Option<(String, String)> {
        let original = self.editing.take()?;
        let text = self.draft.trim().to_string();
        if text.is_empty() || text == original.trim() {
            self.draft = original;
            return None;
        }
        let item = self.items.first_mut()?;
        item.transcript = text.clone();
        self.draft = text.clone();
        Some((item.delivery_id.clone(), text))
    }

    /// Esc: leave edit mode and put the original text back.
    pub fn cancel_edit(&mut self) {
        if let Some(original) = self.editing.take() {
            self.draft = original;
        }
    }

    /// Toggle the single card between the polished and raw transcript.
    pub fn toggle_polish(&mut self) {
        if self.editing.is_some() {
            return;
        }
        let Some(item) = self.current() else { return };
        let Some(raw) = item.raw_transcript.clone() else {
            return;
        };
        let polished = item.transcript.clone();
        if raw.trim() == polished.trim() {
            return;
        }
        self.draft = if self.draft == raw { polished } else { raw };
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        [self.autodismiss_at, self.copied_reset_at, self.close_at]
            .into_iter()
            .flatten()
            .min()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ClipFrame {
    pub ops: Vec<DrawOp>,
    /// Overlay window size in logical px.
    pub window: (f32, f32),
    /// The Swift view bounds (card + 1 pt shell pad) in window coordinates.
    pub outer: VRect,
    pub regions: Vec<(u32, VRect)>,
}

fn single_transcript_width(draft: &str) -> f32 {
    (draft.chars().count() as f32 * 6.7 + 20.0)
        .max(220.0)
        .min(TRANSCRIPT_MAX_WIDTH)
}

/// Lay out and draw the card for `state`. `hot` is per-region hover; `entrance_ms` the time
/// since it appeared (`f32::MAX` = settled).
pub fn build_clip(state: &ClipState, hot: &[f32; REGION_COUNT], entrance_ms: f32) -> ClipFrame {
    if state.single && state.current().is_some() {
        build_single(state, hot, entrance_ms)
    } else {
        build_recent(state, hot, entrance_ms)
    }
}

fn finish(
    mut ops: Vec<DrawOp>,
    card: VRect,
    outer: VRect,
    regions: Vec<(u32, VRect)>,
    entrance_ms: f32,
) -> ClipFrame {
    let m = WINDOW_MARGIN;
    if entrance_ms.is_finite() && entrance_ms < 1000.0 {
        let (alpha, k) = aux_entrance(entrance_ms);
        ops = transform_ops(&ops, card.cx(), card.cy(), k, alpha, 0.0);
    }
    ClipFrame {
        ops,
        window: (outer.w + 2.0 * m, outer.h + 2.0 * m),
        outer,
        regions,
    }
}

fn build_single(state: &ClipState, hot: &[f32; REGION_COUNT], entrance_ms: f32) -> ClipFrame {
    let item = state.current().expect("single clip has an item");
    let text_w = single_transcript_width(&state.draft);
    let size = 12.5;
    let face = Face::Hanken600;
    let lines = wrap_text(&state.draft, face, size, text_w, TRANSCRIPT_MAX_LINES);
    let lh = face.line_height(size);
    let box_w = text_w + 2.0 * TRANSCRIPT_PADDING;
    let box_h = (lines.len() as f32 * lh + 2.0 * TRANSCRIPT_PADDING).max(38.0);

    // edit · [undo/redo polish] · copy · close
    let mut rail: Vec<(u32, Icon, Rgba)> = vec![(R_EDIT, Icon::Pencil, tok::AUX_ACCENT)];
    if let Some(raw) = state.raw_alternative() {
        let icon = if state.draft == raw {
            Icon::Clockwise
        } else {
            Icon::CounterClockwise
        };
        rail.push((R_POLISH, icon, tok::AUX_ACCENT));
    }
    let copied = state.copied_id.as_deref() == Some(item.delivery_id.as_str());
    rail.push((
        R_COPY,
        if copied { Icon::Check } else { Icon::Copy },
        tok::AUX_ACCENT,
    ));
    rail.push((R_CLOSE, Icon::XMark, tok::ERROR));

    let content_w = box_w + rail.len() as f32 * (RAIL_SPACING + AUX_ICON_SIZE);
    let content_h = box_h.max(AUX_ICON_SIZE);
    let card_w = content_w + SINGLE_LEADING_PAD + SINGLE_TRAILING_PAD;
    let card_h = content_h + 8.0;
    let m = WINDOW_MARGIN;
    let outer = VRect::new(
        m,
        m,
        card_w + 2.0 * SHELL_OUTER_PAD,
        card_h + 2.0 * SHELL_OUTER_PAD,
    );
    let card = VRect::new(
        outer.x + SHELL_OUTER_PAD,
        outer.y + SHELL_OUTER_PAD,
        card_w,
        card_h,
    );

    let mut ops: Vec<DrawOp> = Vec::new();
    let mut regions: Vec<(u32, VRect)> = Vec::new();
    push_aux_card(&mut ops, card, CARD_RADIUS);

    let top = card.y + 4.0;
    let left = card.x + SINGLE_LEADING_PAD;
    let box_rect = VRect::new(left, top, box_w, box_h);
    ops.push(DrawOp::FillRRect {
        rect: box_rect,
        radius: 14.0,
        color: tok::AUX_RAISED,
    });
    for (i, line) in lines.iter().enumerate() {
        ops.push(text_at_top(
            line,
            face,
            size,
            tok::AUX_FOREGROUND,
            box_rect.x + TRANSCRIPT_PADDING,
            box_rect.y + TRANSCRIPT_PADDING + i as f32 * lh,
            text_w + 2.0,
            Align::Left,
        ));
    }

    let mut x = box_rect.right() + RAIL_SPACING;
    for (region, icon, tint) in rail {
        let rect = VRect::new(x, top, AUX_ICON_SIZE, AUX_ICON_SIZE);
        push_aux_icon(
            &mut ops,
            rect.cx(),
            rect.cy(),
            icon,
            tint,
            hot[region as usize],
        );
        regions.push((region, rect));
        x += AUX_ICON_SIZE + RAIL_SPACING;
    }
    finish(ops, card, outer, regions, entrance_ms)
}

fn build_recent(state: &ClipState, hot: &[f32; REGION_COUNT], entrance_ms: f32) -> ClipFrame {
    let rows: Vec<&ClipItem> = state.items.iter().take(RECENT_LIMIT).collect();
    let face = Face::Hanken600;
    let lh = face.line_height(12.0);
    let inner_w = RECENT_WIDTH - 12.0;
    let row_lines: Vec<Vec<String>> = rows
        .iter()
        .map(|item| wrap_text(&item.transcript, face, 12.0, inner_w - 20.0, 2))
        .collect();
    let row_heights: Vec<f32> = row_lines
        .iter()
        .map(|lines| (lines.len() as f32 * lh).max(34.0))
        .collect();
    let rows_h: f32 = row_heights.iter().sum::<f32>() + 4.0 * rows.len().saturating_sub(1) as f32;
    // VStack(spacing 4): rows, then the footer with .padding(.top, 2).
    let footer_h = AUX_ICON_SIZE;
    let card_h = 6.0 + rows_h + 4.0 + 2.0 + footer_h + 6.0;
    let card_w = RECENT_WIDTH;
    let m = WINDOW_MARGIN;
    let outer = VRect::new(
        m,
        m,
        card_w + 2.0 * SHELL_OUTER_PAD,
        card_h + 2.0 * SHELL_OUTER_PAD,
    );
    let card = VRect::new(
        outer.x + SHELL_OUTER_PAD,
        outer.y + SHELL_OUTER_PAD,
        card_w,
        card_h,
    );

    let mut ops: Vec<DrawOp> = Vec::new();
    let mut regions: Vec<(u32, VRect)> = Vec::new();
    push_aux_card(&mut ops, card, CARD_RADIUS);

    let mut y = card.y + 6.0;
    for (i, item) in rows.iter().enumerate() {
        let region = R_ROW0 + i as u32;
        let rect = VRect::new(card.x + 6.0, y, inner_w, row_heights[i]);
        let hot_amount = hot[region as usize];
        let copied = state.copied_id.as_deref() == Some(item.delivery_id.as_str());
        // `hot ? hover : raised`: the hover fill replaces the raised one.
        let base = tok::AUX_RAISED.lerp(Rgba::new(36, 36, 36, 1.0), hot_amount);
        ops.push(DrawOp::FillRRect {
            rect,
            radius: 12.0,
            color: base,
        });
        let text_h = row_lines[i].len() as f32 * lh;
        for (n, line) in row_lines[i].iter().enumerate() {
            ops.push(text_at_top(
                line,
                face,
                12.0,
                tok::AUX_FOREGROUND.with_alpha(0.86),
                rect.x + 10.0,
                rect.cy() - text_h * 0.5 + n as f32 * lh,
                inner_w - 20.0 + 2.0,
                Align::Left,
            ));
        }
        let label = if copied { "Copied" } else { "Click to copy" };
        let show = if copied { 1.0 } else { hot_amount };
        if show > 0.003 {
            let tw = text_width(Face::Hanken600, 9.5, label) + 16.0;
            let pill = VRect::new(rect.right() - 6.0 - tw, rect.cy() - 11.0, tw, 22.0);
            ops.push(DrawOp::FillRRect {
                rect: pill,
                radius: 11.0,
                color: Rgba::new(173, 173, 173, show),
            });
            ops.push(text_centered(
                label,
                Face::Hanken600,
                9.5,
                tok::BLACK.with_alpha(0.82 * show),
                pill.x + 8.0,
                pill.cy(),
                tw - 16.0 + 2.0,
                Align::Left,
            ));
        }
        regions.push((region, rect));
        y += row_heights[i] + 4.0;
    }

    // Footer: caption, then [View more] [Open hub] [close], 8 pt default HStack spacing.
    let footer_cy = y + 2.0 + footer_h * 0.5;
    ops.push(text_centered(
        "Click a transcript to copy",
        Face::Hanken500,
        9.5,
        tok::AUX_MUTED.with_alpha(0.82),
        card.x + 6.0,
        footer_cy,
        170.0,
        Align::Left,
    ));
    let mut right = card.right() - 6.0;
    let close = VRect::new(
        right - AUX_ICON_SIZE,
        footer_cy - AUX_ICON_SIZE * 0.5,
        AUX_ICON_SIZE,
        AUX_ICON_SIZE,
    );
    push_aux_icon(
        &mut ops,
        close.cx(),
        close.cy(),
        Icon::XMark,
        tok::ERROR,
        hot[R_CLOSE as usize],
    );
    regions.push((R_CLOSE, close));
    right = close.x - 8.0;
    for (title, region) in [("Open hub", R_OPEN_HUB), ("View more", R_VIEW_MORE)] {
        let w = aux_text_button_width(title);
        let rect = VRect::new(right - w, footer_cy - 12.0, w, 24.0);
        push_aux_text_button(&mut ops, rect, title, hot[region as usize]);
        regions.push((region, rect));
        right = rect.x - 8.0;
    }
    finish(ops, card, outer, regions, entrance_ms)
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
    use super::build_clip;

    pub struct ClipPainter {
        pub shared: Arc<Shared>,
        pub tx: Sender<Cmd>,
    }

    impl gfx::PainterBody for ClipPainter {
        fn body(&mut self, ctx: &mut gfx::Ctx) {
            let now = Instant::now();
            let snapshot = {
                let visual = self.shared.visual.lock().unwrap_or_else(|e| e.into_inner());
                let now_ms = visual.pill.ms(now);
                visual.clip.as_ref().map(|clip| {
                    (
                        clip.clone(),
                        clip.hot_values(now_ms),
                        (now_ms - clip.shown_ms) as f32,
                        clip.needs_frames(now_ms),
                    )
                })
            };
            let needs = match snapshot {
                Some((clip, hot, entrance, needs)) => {
                    let frame = build_clip(&clip, &hot, entrance);
                    gfx::replay(ctx, &frame.ops);
                    needs
                }
                None => false,
            };
            if !needs && self.shared.pump[Surf::Clip as usize].swap(false, Ordering::SeqCst) {
                let _ = self.tx.send(Cmd::PumpIdle(Surf::Clip));
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

    fn item(id: &str, text: &str, raw: Option<&str>) -> ClipItem {
        ClipItem {
            delivery_id: id.to_string(),
            transcript: text.to_string(),
            raw_transcript: raw.map(str::to_string),
        }
    }

    fn state(items: Vec<ClipItem>, single: bool) -> ClipState {
        ClipState::new(items, single, 0.0, Instant::now())
    }

    fn settled(state: &ClipState) -> ClipFrame {
        build_clip(state, &[0.0; REGION_COUNT], f32::MAX)
    }

    fn texts(frame: &ClipFrame) -> Vec<String> {
        frame
            .ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn recent_card_is_320_wide_with_a_1pt_shell_and_three_rows() {
        let s = state(
            vec![
                item("a", "first", None),
                item("b", "second", None),
                item("c", "third", None),
                item("d", "fourth is ignored", None),
            ],
            false,
        );
        let frame = settled(&s);
        assert!((frame.outer.w - 322.0).abs() < 0.01, "recentWidth + 2");
        let rows: Vec<u32> = frame
            .regions
            .iter()
            .map(|(id, _)| *id)
            .filter(|id| (R_ROW0..R_ROW0 + 3).contains(id))
            .collect();
        assert_eq!(rows, vec![10, 11, 12]);
        // 6 + 3 x 34 + 2 x 4 + 4 + 2 + 26 + 6 = 154.
        assert!(
            (frame.outer.h - (154.0 + 2.0)).abs() < 0.01,
            "got {}",
            frame.outer.h
        );
        let row0 = frame.regions[0].1;
        assert!((row0.h - 34.0).abs() < 0.01 && (row0.w - 308.0).abs() < 0.01);
        assert!(texts(&frame).contains(&"Click a transcript to copy".to_string()));
    }

    #[test]
    fn recent_footer_lists_view_more_open_hub_and_close_right_aligned() {
        let s = state(vec![item("a", "hello", None)], false);
        let frame = settled(&s);
        let find = |id: u32| frame.regions.iter().find(|(r, _)| *r == id).unwrap().1;
        let close = find(R_CLOSE);
        let hub = find(R_OPEN_HUB);
        let more = find(R_VIEW_MORE);
        assert!(more.right() < hub.x && hub.right() < close.x);
        // 8 pt HStack spacing.
        assert!((close.x - hub.right() - 8.0).abs() < 0.01);
        assert!((hub.x - more.right() - 8.0).abs() < 0.01);
        // 6 pt card padding from the trailing edge.
        assert!((frame.outer.right() - 1.0 - close.right() - 6.0).abs() < 0.01);
    }

    #[test]
    fn hovering_a_row_reveals_the_click_to_copy_pill() {
        let s = state(vec![item("a", "hello", None)], false);
        let mut hot = [0.0f32; REGION_COUNT];
        let rest = build_clip(&s, &hot, f32::MAX);
        assert!(!texts(&rest).contains(&"Click to copy".to_string()));
        hot[R_ROW0 as usize] = 1.0;
        let hovered = build_clip(&s, &hot, f32::MAX);
        assert!(texts(&hovered).contains(&"Click to copy".to_string()));
        let mut copied = s.clone();
        copied.copied_id = Some("a".to_string());
        assert!(texts(&settled(&copied)).contains(&"Copied".to_string()));
    }

    #[test]
    fn long_recent_text_clamps_to_two_lines_with_an_ellipsis() {
        let long = "word ".repeat(80);
        let s = state(vec![item("a", &long, None)], false);
        let frame = settled(&s);
        let row_text: Vec<String> = texts(&frame)
            .into_iter()
            .filter(|t| t.starts_with("word"))
            .collect();
        assert_eq!(row_text.len(), 2);
        assert!(row_text[1].ends_with('…'));
    }

    #[test]
    fn single_clip_has_edit_copy_close_and_polish_only_when_raw_differs() {
        let plain = state(vec![item("current", "Hello there.", None)], true);
        let ids: Vec<u32> = settled(&plain).regions.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![R_EDIT, R_COPY, R_CLOSE]);

        let polished = state(
            vec![item("current", "Hello there.", Some("hello there"))],
            true,
        );
        let frame = settled(&polished);
        let ids: Vec<u32> = frame.regions.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![R_EDIT, R_POLISH, R_COPY, R_CLOSE]);
        // Rail icons are 26 pt apart by 5 pt spacing.
        let a = frame.regions[0].1;
        let b = frame.regions[1].1;
        assert!((b.x - a.right() - 5.0).abs() < 0.01);
        assert!((a.w - 26.0).abs() < 0.01);
    }

    #[test]
    fn single_card_padding_matches_swift() {
        let s = state(vec![item("current", "Short.", None)], true);
        let frame = settled(&s);
        // Transcript box is at least 220 + 20 wide and 38 tall.
        let edit = frame.regions[0].1;
        let box_right = edit.x - 5.0;
        let card_left = frame.outer.x + 1.0;
        assert!((box_right - card_left - 7.0 - (220.0 + 20.0)).abs() < 0.01);
        let card_h = frame.outer.h - 2.0;
        assert!((card_h - (38.0 + 8.0)).abs() < 0.01, "got {card_h}");
        // Icons are top-aligned with the box (HStack alignment .top).
        assert!((edit.y - (frame.outer.y + 1.0 + 4.0)).abs() < 0.01);
    }

    #[test]
    fn single_transcript_width_is_clamped_220_to_460() {
        assert_eq!(single_transcript_width("a"), 220.0);
        assert_eq!(single_transcript_width(&"x".repeat(1000)), 460.0);
        assert!((single_transcript_width(&"x".repeat(50)) - (50.0 * 6.7 + 20.0)).abs() < 0.01);
    }

    #[test]
    fn single_transcript_stops_at_nine_lines() {
        let long = "lorem ipsum dolor sit amet ".repeat(120);
        let s = state(vec![item("current", &long, None)], true);
        let frame = settled(&s);
        let lines = texts(&frame).len();
        assert_eq!(lines, 9);
    }

    #[test]
    fn edit_commit_emits_an_update_for_the_edited_text() {
        let mut s = state(vec![item("d1", "helo world", None)], true);
        assert!(s.begin_edit());
        assert!(s.is_editing());
        assert!(
            s.autodismiss_at.is_none(),
            "an edited card never auto-dismisses"
        );
        s.set_edit_text("hello world".to_string());
        assert_eq!(
            s.commit_edit(),
            Some(("d1".to_string(), "hello world".to_string()))
        );
        assert!(!s.is_editing());
        assert_eq!(s.draft, "hello world");
        assert_eq!(s.current().unwrap().transcript, "hello world");
    }

    #[test]
    fn edit_cancel_restores_the_original_text() {
        let mut s = state(vec![item("d1", "helo world", None)], true);
        s.begin_edit();
        s.set_edit_text("something else".to_string());
        s.cancel_edit();
        assert!(!s.is_editing());
        assert_eq!(s.draft, "helo world");
        assert_eq!(s.current().unwrap().transcript, "helo world");
    }

    #[test]
    fn unchanged_or_blank_edit_commits_nothing() {
        let mut s = state(vec![item("d1", "hello", None)], true);
        s.begin_edit();
        assert_eq!(s.commit_edit(), None);
        s.begin_edit();
        s.set_edit_text("   ".to_string());
        assert_eq!(s.commit_edit(), None);
        assert_eq!(s.draft, "hello");
    }

    #[test]
    fn recent_card_cannot_edit_and_polish_is_frozen_while_editing() {
        let mut recent = state(vec![item("a", "x", None)], false);
        assert!(!recent.begin_edit());
        let mut s = state(vec![item("d1", "polished", Some("raw text"))], true);
        s.begin_edit();
        s.toggle_polish();
        assert_eq!(s.draft, "polished");
        assert!(!s.begin_edit(), "already editing");
    }

    #[test]
    fn polish_toggle_swaps_between_raw_and_polished() {
        let mut s = state(
            vec![item("current", "Hello there.", Some("hello there"))],
            true,
        );
        assert_eq!(s.draft, "Hello there.");
        s.toggle_polish();
        assert_eq!(s.draft, "hello there");
        s.toggle_polish();
        assert_eq!(s.draft, "Hello there.");
        let mut none = state(vec![item("current", "Same.", Some("Same."))], true);
        none.toggle_polish();
        assert_eq!(none.draft, "Same.");
    }

    #[test]
    fn single_autodismiss_arms_only_for_the_single_card() {
        let single = state(vec![item("current", "x", None)], true);
        assert!(single.autodismiss_at.is_some());
        let recent = state(vec![item("a", "x", None)], false);
        assert!(recent.autodismiss_at.is_none());
        assert_eq!(recent.next_deadline(), None);
    }

    #[test]
    fn copied_icon_swaps_to_a_checkmark() {
        let mut s = state(vec![item("current", "Hello.", None)], true);
        let before = settled(&s).ops.len();
        s.copied_id = Some("current".to_string());
        let after = settled(&s).ops.len();
        assert_ne!(
            before, after,
            "copy glyph and checkmark are different outlines"
        );
    }
}
