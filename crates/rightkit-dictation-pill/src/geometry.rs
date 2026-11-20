//! The geometry of the pill, hint and clip painters: rectangles, row and column positions, text
//! measurement and wrapping, the capsule's content layout in each mode, and the pose a card is
//! drawn in. A painter calls these for where things go, then draws them with its own colours,
//! glyphs, words and radii.
//!
//! Nothing here draws a product: every size, spacing, padding, font size and face is an input in
//! the spec a function takes, and the functions return rectangles, positions, measured text and
//! display-list ops built only from colours the caller passes. Moved from HeardRight
//! `pill_native_win/painters.rs`; the arithmetic is unchanged.

use crate::anim::AuxPose;
use crate::draw::{fade_ops, transform_ops, DrawOp, Rgba, VRect};
use crate::text::{ellipsize, text_width, wrap_text, Face};

// ---------------------------------------------------------------------------------------------
// Intro glow
// ---------------------------------------------------------------------------------------------

/// Abramowitz & Stegun 7.1.26.
fn erf(x: f32) -> f32 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let y = 1.0
        - (((((1.061_405_4 * t - 1.453_152_1) * t) + 1.421_413_7) * t - 0.284_496_72) * t
            + 0.254_829_6)
            * t
            * (-x * x).exp();
    sign * y
}

fn norm_cdf(x: f32) -> f32 {
    0.5 * (1.0 + erf(x / std::f32::consts::SQRT_2))
}

/// The host's glow: a Gaussian-blurred stroke band laid as concentric strokes inside a capsule.
/// Every field is the blurred stroke's own value at `bloom = 0` plus its gain at `bloom = 1`.
#[derive(Clone, Copy, Debug)]
pub struct GlowSpec {
    /// Blur sigma: `sigma_base + sigma_gain * bloom`.
    pub sigma_base: f32,
    pub sigma_gain: f32,
    /// Stroke band width: `band_base + band_gain * bloom` (the band's half is used).
    pub band_base: f32,
    pub band_gain: f32,
    /// Peak alpha before the opacity: `(alpha_base + alpha_gain * bloom) * bloom`.
    pub alpha_base: f32,
    pub alpha_gain: f32,
    /// No glow at or below this bloom.
    pub min_bloom: f32,
    /// A ring fainter than this is not emitted.
    pub min_alpha: f32,
    /// The glow reaches `reach_sigmas * sigma + band / 2` inward, at most half the capsule height.
    pub reach_sigmas: f32,
    /// Distance between rings, and the width of each ring (at least [`MIN_GLOW_STEP`]).
    pub step: f32,
}

/// The smallest ring step [`gaussian_glow_ops`] uses; a smaller or non-finite `GlowSpec::step`
/// reads as this.
pub const MIN_GLOW_STEP: f32 = 0.25;
/// The most rings [`gaussian_glow_ops`] emits, whatever the spec says.
pub const MAX_GLOW_RINGS: usize = 256;

/// The intro bloom overlay inside `cap`: concentric `StrokeRRect` rings of `color` whose alpha
/// follows the blurred band's profile. A capsule with a non-finite or non-positive size, a
/// non-finite bloom or spec value, or a sigma that is not positive draws nothing. The ring step is
/// at least [`MIN_GLOW_STEP`], rings are placed by index (never by repeated addition, which a tiny
/// step would stall), and at most [`MAX_GLOW_RINGS`] are made.
pub fn gaussian_glow_ops(cap: VRect, bloom: f32, color: Rgba, spec: &GlowSpec) -> Vec<DrawOp> {
    let finite = [
        cap.x, cap.y, cap.w, cap.h, bloom, spec.sigma_base, spec.sigma_gain, spec.band_base,
        spec.band_gain, spec.alpha_base, spec.alpha_gain, spec.min_bloom, spec.min_alpha,
        spec.reach_sigmas,
    ]
    .iter()
    .all(|v| v.is_finite());
    if !finite || cap.w <= 0.0 || cap.h <= 0.0 {
        return Vec::new();
    }
    let b = bloom.clamp(0.0, 1.0);
    if b <= spec.min_bloom {
        return Vec::new();
    }
    let step = if spec.step.is_finite() {
        spec.step.max(MIN_GLOW_STEP)
    } else {
        MIN_GLOW_STEP
    };
    let sigma = spec.sigma_base + spec.sigma_gain * b;
    if !(sigma.is_finite() && sigma > 0.0) {
        return Vec::new();
    }
    let half = (spec.band_base + spec.band_gain * b) * 0.5;
    let base_alpha = (spec.alpha_base + spec.alpha_gain * b) * b;
    let max_depth = (cap.h * 0.5).min(spec.reach_sigmas.max(0.0) * sigma + half.max(0.0));
    if !max_depth.is_finite() || max_depth <= 0.0 {
        return Vec::new();
    }
    let mut ops = Vec::new();
    for ring in 0..MAX_GLOW_RINGS {
        let d = ring as f32 * step;
        if d >= max_depth {
            break;
        }
        let c = d + step * 0.5;
        let intensity = norm_cdf((c + half) / sigma) - norm_cdf((c - half) / sigma);
        let alpha = base_alpha * intensity;
        if alpha > spec.min_alpha {
            ops.push(DrawOp::StrokeRRect {
                rect: cap.inset(c),
                radius: (cap.h * 0.5 - c).max(0.5),
                width: step,
                color: color.with_alpha(alpha),
            });
        }
    }
    ops
}

// ---------------------------------------------------------------------------------------------
// Capsule
// ---------------------------------------------------------------------------------------------

/// The capsule rectangle in an overlay window `win_w` x `win_h`: centred horizontally, its resting
/// bottom edge `bottom_pad` above the window bottom, shifted by `offset_y`.
pub fn capsule_rect(
    win_w: f32,
    win_h: f32,
    bottom_pad: f32,
    cap_w: f32,
    cap_h: f32,
    offset_y: f32,
) -> VRect {
    VRect::new(
        (win_w - cap_w) * 0.5,
        win_h - bottom_pad - cap_h + offset_y,
        cap_w,
        cap_h,
    )
}

/// Rectangles of a row of equal-width bars centred on `(cx, cy)`: `count` bars `bar_w` wide with
/// `gap` between them, bar `i` as tall as `height(i)`.
pub fn bar_row_rects(
    cx: f32,
    cy: f32,
    count: usize,
    bar_w: f32,
    gap: f32,
    height: impl Fn(usize) -> f32,
) -> Vec<VRect> {
    let total = count as f32 * bar_w + (count as f32 - 1.0) * gap;
    let mut x = cx - total * 0.5;
    let mut rects = Vec::with_capacity(count);
    for i in 0..count {
        let h = height(i);
        rects.push(VRect::new(x, cy - h * 0.5, bar_w, h));
        x += bar_w + gap;
    }
    rects
}

/// Centres of `count` round buttons `button` wide with `spacing` between them, centred on `cx`.
pub fn button_row_centers(cx: f32, count: usize, button: f32, spacing: f32) -> Vec<f32> {
    let x0 = cx - (count as f32 * button + (count as f32 - 1.0) * spacing) * 0.5;
    (0..count)
        .map(|i| x0 + button * 0.5 + i as f32 * (button + spacing))
        .collect()
}

/// Positions in the compact (unhovered) recording capsule: bars and timer spaced by equal gaps in
/// the layout frame, then scaled about the capsule centre.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompactRecording {
    /// Centre x of the bars frame.
    pub bars_cx: f32,
    /// Left x of the timer text.
    pub timer_x: f32,
}

/// Lay out the compact recording content: a frame `base_w` wide holding the bars frame
/// (`bars_frame` wide) and the timer text (`timer_w` wide) with three equal gaps, scaled by
/// `scale` about `cx`.
pub fn compact_recording(
    cx: f32,
    base_w: f32,
    bars_frame: f32,
    timer_w: f32,
    scale: f32,
) -> CompactRecording {
    let gap = ((base_w - bars_frame - timer_w) / 3.0).max(0.0);
    CompactRecording {
        bars_cx: cx + (-base_w * 0.5 + gap + bars_frame * 0.5) * scale,
        timer_x: cx + (-base_w * 0.5 + gap * 2.0 + bars_frame) * scale,
    }
}

/// The host's spacing in the hovered recording dock.
#[derive(Clone, Copy, Debug)]
pub struct HoverDock {
    pub button: f32,
    /// Between a button and the next element.
    pub gap: f32,
    pub bars_frame: f32,
    /// Between the bars frame and the timer text.
    pub timer_gap: f32,
}

/// One element of the hovered recording dock, in the order the host lays them out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoverSlot {
    /// A round button; the number is the host's own id for it (the crate attaches no meaning).
    Button(u8),
    /// The bars frame followed by the timer text, laid out as one group (the bars frame, then
    /// `timer_gap`, then the timer).
    BarsTimer,
}

/// Where a [`HoverSlot`] landed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HoverPlacement {
    /// Centre x of the button with the host's id.
    Button { id: u8, cx: f32 },
    /// Centre x of the bars frame and left x of the timer text.
    BarsTimer { bars_cx: f32, timer_x: f32 },
}

/// Lay out the hovered recording dock: `total` wide content centred on `cx`, inset by `pad`, the
/// elements in the host's `order`, left to right. Returns one placement per entry of `order`, in
/// the same order. Which buttons exist, and where the bars and timer sit among them, is the
/// host's; this function only advances the x position.
pub fn hover_recording(
    cx: f32,
    total: f32,
    pad: f32,
    timer_w: f32,
    dock: &HoverDock,
    order: &[HoverSlot],
) -> Vec<HoverPlacement> {
    let mut x = cx - total * 0.5 + pad;
    let mut placed = Vec::with_capacity(order.len());
    for slot in order {
        match *slot {
            HoverSlot::Button(id) => {
                placed.push(HoverPlacement::Button {
                    id,
                    cx: x + dock.button * 0.5,
                });
                x += dock.button + dock.gap;
            }
            HoverSlot::BarsTimer => {
                placed.push(HoverPlacement::BarsTimer {
                    bars_cx: x + dock.bars_frame * 0.5,
                    timer_x: x + dock.bars_frame + dock.timer_gap,
                });
                x += dock.bars_frame + dock.timer_gap + timer_w + dock.gap;
            }
        }
    }
    placed
}

/// The host's measures for a status or error row (icon, then label).
#[derive(Clone, Copy, Debug)]
pub struct StatusSpec {
    pub face: Face,
    pub size: f32,
    /// The label may be this much narrower than the capsule.
    pub label_inset: f32,
    /// The label's width never goes below this.
    pub min_label_w: f32,
    pub icon_w: f32,
    /// Between the icon and the label.
    pub icon_gap: f32,
}

/// Positions in a status or error capsule.
#[derive(Clone, Debug, PartialEq)]
pub struct StatusRow {
    /// The label, shortened with an ellipsis to fit.
    pub label: String,
    /// The measured width of `label`.
    pub label_w: f32,
    /// Centre x of the icon.
    pub icon_cx: f32,
    /// Left x of the label.
    pub label_x: f32,
}

/// Lay out a status row for `label` centred on `cx` in a capsule `cap_w` wide.
pub fn status_row(cx: f32, cap_w: f32, label: &str, spec: &StatusSpec) -> StatusRow {
    let max_label = (cap_w - spec.label_inset).max(spec.min_label_w);
    let label = ellipsize(label, spec.face, spec.size, max_label);
    let label_w = text_width(spec.face, spec.size, &label);
    let total = spec.icon_w + spec.icon_gap + label_w;
    let x0 = cx - total * 0.5;
    StatusRow {
        label,
        label_w,
        icon_cx: x0 + spec.icon_w * 0.5,
        label_x: x0 + spec.icon_w + spec.icon_gap,
    }
}

// ---------------------------------------------------------------------------------------------
// Cards
// ---------------------------------------------------------------------------------------------

/// Apply a card's pose to its display list: the content fades over the shell (the first
/// `shell_len` ops), then the whole card takes its opacity and scale about `card`'s centre.
pub fn apply_card_pose(mut ops: Vec<DrawOp>, shell_len: usize, card: VRect, pose: AuxPose) -> Vec<DrawOp> {
    if pose.content_alpha < 0.999 {
        let content_ops = fade_ops(&ops[shell_len..], pose.content_alpha);
        ops.truncate(shell_len);
        ops.extend(content_ops);
    }
    if pose.alpha < 0.999 || (pose.scale - 1.0).abs() > 0.0005 {
        ops = transform_ops(&ops, card.cx(), card.cy(), pose.scale, pose.alpha, 0.0);
    }
    ops
}

/// The host's measures for a one-row card.
#[derive(Clone, Copy, Debug)]
pub struct RowSpec {
    pub spacing: f32,
    pub lead: f32,
    pub trail: f32,
    /// Added to the tallest item for the card height.
    pub vertical_padding: f32,
    pub min_height: f32,
    /// Transparent margin between the card and its window.
    pub margin: f32,
}

/// A one-row card: the card, its window and where each item starts.
#[derive(Clone, Debug, PartialEq)]
pub struct RowCard {
    pub card: VRect,
    /// Overlay window size.
    pub window: (f32, f32),
    /// Vertical centre of the row.
    pub cy: f32,
    /// Left x of each item.
    pub xs: Vec<f32>,
}

/// Lay out items of the given `widths` and `heights` in one row.
pub fn row_card(widths: &[f32], heights: &[f32], spec: &RowSpec) -> RowCard {
    let content_w: f32 =
        widths.iter().sum::<f32>() + spec.spacing * widths.len().saturating_sub(1) as f32;
    let row_h = heights.iter().copied().fold(0.0f32, f32::max);
    let card_w = spec.lead + content_w + spec.trail;
    let card_h = (row_h + spec.vertical_padding).max(spec.min_height);
    let m = spec.margin;
    let card = VRect::new(m, m, card_w, card_h);
    let window = (card_w + 2.0 * m, card_h + 2.0 * m);
    let mut xs = Vec::with_capacity(widths.len());
    let mut x = card.x + spec.lead;
    for w in widths {
        xs.push(x);
        x += *w + spec.spacing;
    }
    RowCard {
        card,
        window,
        cy: card.cy(),
        xs,
    }
}

/// The host's measures for a title-and-body block.
#[derive(Clone, Copy, Debug)]
pub struct FeatureSpec {
    pub size: f32,
    pub title_face: Face,
    pub body_face: Face,
    pub max_width: f32,
    pub max_lines: usize,
    /// Between the title and the body.
    pub title_gap: f32,
}

/// A title-and-body block measured to fit.
#[derive(Clone, Debug, PartialEq)]
pub struct FeatureBlock {
    pub lines: Vec<String>,
    pub width: f32,
    pub height: f32,
    pub line_height: f32,
}

/// Whether the title or the body is wider than the block may be.
pub fn feature_needs_wrap(title: &str, body: &str, spec: &FeatureSpec) -> bool {
    let t = text_width(spec.title_face, spec.size, title).ceil();
    let b = text_width(spec.body_face, spec.size, body).ceil();
    t.max(b) > spec.max_width
}

/// Measure a block: the body wrapped to `max_lines` when either text is too wide, else on one line.
pub fn feature_block(title: &str, body: &str, spec: &FeatureSpec) -> FeatureBlock {
    let wrap = feature_needs_wrap(title, body, spec);
    let lines = if wrap {
        wrap_text(body, spec.body_face, spec.size, spec.max_width, spec.max_lines)
    } else {
        vec![body.to_string()]
    };
    let widest = lines
        .iter()
        .map(|l| text_width(spec.body_face, spec.size, l))
        .fold(text_width(spec.title_face, spec.size, title), f32::max);
    let width = if wrap { spec.max_width } else { widest.ceil() };
    let line_height = spec.body_face.line_height(spec.size);
    let height = line_height + spec.title_gap + line_height * lines.len() as f32;
    FeatureBlock {
        lines,
        width,
        height,
        line_height,
    }
}

/// Left x of each element laid right to left from `right`: element `i` is `widths[i]` wide and
/// `gap` separates neighbours.
pub fn right_to_left(right: f32, widths: &[f32], gap: f32) -> Vec<f32> {
    let mut right = right;
    widths
        .iter()
        .map(|w| {
            let x = right - *w;
            right = x - gap;
            x
        })
        .collect()
}

/// A chip at the trailing edge of `row`, vertically centred: `label_w` plus `pad_x` on both sides.
pub fn trailing_chip(row: VRect, label_w: f32, pad_x: f32, height: f32, inset: f32) -> VRect {
    let w = label_w + 2.0 * pad_x;
    VRect::new(row.right() - inset - w, row.cy() - height * 0.5, w, height)
}

/// The host's measures for the single-transcript card.
#[derive(Clone, Copy, Debug)]
pub struct SingleSpec {
    pub face: Face,
    pub size: f32,
    /// Text width estimate: `chars * char_w + extra_w`, within `min_w..=max_w`.
    pub char_w: f32,
    pub extra_w: f32,
    pub min_w: f32,
    pub max_w: f32,
    pub max_lines: usize,
    /// Padding inside the transcript box.
    pub box_padding: f32,
    pub min_box_h: f32,
    /// Between the box and the rail, and between rail icons.
    pub rail_spacing: f32,
    pub icon: f32,
    pub lead: f32,
    pub trail: f32,
    /// Added to the content height for the card height.
    pub vertical_padding: f32,
    /// Between the card and the outer (hit) rectangle.
    pub outer_pad: f32,
    /// Between the outer rectangle and the window.
    pub margin: f32,
    /// Between the card top and the box top.
    pub top_inset: f32,
}

/// The single-transcript card: the wrapped text, the box, the rail icon positions and the card.
#[derive(Clone, Debug, PartialEq)]
pub struct SingleCard {
    pub text_w: f32,
    pub lines: Vec<String>,
    pub line_height: f32,
    pub box_rect: VRect,
    /// Top y of the rail icons.
    pub rail_top: f32,
    /// Left x of each rail icon.
    pub rail_xs: Vec<f32>,
    pub card: VRect,
    pub outer: VRect,
    pub window: (f32, f32),
}

/// Lay out the single card for `shown` with `rail_count` icons.
pub fn single_card(spec: &SingleSpec, shown: &str, rail_count: usize) -> SingleCard {
    let text_w = (shown.chars().count() as f32 * spec.char_w + spec.extra_w)
        .max(spec.min_w)
        .min(spec.max_w);
    let lines = wrap_text(shown, spec.face, spec.size, text_w, spec.max_lines);
    let line_height = spec.face.line_height(spec.size);
    let box_w = text_w + 2.0 * spec.box_padding;
    let box_h = (lines.len() as f32 * line_height + 2.0 * spec.box_padding).max(spec.min_box_h);
    let content_w = box_w + rail_count as f32 * (spec.rail_spacing + spec.icon);
    let content_h = box_h.max(spec.icon);
    let card_w = content_w + spec.lead + spec.trail;
    let card_h = content_h + spec.vertical_padding;
    let m = spec.margin;
    let outer = VRect::new(
        m,
        m,
        card_w + 2.0 * spec.outer_pad,
        card_h + 2.0 * spec.outer_pad,
    );
    let card = VRect::new(
        outer.x + spec.outer_pad,
        outer.y + spec.outer_pad,
        card_w,
        card_h,
    );
    let top = card.y + spec.top_inset;
    let left = card.x + spec.lead;
    let box_rect = VRect::new(left, top, box_w, box_h);
    let mut rail_xs = Vec::with_capacity(rail_count);
    let mut x = box_rect.right() + spec.rail_spacing;
    for _ in 0..rail_count {
        rail_xs.push(x);
        x += spec.icon + spec.rail_spacing;
    }
    let window = (outer.w + 2.0 * m, outer.h + 2.0 * m);
    SingleCard {
        text_w,
        lines,
        line_height,
        box_rect,
        rail_top: top,
        rail_xs,
        card,
        outer,
        window,
    }
}

/// The host's measures for the recent-transcripts card.
#[derive(Clone, Copy, Debug)]
pub struct RecentSpec {
    pub face: Face,
    pub size: f32,
    pub card_w: f32,
    /// Between the card edge and a row.
    pub side_pad: f32,
    /// Row text is `text_inset` narrower than the row on each side pair (`row_w - text_inset`).
    pub text_inset: f32,
    pub max_lines: usize,
    pub min_row_h: f32,
    pub row_gap: f32,
    pub top_pad: f32,
    pub bottom_pad: f32,
    /// Between the last row and the footer.
    pub footer_gap: f32,
    /// Footer height.
    pub footer_h: f32,
    pub outer_pad: f32,
    pub margin: f32,
}

/// The recent card: each row's wrapped text and rectangle, the footer's centre and the card.
#[derive(Clone, Debug, PartialEq)]
pub struct RecentCard {
    pub line_height: f32,
    pub row_lines: Vec<Vec<String>>,
    pub row_rects: Vec<VRect>,
    pub footer_cy: f32,
    pub card: VRect,
    pub outer: VRect,
    pub window: (f32, f32),
}

/// Lay out the recent card for rows whose primary texts are `rows`.
pub fn recent_card(spec: &RecentSpec, rows: &[&str]) -> RecentCard {
    let line_height = spec.face.line_height(spec.size);
    let inner_w = spec.card_w - 2.0 * spec.side_pad;
    let row_lines: Vec<Vec<String>> = rows
        .iter()
        .map(|text| {
            wrap_text(
                text,
                spec.face,
                spec.size,
                inner_w - spec.text_inset,
                spec.max_lines,
            )
        })
        .collect();
    let row_heights: Vec<f32> = row_lines
        .iter()
        .map(|lines| (lines.len() as f32 * line_height).max(spec.min_row_h))
        .collect();
    let rows_h: f32 =
        row_heights.iter().sum::<f32>() + spec.row_gap * rows.len().saturating_sub(1) as f32;
    let card_h = spec.top_pad + rows_h + spec.row_gap + spec.footer_gap + spec.footer_h + spec.bottom_pad;
    let m = spec.margin;
    let outer = VRect::new(
        m,
        m,
        spec.card_w + 2.0 * spec.outer_pad,
        card_h + 2.0 * spec.outer_pad,
    );
    let card = VRect::new(
        outer.x + spec.outer_pad,
        outer.y + spec.outer_pad,
        spec.card_w,
        card_h,
    );
    let mut y = card.y + spec.top_pad;
    let mut row_rects = Vec::with_capacity(rows.len());
    for height in &row_heights {
        row_rects.push(VRect::new(card.x + spec.side_pad, y, inner_w, *height));
        y += *height + spec.row_gap;
    }
    let footer_cy = y + spec.footer_gap + spec.footer_h * 0.5;
    let window = (outer.w + 2.0 * m, outer.h + 2.0 * m);
    RecentCard {
        line_height,
        row_lines,
        row_rects,
        footer_cy,
        card,
        outer,
        window,
    }
}
