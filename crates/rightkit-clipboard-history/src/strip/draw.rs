//! What the strip draws (spec 7.2 to 7.4, the Mac's `ClipboardStripView` in the Mac's metrics):
//! the header with its tabs, search field and filter button, the cards, the empty and error
//! states, the footer and the selection ring. One picture of the strip's own size; the window
//! (`clip_strip.rs`) puts it on the screen, and the picture renderer (`shot.rs`) draws it from a
//! fixture. Nothing here logs item content.

use super::*;
use rightkit_win_ui::glyphs::RowGlyph;

/// The accent the selected part of a search is lit in.
const SELECTION: u32 = 0x0A_84FF;
/// The strip's cards are cut by a rectangle this far outside the area they fill, so a ring
/// and a hovered card are not cut off at the edge of the area (the Mac's 6pt).
const OVERSHOOT: f32 = 6.0;

/// `#RRGGBB` or `#RRGGBBAA`: the colour and its opacity.
pub(super) fn hex_parts(hex: &str) -> Option<(u32, f32)> {
    let digits = hex.strip_prefix('#').unwrap_or(hex);
    if !digits.is_ascii() || !matches!(digits.len(), 6 | 8) {
        return None;
    }
    let rgb = u32::from_str_radix(&digits[..6], 16).ok()?;
    let alpha = if digits.len() == 8 {
        f32::from(u8::from_str_radix(&digits[6..], 16).ok()?) / 255.0
    } else {
        1.0
    };
    Some((rgb, alpha))
}

/// Whether black reads better than white on `rgb` (7.4).
pub(super) fn is_light(rgb: u32) -> bool {
    let channel = |shift: u32| ((rgb >> shift) & 0xFF) as f32 / 255.0;
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0) > 0.55
}

pub(super) fn kind_plural(kind: Kind) -> &'static str {
    match kind {
        Kind::Text => "Text",
        Kind::Link => "Links",
        Kind::Colour => "Colours",
        Kind::Code => "Code",
        Kind::File => "Files",
        Kind::Image => "Images",
        Kind::Rich => "Rich",
    }
}

/// A byte count as the Mac's `ByteCountFormatter` words it: decimal units, no decimals under
/// a megabyte or from a hundred, and no trailing zero ("120 MB", "1.5 MB", "1.25 GB").
pub(super) fn bytes_text(bytes: u64) -> String {
    let value = bytes as f64;
    let trimmed = |number: f64, places: usize| {
        let text = format!("{number:.places$}");
        if text.contains('.') {
            text.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            text
        }
    };
    if bytes == 1 {
        "1 byte".to_string()
    } else if bytes < 1000 {
        format!("{bytes} bytes")
    } else if bytes < 1_000_000 {
        format!("{:.0} KB", value / 1000.0)
    } else if bytes < 1_000_000_000 {
        let mb = value / 1_000_000.0;
        format!("{} MB", trimmed(mb, if mb >= 100.0 { 0 } else { 1 }))
    } else {
        format!("{} GB", trimmed(value / 1_000_000_000.0, 2))
    }
}

impl Strip {
    // ---- the picture --------------------------------------------------------------------

    /// The strip as a picture of its own size: black, the header, the cards (or the empty
    /// state), the footer, the selection ring and any popover.
    pub(super) fn render(&mut self) -> Canvas {
        let k = self.k;
        let (w, h) = self.size;
        let mut canvas = Canvas::new(w, h);
        // Solid black like the notch: no glass, whatever the notch's surface says.
        canvas.fill_continuous_rect((0.0, 0.0, w as f32, h as f32), CORNER * k, 0x00_0000, 1.0);
        if self.still {
            self.draw_still_chrome(&mut canvas);
        } else {
            self.draw_header(&mut canvas);
        }
        self.clamp_scroll();
        self.card_rects.clear();
        self.action_rect = None;
        self.hits.clear();
        let (pad, header, gap) = (PAD * k, HEADER * k, HEADER_GAP * k);
        let top = pad + header + gap;
        if self.cards.is_empty() {
            self.draw_empty(&mut canvas, top);
        } else {
            self.draw_cards(&mut canvas, top);
        }
        if !self.still {
            self.draw_footer(&mut canvas);
        }
        self.draw_popover(&mut canvas);
        canvas
    }

    /// The footer line, along the panel's bottom.
    fn draw_footer(&mut self, canvas: &mut Canvas) {
        let (w, h) = self.size;
        let pad = PAD * self.k;
        if let Some((text, alert)) = self.footer_text() {
            let color = if alert { ALERT } else { INK };
            let alpha = if alert { 1.0 } else { META };
            let y = h as f32 - pad - FOOTER * self.k / 2.0;
            self.line(
                canvas,
                &text,
                9.5,
                Face::Regular,
                (pad, y),
                w as f32 - 2.0 * pad,
                color,
                alpha,
            );
        }
    }

    /// The size a still lays its content out at (the Mac's stack grows to its run and the
    /// frame centres what it cannot fit) and the offset that puts it back: `(w, h, dx, dy)`.
    fn still_extent(&self) -> (f32, f32, f32, f32) {
        let (w, h) = (self.size.0 as f32, self.size.1 as f32);
        let shift = self.overflow_shift();
        if self.vertical {
            (w, h - 2.0 * shift, 0.0, shift)
        } else {
            (w - 2.0 * shift, h, shift, 0.0)
        }
    }

    /// A still's header and footer, laid out with the run and slid with it, so a run too long
    /// for the panel carries them off its edges as the Mac's picture does.
    fn draw_still_chrome(&mut self, canvas: &mut Canvas) {
        let (vw, vh, dx, dy) = self.still_extent();
        let real = self.size;
        self.size = (vw.ceil() as usize, vh.ceil() as usize);
        let mut layer = Canvas::new(self.size.0, self.size.1);
        self.draw_header(&mut layer);
        self.draw_footer(&mut layer);
        self.size = real;
        canvas.draw_image(
            &layer.pixels,
            (layer.width, layer.height),
            dx.round(),
            dy.round(),
            layer.width as f32,
            layer.height as f32,
        );
    }

    /// The area the cards fill, widened by `OVERSHOOT` all round: `(x, y, width, height)`.
    fn card_area(&self) -> (f32, f32, f32, f32) {
        let k = self.k;
        let (w, h) = (self.size.0 as f32, self.size.1 as f32);
        let (pad, header, gap) = (PAD * k, HEADER * k, HEADER_GAP * k);
        let footer = if self.footer_shown() { FOOTER * k } else { 0.0 };
        if self.still {
            // The body the Mac clips the run to: the ring is cut at its edges too.
            let (vw, vh, dx, dy) = self.still_extent();
            let (x, y) = ((pad + dx).round(), (pad + header + gap + dy).round());
            let width = (vw - 2.0 * pad).ceil().max(1.0);
            let height = (vh - (pad * 2.0 + header + gap) - footer).ceil().max(1.0);
            return (x, y, width, height);
        }
        let over = OVERSHOOT * k;
        let x = (pad - over).round();
        let y = (pad + header + gap - over).round();
        let right = (w - pad + over).round();
        let bottom = (h - pad - footer + over).round();
        (x, y, (right - x).max(1.0), (bottom - y).max(1.0))
    }

    fn draw_cards(&mut self, canvas: &mut Canvas, top: f32) {
        let k = self.k;
        let pad = PAD * k;
        let (ax, ay, aw, ah) = self.card_area();
        let mut area = Canvas::new(aw as usize, ah as usize);
        let step = self.card_px + SPACING * k;
        let side = self.card_px;
        let shift = self.overflow_shift();
        for index in 0..self.cards.len() {
            let along = index as f32 * step - self.scroll + shift;
            let rect = if self.vertical {
                Rect {
                    x: pad,
                    y: top + along,
                    w: side,
                    h: side,
                }
            } else {
                Rect {
                    x: pad + along,
                    y: top,
                    w: side,
                    h: side,
                }
            };
            self.card_rects.push(rect);
            let local = Rect {
                x: rect.x - ax,
                y: rect.y - ay,
                w: side,
                h: side,
            };
            let visible =
                local.x + local.w > 0.0 && local.y + local.h > 0.0 && local.x < aw && local.y < ah;
            if visible {
                self.draw_card_grown(&mut area, index, local);
            }
        }
        // The ring glides to the selected card: along the strip it is on its own spring.
        if self.selected < self.cards.len() {
            let along = self.ring.value - self.scroll + shift;
            let rect = if self.vertical {
                Rect {
                    x: pad - ax,
                    y: top + along - ay,
                    w: side,
                    h: side,
                }
            } else {
                Rect {
                    x: pad + along - ax,
                    y: top - ay,
                    w: side,
                    h: side,
                }
            };
            let ring = 5.0 * k;
            area.stroke_continuous_rect(
                (
                    rect.x - ring,
                    rect.y - ring,
                    rect.w + 2.0 * ring,
                    rect.h + 2.0 * ring,
                ),
                (CARD_CORNER + 4.0) * k,
                2.0 * k,
                INK,
                CONTENT,
            );
        }
        canvas.draw_image(&area.pixels, (area.width, area.height), ax, ay, aw, ah);
    }

    /// A card at its hover scale (1.04 on the emphasis spring) about its middle.
    fn draw_card_grown(&mut self, area: &mut Canvas, index: usize, rect: Rect) {
        let grown = self.hover_scale.get(index).map_or(0.0, |s| s.value);
        let scale = 1.0 + (HOVER_SCALE - 1.0) * grown;
        if (scale - 1.0).abs() < 0.0005 {
            self.draw_card(area, index, rect);
            return;
        }
        // The card is drawn on its own and put back larger.
        let margin = 8.0 * self.k;
        let side = rect.w;
        let size = (side + 2.0 * margin).ceil();
        let mut own = Canvas::new(size as usize, size as usize);
        let at = Rect {
            x: margin,
            y: margin,
            w: side,
            h: rect.h,
        };
        self.draw_card(&mut own, index, at);
        let big = size * scale;
        let x = rect.x + rect.w / 2.0 - big / 2.0;
        let y = rect.y + rect.h / 2.0 - big / 2.0;
        area.draw_image(&own.pixels, (own.width, own.height), x, y, big, big);
    }

    // ---- the header ---------------------------------------------------------------------

    fn draw_header(&mut self, canvas: &mut Canvas) {
        let k = self.k;
        let (pad, header, pill, space) = (PAD * k, HEADER * k, TAB_HEIGHT * k, 6.0 * k);
        let mid = pad + header / 2.0;
        let width = self.size.0 as f32;
        self.tabs.clear();
        let expanded = self.searching || !self.query.is_empty();
        // The filter button, at the far end.
        let filter = Rect {
            x: width - pad - pill,
            y: mid - pill / 2.0,
            w: pill,
            h: pill,
        };
        let active = self.panel.active();
        canvas.fill_round_rect(
            filter.x,
            filter.y,
            pill,
            pill,
            [pill / 2.0; 4],
            INK,
            if active { 0.14 } else { 0.06 },
        );
        let ink = if active { CONTENT } else { META };
        let (cx, cy) = (filter.x + pill / 2.0, mid);
        for (n, half) in [5.0, 3.5, 2.0].into_iter().enumerate() {
            let y = cy + (n as f32 - 1.0) * 3.2 * k;
            canvas.fill_round_rect(
                cx - half * k,
                y - 0.6 * k,
                2.0 * half * k,
                1.2 * k,
                [0.6 * k; 4],
                INK,
                ink,
            );
        }
        self.tabs.push((filter, Tab::Filter, None));
        // The search: a round button, or the field (the whole header's width on a column).
        let right = filter.x - space;
        let field_w = if !expanded {
            pill
        } else if self.vertical {
            (right - pad).max(pill)
        } else {
            SEARCH_WIDTH * k
        };
        let field = Rect {
            x: right - field_w,
            y: mid - pill / 2.0,
            w: field_w,
            h: pill,
        };
        if expanded {
            self.draw_field(canvas, field, mid);
        } else {
            canvas.fill_round_rect(field.x, field.y, pill, pill, [pill / 2.0; 4], INK, 0.06);
            glyphs::row_glyph(
                canvas,
                RowGlyph::Magnifier,
                (field.x + pill / 2.0, mid),
                11.0 * k,
                INK,
                META,
            );
        }
        self.tabs.push((field, Tab::Search, None));
        // The tabs fill what is left; on a column an open search covers them.
        if expanded && self.vertical {
            return;
        }
        self.draw_tabs(canvas, pad, field.x - space, mid);
    }

    fn draw_field(&mut self, canvas: &mut Canvas, field: Rect, mid: f32) {
        let k = self.k;
        canvas.fill_round_rect(
            field.x,
            field.y,
            field.w,
            field.h,
            [field.h / 2.0; 4],
            INK,
            0.10,
        );
        let mut x = field.x + 8.0 * k;
        glyphs::row_glyph(
            canvas,
            RowGlyph::Magnifier,
            (x + 5.5 * k, mid),
            11.0 * k,
            INK,
            META,
        );
        x += 11.0 * k + 5.0 * k;
        let limit = field.x + field.w - 8.0 * k;
        for chip in self.query().chips {
            let text_w = self.width_of(&chip, 9.5, Face::Regular);
            let w = text_w + 12.0 * k;
            if x + w > limit {
                break;
            }
            canvas.fill_round_rect(x, mid - 8.0 * k, w, 16.0 * k, [8.0 * k; 4], INK, 0.14);
            self.line(
                canvas,
                &chip,
                9.5,
                Face::Regular,
                (x + 6.0 * k, mid),
                text_w + 2.0,
                INK,
                CONTENT,
            );
            x += w + 5.0 * k;
        }
        let text = self.text();
        let room = (limit - x - 2.0 * k).max(0.0);
        let mut end = x;
        if !text.is_empty() {
            let drawn = self.width_of(&text, 11.0, Face::Regular).min(room);
            if self.select_all {
                canvas.fill_round_rect(
                    x - 2.0 * k,
                    mid - 8.0 * k,
                    drawn + 4.0 * k,
                    16.0 * k,
                    [3.0 * k; 4],
                    SELECTION,
                    0.55,
                );
            }
            self.line(
                canvas,
                &text,
                11.0,
                Face::Regular,
                (x, mid),
                room,
                INK,
                CONTENT,
            );
            end = x + drawn;
        }
        // The caret, a thin bar after the text.
        canvas.fill_round_rect(
            end.min(limit) + 1.0 * k,
            mid - 6.5 * k,
            1.2 * k,
            13.0 * k,
            [0.6 * k; 4],
            INK,
            CONTENT,
        );
    }

    /// The tabs: History, Pinned, each board, a board being named, and a plus. The row is as
    /// long as it needs and is cut to what the header has left; the wheel over the header moves
    /// it (the Mac's tabs scroll sideways).
    fn draw_tabs(&mut self, canvas: &mut Canvas, left: f32, right: f32, mid: f32) {
        let k = self.k;
        let (header, pill) = (HEADER * k, TAB_HEIGHT * k);
        let region_w = (right - left).max(0.0);
        if region_w < 1.0 {
            return;
        }
        struct Pill {
            label: String,
            tab: Tab,
            board: Option<String>,
            dot: Option<u32>,
            selected: bool,
            editing: bool,
        }
        let editing_text = |naming: &Naming| naming.text.iter().collect::<String>();
        let mut pills: Vec<Pill> = vec![
            Pill {
                label: "History".to_string(),
                tab: Tab::View(View::History),
                board: None,
                dot: None,
                selected: self.view == View::History,
                editing: false,
            },
            Pill {
                label: "Pinned".to_string(),
                tab: Tab::View(View::Pinned),
                board: None,
                dot: None,
                selected: self.view == View::Pinned,
                editing: false,
            },
        ];
        for board in &self.snap.boards {
            let renaming = self
                .naming
                .as_ref()
                .filter(|naming| naming.rename.as_deref() == Some(board.id.as_str()));
            pills.push(Pill {
                label: renaming.map_or_else(|| board.name.clone(), editing_text),
                tab: Tab::View(View::Board(board.id.clone())),
                board: Some(board.id.clone()),
                dot: Some(rgb_of(&board.colour).unwrap_or(GREY)),
                selected: self.view == View::Board(board.id.clone()),
                editing: renaming.is_some(),
            });
        }
        if let Some(naming) = self.naming.as_ref().filter(|n| n.rename.is_none()) {
            pills.push(Pill {
                label: editing_text(naming),
                tab: Tab::New,
                board: None,
                dot: rgb_of(clip::PALETTE[7].1),
                selected: false,
                editing: true,
            });
        }
        let mut widths: Vec<f32> = Vec::with_capacity(pills.len() + 1);
        for item in &pills {
            let text_w = self.width_of(&item.label, 11.0, Face::Regular)
                + if item.editing { 3.0 * k } else { 0.0 };
            let dot_w = if item.dot.is_some() { 11.0 * k } else { 0.0 };
            widths.push(text_w + dot_w + 18.0 * k);
        }
        // The plus that starts a board.
        let plus = pill;
        let total: f32 = widths.iter().sum::<f32>() + plus + 4.0 * k * widths.len() as f32;
        self.tab_scroll = self.tab_scroll.clamp(0.0, (total - region_w).max(0.0));
        let mut row = Canvas::new(region_w as usize + 1, header as usize + 1);
        let centre = header / 2.0;
        let mut x = -self.tab_scroll;
        let mut recorded: Vec<(Rect, Tab, Option<String>)> = Vec::new();
        for (item, width) in pills.iter().zip(&widths) {
            let rect = Rect {
                x,
                y: centre - pill / 2.0,
                w: *width,
                h: pill,
            };
            if item.selected {
                row.fill_round_rect(rect.x, rect.y, rect.w, rect.h, [pill / 2.0; 4], INK, 0.14);
            }
            let mut text_x = rect.x + 9.0 * k;
            if let Some(colour) = item.dot {
                row.fill_round_rect(
                    text_x,
                    centre - 3.0 * k,
                    6.0 * k,
                    6.0 * k,
                    [3.0 * k; 4],
                    colour,
                    1.0,
                );
                text_x += 11.0 * k;
            }
            let alpha = if item.selected || item.editing {
                CONTENT
            } else {
                META
            };
            let drawn = self.line(
                &mut row,
                &item.label,
                11.0,
                Face::Regular,
                (text_x, centre - 0.5 * k),
                rect.w,
                INK,
                alpha,
            );
            if item.editing {
                row.fill_round_rect(
                    text_x + drawn + 1.0 * k,
                    centre - 6.0 * k,
                    1.2 * k,
                    12.0 * k,
                    [0.6 * k; 4],
                    INK,
                    CONTENT,
                );
            }
            recorded.push((
                Rect {
                    x: left + rect.x,
                    y: mid - pill / 2.0,
                    w: rect.w,
                    h: pill,
                },
                item.tab.clone(),
                item.board.clone(),
            ));
            x += width + 4.0 * k;
        }
        // The plus: two strokes, in the metadata ink.
        let (px, py) = (x + plus / 2.0, centre);
        for (dx, dy) in [(4.5, 0.0), (0.0, 4.5)] {
            row.fill_round_rect(
                px - dx * k - if dx == 0.0 { 0.75 * k } else { 0.0 },
                py - dy * k - if dy == 0.0 { 0.75 * k } else { 0.0 },
                if dx == 0.0 { 1.5 * k } else { 2.0 * dx * k },
                if dy == 0.0 { 1.5 * k } else { 2.0 * dy * k },
                [0.75 * k; 4],
                INK,
                META,
            );
        }
        recorded.push((
            Rect {
                x: left + x,
                y: mid - pill / 2.0,
                w: plus,
                h: pill,
            },
            Tab::New,
            None,
        ));
        canvas.draw_image(
            &row.pixels,
            (row.width, row.height),
            left,
            mid - header / 2.0,
            row.width as f32,
            row.height as f32,
        );
        // A tab only counts where it shows.
        for (rect, tab, board) in recorded {
            let from = rect.x.max(left);
            let to = (rect.x + rect.w).min(right);
            if to > from {
                self.tabs.push((
                    Rect {
                        x: from,
                        w: to - from,
                        ..rect
                    },
                    tab,
                    board,
                ));
            }
        }
    }

    // ---- the empty and error states -----------------------------------------------------

    fn draw_empty(&mut self, canvas: &mut Canvas, top: f32) {
        let k = self.k;
        let (title, detail, action) = self.empty_lines();
        let (w, h) = (self.size.0 as f32, self.size.1 as f32);
        let footer = if self.footer_shown() { FOOTER * k } else { 0.0 };
        let area_h = (h - PAD * k - footer - top).max(0.0);
        // The lines, 4pt apart, centred in the area: the title, the detail, the action.
        let title_h = 15.0 * k;
        let detail_h = if detail.is_some() {
            11.0 * k + 4.0 * k
        } else {
            0.0
        };
        let action_h = if action.is_some() {
            TAB_HEIGHT * k + 6.0 * k + 4.0 * k
        } else {
            0.0
        };
        let block = title_h + detail_h + action_h;
        // The Mac's lines sit a point higher in theirs (r9 B8).
        let mut y = top + (area_h - block) / 2.0 - 1.0 * k;
        if let Some(mask) = self.mask(&title, 12.5, Face::Bold) {
            let x = (w - mask.width as f32) / 2.0;
            Self::put(canvas, &mask, x, y + title_h / 2.0, w, INK, CONTENT);
        }
        y += title_h;
        if let Some(detail) = detail
            && let Some(mask) = self.mask(&detail, 9.5, Face::Regular)
        {
            y += 4.0 * k;
            let x = (w - mask.width as f32) / 2.0;
            Self::put(canvas, &mask, x, y + 5.5 * k, w, INK, META);
            y += 11.0 * k;
        }
        if let Some(label) = action {
            y += 4.0 * k + 6.0 * k;
            let text_w = self.width_of(&label, 11.0, Face::Regular);
            let pill_w = text_w + 20.0 * k;
            let rect = Rect {
                x: (w - pill_w) / 2.0,
                y,
                w: pill_w,
                h: TAB_HEIGHT * k,
            };
            canvas.fill_round_rect(rect.x, rect.y, rect.w, rect.h, [rect.h / 2.0; 4], INK, 0.14);
            self.line(
                canvas,
                &label,
                11.0,
                Face::Regular,
                (rect.x + 10.0 * k, rect.y + rect.h / 2.0),
                text_w + 2.0,
                INK,
                CONTENT,
            );
            self.action_rect = Some(rect);
        }
    }

    // ---- a card -------------------------------------------------------------------------

    fn draw_card(&mut self, canvas: &mut Canvas, index: usize, rect: Rect) {
        let k = self.k;
        let card = self.cards[index];
        let item = self.snap.items[card.item].clone();
        let side = rect.w;
        let corner = CARD_CORNER * k;
        let shape = (rect.x, rect.y, rect.w, rect.h);
        let on_colour = item.kind == Kind::Colour;
        let hex = item
            .meta_str("hex")
            .map_or_else(|| item.preview.clone(), str::to_string);
        let (colour_rgb, colour_alpha) = hex_parts(&hex).unwrap_or((GREY, 1.0));
        // Ink over a colour card is black on a light colour.
        let (primary, secondary) = if on_colour {
            let base = if is_light(colour_rgb) { 0x00_0000 } else { INK };
            ((base, CONTENT), (base, META))
        } else {
            ((INK, CONTENT), (INK, META))
        };
        // A screenshot stack has two edges behind it.
        if card.stack > 1 {
            canvas.fill_round_rect(
                rect.x + 9.0 * k,
                rect.y + 3.0 * k,
                side - 6.0 * k,
                side - 6.0 * k,
                [corner; 4],
                INK,
                0.04,
            );
            canvas.fill_round_rect(
                rect.x + 18.0 * k,
                rect.y + 6.0 * k,
                side - 12.0 * k,
                side - 12.0 * k,
                [corner; 4],
                INK,
                0.03,
            );
        }
        if on_colour {
            if colour_alpha < 0.999 {
                // A checkerboard shows through a colour that is not opaque.
                let cell = 8.0 * k;
                let cells = (side / cell).ceil() as usize;
                for row in 0..cells {
                    for column in 0..cells {
                        let grey = if (row + column).is_multiple_of(2) {
                            0x8C_8C8C
                        } else {
                            0xCC_CCCC
                        };
                        let (x, y) = (rect.x + column as f32 * cell, rect.y + row as f32 * cell);
                        canvas.fill_rect_within((x, y, cell, cell), shape, corner, grey, 1.0);
                    }
                }
            }
            canvas.fill_continuous_rect(shape, corner, colour_rgb, colour_alpha);
        } else {
            canvas.fill_continuous_rect(shape, corner, INK, 0.06);
        }
        // The band: the source app's colour, cut by the card's corners.
        let band = self.app_colour(&item);
        canvas.fill_rect_within((rect.x, rect.y, rect.w, BAND * k), shape, corner, band, 1.0);
        if item.pinned {
            // A small tab hanging from the band, with the pin on it.
            let (tab_w, tab_h) = (16.0 * k, 11.0 * k);
            let tab_x = rect.x + rect.w - 8.0 * k - tab_w;
            canvas.fill_round_rect(
                tab_x,
                rect.y,
                tab_w,
                tab_h,
                [0.0, 0.0, 5.0 * k, 5.0 * k],
                band,
                1.0,
            );
            // pin.fill: a cap, a stem, a flange and the needle, one outline.
            let (px, py) = (tab_x + tab_w / 2.0, rect.y);
            let outline: Vec<(f32, f32)> = [
                (-1.9, 1.75),
                (1.9, 1.75),
                (1.9, 3.0),
                (1.1, 3.0),
                (1.1, 4.4),
                (3.0, 5.4),
                (3.0, 6.8),
                (0.45, 6.8),
                (0.45, 10.0),
                (-0.45, 10.0),
                (-0.45, 6.8),
                (-3.0, 6.8),
                (-3.0, 5.4),
                (-1.1, 4.4),
                (-1.1, 3.0),
                (-1.9, 3.0),
            ]
            .iter()
            .map(|&(x, y)| (px + x * k, py + y * k))
            .collect();
            canvas.fill_polygon(&[outline], 0x00_0000, 0.8);
        }
        let left = rect.x + 8.0 * k;
        let inner = rect.w - 16.0 * k;
        // The header line: the app's icon and name, the kind's glyph.
        let line_y = rect.y + (3.0 + 5.0 + 6.5) * k;
        let icon = 13.0 * k;
        let app_icon = item
            .source
            .as_ref()
            .and_then(|app| clip_capture::app_path(&app.id))
            .and_then(|path| fileicon::of(&path));
        match app_icon {
            Some(image) => canvas.draw_image(
                &image.pixels,
                (image.width, image.height),
                left,
                line_y - icon / 2.0,
                icon,
                icon,
            ),
            None => canvas.fill_round_rect(
                left,
                line_y - icon / 2.0,
                icon,
                icon,
                [icon * 0.24; 4],
                band,
                1.0,
            ),
        }
        if self.size_class != CardSize::Small
            && let Some(app) = &item.source
        {
            let name = app.name.clone();
            self.line(
                canvas,
                &name,
                9.5,
                Face::Regular,
                (left + icon + 5.0 * k, line_y),
                inner - 2.0 * icon - 10.0 * k,
                secondary.0,
                secondary.1,
            );
        }
        self.draw_kind_glyph(
            canvas,
            item.kind,
            (rect.x + rect.w - 8.0 * k - icon / 2.0, line_y),
            secondary,
        );
        // The body, by kind: images and colours run to the card's edges.
        let flush = on_colour || item.kind == Kind::Image;
        let body_top = rect.y + (3.0 + 5.0 + 13.0 + 5.0) * k;
        let body_bottom = rect.y + side - (6.0 + 12.0 + 3.0) * k;
        let body = Rect {
            x: if flush { rect.x } else { left },
            y: body_top,
            w: if flush { rect.w } else { inner },
            h: (body_bottom - body_top).max(0.0),
        };
        self.draw_body(canvas, &item, card.stack, rect, body, (primary, secondary));
        // The footer: the age, the language, the size, the boards' dots.
        let foot_y = rect.y + side - 12.0 * k;
        let has_number = self.numbers && index < 9;
        let mut x = left + if has_number { 19.0 * k } else { 0.0 };
        let when = clip_search::relative(item.used, self.now(), self.offset);
        let when_w = self.line(
            canvas,
            &when,
            9.5,
            Face::Regular,
            (x, foot_y),
            inner * 0.5,
            secondary.0,
            secondary.1,
        );
        x += when_w + 4.0 * k;
        if item.kind == Kind::Code
            && self.size_class != CardSize::Small
            && let Some(language) = item.meta_str("language")
        {
            let language = language.to_string();
            let text_w = self.width_of(&language, 8.5, Face::Regular);
            if !on_colour {
                canvas.fill_round_rect(
                    x,
                    foot_y - 6.0 * k,
                    text_w + 8.0 * k,
                    12.0 * k,
                    [6.0 * k; 4],
                    INK,
                    0.10,
                );
            }
            self.line(
                canvas,
                &language,
                8.5,
                Face::Regular,
                (x + 4.0 * k, foot_y),
                text_w + 2.0,
                secondary.0,
                secondary.1,
            );
        }
        let mut right = rect.x + rect.w - 8.0 * k;
        let dots: Vec<u32> = self
            .snap
            .boards
            .iter()
            .filter(|b| item.boards.contains(&b.id))
            .take(3)
            .map(|b| rgb_of(&b.colour).unwrap_or(GREY))
            .collect();
        for dot in dots.iter().rev() {
            right -= 5.0 * k;
            canvas.fill_round_rect(
                right,
                foot_y - 2.5 * k,
                5.0 * k,
                5.0 * k,
                [2.5 * k; 4],
                *dot,
                1.0,
            );
            right -= 4.0 * k;
        }
        let detail = match item.kind {
            Kind::Text | Kind::Code | Kind::Rich => {
                let chars = item
                    .meta_num("chars")
                    .map_or_else(|| item.preview.chars().count(), |n| n as usize);
                format!("{chars} chars")
            }
            Kind::File | Kind::Image => bytes_text(item.size),
            Kind::Link | Kind::Colour => String::new(),
        };
        if !detail.is_empty() {
            let width = self.width_of(&detail, 9.5, Face::Regular);
            self.line(
                canvas,
                &detail,
                9.5,
                Face::Regular,
                (right - width, foot_y),
                width + 2.0,
                secondary.0,
                secondary.1,
            );
        }
        if has_number {
            // The quick-paste number, a small disc at the bottom left.
            let radius = 7.0 * k;
            let (cx, cy) = (rect.x + 6.0 * k + radius, rect.y + side - 6.0 * k - radius);
            if !on_colour {
                canvas.fill_round_rect(
                    cx - radius,
                    cy - radius,
                    2.0 * radius,
                    2.0 * radius,
                    [radius; 4],
                    INK,
                    0.14,
                );
            }
            let digit = (index + 1).to_string();
            if let Some(mask) = self.mask(&digit, 9.0, Face::Bold) {
                let x = cx - mask.width as f32 / 2.0;
                let ink = if on_colour { primary.0 } else { INK };
                let alpha = if on_colour { primary.1 } else { CONTENT };
                Self::put(canvas, &mask, x, cy, 2.0 * radius, ink, alpha);
            }
        }
    }

    fn draw_kind_glyph(
        &mut self,
        canvas: &mut Canvas,
        kind: Kind,
        centre: (f32, f32),
        (ink, alpha): (u32, f32),
    ) {
        let size = 12.0 * self.k;
        if kind != Kind::Rich {
            super::kinds::draw(
                canvas,
                kind,
                super::kinds::Place::Header,
                centre,
                size,
                (ink, alpha),
            );
        } else if let Some(mask) = self.mask("Aa", 10.0, Face::Bold) {
            let x = centre.0 - mask.width as f32 / 2.0;
            Self::put(canvas, &mask, x, centre.1, 40.0 * self.k, ink, alpha);
        }
    }

    fn draw_body(
        &mut self,
        canvas: &mut Canvas,
        item: &Item,
        stack: usize,
        card: Rect,
        body: Rect,
        (primary, secondary): ((u32, f32), (u32, f32)),
    ) {
        let k = self.k;
        match item.kind {
            Kind::Text | Kind::Rich => {
                // The Mac sets 11 pt text on a 14 pt pitch; the first line stays where it was.
                let line_h = 14.0 * k;
                let fit = ((body.h / (11.0 * 1.2 * k)).floor() as usize).max(1);
                let lines = self.size_class.lines().min(fit);
                let rows = self.wrap(&item.preview, 11.0, Face::Regular, body.w, lines);
                for (n, row) in rows.iter().enumerate() {
                    let y = body.y + 6.6 * k + n as f32 * line_h;
                    self.line(
                        canvas,
                        row,
                        11.0,
                        Face::Regular,
                        (body.x, y),
                        body.w,
                        primary.0,
                        primary.1,
                    );
                }
            }
            Kind::Code => {
                // 10 pt code on a 13 pt pitch.
                let line_h = 13.0 * k;
                let fit = ((body.h / (10.0 * 1.2 * k)).floor() as usize).max(1);
                let lines = (self.size_class.lines() + 1).min(fit);
                let rows = self.wrap(&item.preview, 10.0, Face::Mono, body.w, lines);
                for (n, row) in rows.iter().enumerate() {
                    let y = body.y + 6.0 * k + n as f32 * line_h;
                    self.line(
                        canvas,
                        row,
                        10.0,
                        Face::Mono,
                        (body.x, y),
                        body.w,
                        primary.0,
                        primary.1,
                    );
                }
            }
            Kind::Link => {
                let mailto = item.meta_str("scheme") == Some("mailto");
                let host = item
                    .meta_str("host")
                    .map_or_else(|| item.preview.clone(), str::to_string);
                let icon_y = body.y + 8.0 * k;
                if mailto {
                    // An envelope.
                    let (cx, cy, w, h) = (body.x + 5.5 * k, icon_y, 11.0 * k, 8.0 * k);
                    canvas.stroke_polyline(
                        &[
                            (cx - w / 2.0, cy - h / 2.0),
                            (cx + w / 2.0, cy - h / 2.0),
                            (cx + w / 2.0, cy + h / 2.0),
                            (cx - w / 2.0, cy + h / 2.0),
                        ],
                        1.0 * k,
                        true,
                        secondary.0,
                        secondary.1,
                    );
                    canvas.stroke_polyline(
                        &[
                            (cx - w / 2.0, cy - h / 2.0),
                            (cx, cy + h * 0.1),
                            (cx + w / 2.0, cy - h / 2.0),
                        ],
                        1.0 * k,
                        false,
                        secondary.0,
                        secondary.1,
                    );
                } else {
                    glyphs::row_glyph(
                        canvas,
                        RowGlyph::Globe,
                        (body.x + 5.5 * k, icon_y),
                        11.0 * k,
                        secondary.0,
                        secondary.1,
                    );
                }
                self.line(
                    canvas,
                    &host,
                    12.5,
                    Face::Bold,
                    (body.x + 16.5 * k, icon_y),
                    body.w - 16.5 * k,
                    primary.0,
                    primary.1,
                );
                let path = link_path(&item.preview);
                if !path.is_empty() {
                    let rows = self.wrap(&path, 10.0, Face::Regular, body.w, 2);
                    for (n, row) in rows.iter().enumerate() {
                        let y = body.y + 8.0 * k + 8.0 * k + 4.0 * k + (n as f32 + 0.5) * 12.0 * k;
                        self.line(
                            canvas,
                            row,
                            10.0,
                            Face::Regular,
                            (body.x, y),
                            body.w,
                            secondary.0,
                            secondary.1,
                        );
                    }
                }
            }
            Kind::Colour => {
                let hex = item
                    .meta_str("hex")
                    .map_or_else(|| item.preview.clone(), str::to_string)
                    .to_uppercase();
                if let Some(mask) = self.mask(&hex, 12.5, Face::Mono) {
                    let x = card.x + (card.w - mask.width as f32) / 2.0;
                    Self::put(
                        canvas,
                        &mask,
                        x,
                        body.y + body.h / 2.0,
                        card.w,
                        primary.0,
                        primary.1,
                    );
                }
            }
            Kind::File => self.draw_file_body(canvas, item, body, (primary, secondary)),
            Kind::Image => self.draw_image_body(canvas, item, stack, card, body),
        }
    }

    /// A file: its icon over its name, how many more there are, and "Missing" in red.
    fn draw_file_body(
        &mut self,
        canvas: &mut Canvas,
        item: &Item,
        body: Rect,
        (primary, secondary): ((u32, f32), (u32, f32)),
    ) {
        let k = self.k;
        let paths = self.paths_of(&item.id);
        let first = paths.first().cloned().unwrap_or_default();
        let names = item.meta_names();
        let name = names
            .first()
            .cloned()
            .unwrap_or_else(|| item.preview.clone());
        let count = item
            .meta_num("count")
            .map_or(names.len().max(1), |n| n as usize);
        let missing = matches!(item.meta_value("anyMissing"), Some(json::Value::Bool(true)))
            || paths
                .iter()
                .take(8)
                .any(|path| !std::path::Path::new(path).exists());
        let icon = 48.0 * k;
        let rows = self.wrap(&name, 11.0, Face::Regular, body.w, 2);
        let mut height = icon;
        height += rows.len() as f32 * (14.0 * k + 0.0) + 4.0 * k * rows.len().min(1) as f32;
        if count > 1 {
            height += 4.0 * k + 12.0 * k;
        }
        if missing {
            height += 4.0 * k + 12.0 * k;
        }
        let mut y = body.y + ((body.h - height) / 2.0).max(0.0);
        let cx = body.x + body.w / 2.0;
        match fileicon::of(&first) {
            Some(image) => canvas.draw_image(
                &image.pixels,
                (image.width, image.height),
                cx - icon / 2.0,
                y,
                icon,
                icon,
            ),
            // `doc.fill` scaled to fit the icon's box less 6 pt each side.
            None => {
                super::kinds::page_fill(canvas, (cx, y + icon / 2.0), icon - 12.0 * k, (INK, META))
            }
        }
        y += icon + 4.0 * k;
        for row in &rows {
            if let Some(mask) = self.mask(row, 11.0, Face::Regular) {
                let x = cx - (mask.width as f32).min(body.w) / 2.0;
                Self::put(canvas, &mask, x, y + 7.0 * k, body.w, primary.0, primary.1);
            }
            y += 14.0 * k;
        }
        if count > 1 {
            let text = format!("+{} more", count - 1);
            if let Some(mask) = self.mask(&text, 9.5, Face::Regular) {
                let x = cx - mask.width as f32 / 2.0;
                Self::put(
                    canvas,
                    &mask,
                    x,
                    y + 10.0 * k,
                    body.w,
                    secondary.0,
                    secondary.1,
                );
            }
            y += 16.0 * k;
        }
        if missing && let Some(mask) = self.mask("Missing", 9.5, Face::Bold) {
            let x = cx - mask.width as f32 / 2.0;
            Self::put(canvas, &mask, x, y + 10.0 * k, body.w, ALERT, 1.0);
        }
    }

    /// An image: its thumbnail (or a stand-in picture) on a dark tile, and the count of a stack.
    fn draw_image_body(
        &mut self,
        canvas: &mut Canvas,
        item: &Item,
        stack: usize,
        card: Rect,
        tile: Rect,
    ) {
        let k = self.k;
        canvas.fill_rect_within(
            (tile.x, tile.y, tile.w, tile.h),
            (card.x, card.y, card.w, card.h),
            CARD_CORNER * k,
            0x11_1111,
            1.0,
        );
        if let Some(name) = self.arts.get(&item.id).cloned() {
            draw_art(canvas, &name, tile, layout::scale(self.dpi));
        } else {
            match self.thumb(&item.id) {
                Some((w, h, pixels)) => {
                    let scale = (tile.w / w as f32).min(tile.h / h as f32);
                    let (dw, dh) = (w as f32 * scale, h as f32 * scale);
                    canvas.draw_image(
                        &pixels,
                        (w, h),
                        tile.x + (tile.w - dw) / 2.0,
                        tile.y + (tile.h - dh) / 2.0,
                        dw,
                        dh,
                    );
                }
                None => glyphs::row_glyph(
                    canvas,
                    RowGlyph::Doc,
                    (tile.x + tile.w / 2.0, tile.y + tile.h / 2.0),
                    22.0 * k,
                    INK,
                    META,
                ),
            }
        }
        if stack > 1 {
            let text = stack.to_string();
            let height = 16.0 * k;
            let text_w = self.width_of(&text, 10.0, Face::Bold);
            let width = (text_w + 12.0 * k).max(18.0 * k);
            let x = tile.x + tile.w - 5.0 * k - width;
            let y = tile.y + 5.0 * k;
            canvas.fill_round_rect(x, y, width, height, [height / 2.0; 4], INK, 0.88);
            self.line(
                canvas,
                &text,
                10.0,
                Face::Bold,
                (x + (width - text_w) / 2.0, y + height / 2.0),
                width,
                0x00_0000,
                0.85,
            );
        }
    }
}

/// The stand-in pictures of the fixtures' `thumb` names (no image files are needed).
/// The stand-in picture `name`, as the Mac draws it: its shapes in fixed points (`unit` device
/// pixels each), whatever the tile's size.
pub(super) fn draw_art(canvas: &mut Canvas, name: &str, tile: Rect, unit: f32) {
    let (x0, y0) = (
        tile.x.floor().max(0.0) as usize,
        tile.y.floor().max(0.0) as usize,
    );
    let (x1, y1) = (
        ((tile.x + tile.w).ceil().max(0.0) as usize).min(canvas.width),
        ((tile.y + tile.h).ceil().max(0.0) as usize).min(canvas.height),
    );
    let mix = |a: u32, b: u32, t: f32| -> u32 {
        let lerp = |shift: u32| {
            let (p, q) = (((a >> shift) & 0xFF) as f32, ((b >> shift) & 0xFF) as f32);
            (p + (q - p) * t.clamp(0.0, 1.0)).round() as u32
        };
        (lerp(16) << 16) | (lerp(8) << 8) | lerp(0)
    };
    let k = unit;
    let set = |canvas: &mut Canvas, x: usize, y: usize, rgb: u32| {
        canvas.pixels[y * canvas.width + x] = 0xFF00_0000 | rgb;
    };
    match name {
        "gradient-blue" | "gradient-orange" => {
            let (from, to) = if name == "gradient-blue" {
                (0x5E_9BFF, 0x2A_4FA8)
            } else {
                (0xFF_9F0A, 0xC2_410C)
            };
            for y in y0..y1 {
                for x in x0..x1 {
                    let t = ((x as f32 - tile.x) / tile.w + (y as f32 - tile.y) / tile.h) / 2.0;
                    set(canvas, x, y, mix(from, to, t));
                }
            }
        }
        "photo-green" => {
            for y in y0..y1 {
                for x in x0..x1 {
                    let t = (y as f32 - tile.y) / tile.h;
                    set(canvas, x, y, mix(0x9A_D3F0, 0x2E_7D32, t));
                }
            }
            canvas.fill_round_rect(
                tile.x + tile.w / 2.0 + 34.0 * k - 14.0 * k,
                tile.y + tile.h / 2.0 - 30.0 * k - 14.0 * k,
                28.0 * k,
                28.0 * k,
                [14.0 * k; 4],
                0xFF_E082,
                1.0,
            );
        }
        "screenshot-light" | "screenshot-dark" => {
            let (background, bar, line) = if name == "screenshot-light" {
                (0xF0_F0F0, 0xCC_CCCC, 0xB3_B3B3)
            } else {
                (0x29_2929, 0x42_4242, 0x5C_5C5C)
            };
            canvas.fill_round_rect(tile.x, tile.y, tile.w, tile.h, [0.0; 4], background, 1.0);
            let (left, top) = (tile.x + 8.0 * k, tile.y + 8.0 * k);
            canvas.fill_round_rect(left, top, tile.w - 16.0 * k, 10.0 * k, [0.0; 4], bar, 1.0);
            for row in 0..5 {
                canvas.fill_round_rect(
                    left,
                    top + 15.0 * k + row as f32 * 9.0 * k,
                    (150.0 - row as f32 * 18.0) * k,
                    4.0 * k,
                    [0.0; 4],
                    line,
                    1.0,
                );
            }
        }
        _ => canvas.fill_round_rect(tile.x, tile.y, tile.w, tile.h, [0.0; 4], 0x3A_3A3C, 1.0),
    }
}
