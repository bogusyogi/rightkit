//! The strip's popovers and its preview panel (spec 5.3, 6 and 7.6, the Mac's
//! `ClipFilterPopover`, `ClipColourPopover`, `ClipBoardPickerPopover` and `ClipPreviewView`):
//! the filter chips, the board colour swatches with their hex field, the board picker, the menu
//! facsimile of a picture, and the Space preview. Each draws into a picture; the window code
//! decides where a picture goes. Nothing here logs item content.

use super::draw::{hex_parts, is_light, kind_plural};
use super::*;

/// What a popover's controls mean when one is pressed.
#[derive(Clone, PartialEq)]
pub(super) enum PopHit {
    Kind(Option<Kind>),
    App(String),
    Date(DatePick),
    Swatch(usize),
    Hex,
    PickerRow(usize),
}

/// The preview panel's largest size (7.6).
pub(super) const PREVIEW_MAX: (f32, f32) = (560.0, 420.0);
/// The panel's distance from the strip it hangs from (the Mac's `lead`).
pub(super) const PREVIEW_LEAD: f32 = 4.0;

/// One row of the board picker (Ctrl+B).
pub(super) struct PickRow {
    pub id: String,
    pub title: String,
    pub colour: Option<u32>,
    pub ticked: bool,
    pub new: bool,
}

impl Strip {
    // ---- shared parts -------------------------------------------------------------------

    /// The black card a popover sits on, with its 1px edge.
    fn popover_frame(&mut self, canvas: &mut Canvas, rect: Rect) {
        let k = self.k;
        self.popover_rect = Some(rect);
        canvas.fill_continuous_rect((rect.x, rect.y, rect.w, rect.h), 12.0 * k, 0x00_0000, 1.0);
        canvas.stroke_continuous_rect((rect.x, rect.y, rect.w, rect.h), 12.0 * k, 1.0, INK, 0.14);
    }

    fn pop_label(&mut self, canvas: &mut Canvas, text: &str, x: f32, y: f32) {
        let k = self.k;
        self.line(
            canvas,
            text,
            9.5,
            Face::Bold,
            (x, y + 6.0 * k),
            200.0 * k,
            INK,
            META,
        );
    }

    /// Chips wrapped onto as many lines as they need, from `(x, y)` in `room`; the chips'
    /// rectangles and the height used. A chip is 20 high, 4 apart.
    fn chip_flow(&mut self, labels: &[String], x: f32, y: f32, room: f32) -> (Vec<Rect>, f32) {
        let k = self.k;
        let (mut cx, mut cy) = (0.0f32, 0.0f32);
        let mut rects = Vec::with_capacity(labels.len());
        for label in labels {
            let w = self.width_of(label, 10.5, Face::Regular) + 16.0 * k;
            if cx > 0.0 && cx + w > room {
                cx = 0.0;
                cy += 24.0 * k;
            }
            rects.push(Rect {
                x: x + cx,
                y: y + cy,
                w,
                h: 20.0 * k,
            });
            cx += w + 4.0 * k;
        }
        (rects, cy + 20.0 * k)
    }

    fn chip(&mut self, canvas: &mut Canvas, rect: Rect, label: &str, on: bool) {
        let k = self.k;
        canvas.fill_round_rect(
            rect.x,
            rect.y,
            rect.w,
            rect.h,
            [rect.h / 2.0; 4],
            INK,
            if on { 0.18 } else { 0.06 },
        );
        self.line(
            canvas,
            label,
            10.5,
            Face::Regular,
            (rect.x + 8.0 * k, rect.y + rect.h / 2.0),
            rect.w,
            INK,
            if on { CONTENT } else { META },
        );
    }

    // ---- drawing the open popover -------------------------------------------------------

    /// The open popover over the strip's picture, and its controls recorded for the pointer.
    pub(super) fn draw_popover(&mut self, canvas: &mut Canvas) {
        let k = self.k;
        let top = (PAD + HEADER + 4.0) * k;
        self.popover_rect = None;
        match self.popover.clone() {
            Some(Popover::Filters) => self.draw_filters(canvas, top),
            Some(Popover::Colour { board, hex }) => {
                self.draw_colour(canvas, top, &board, &hex);
            }
            Some(Popover::Picker { query, selected }) => {
                let text: String = query.iter().collect();
                self.draw_picker(canvas, top, &text, selected);
            }
            Some(Popover::Context) | None => {}
        }
    }

    fn draw_filters(&mut self, canvas: &mut Canvas, top: f32) {
        let k = self.k;
        let (width, pad) = (250.0 * k, 10.0 * k);
        let x = self.size.0 as f32 - PAD * k - width;
        let inner = width - 2.0 * pad;
        let mut kind_labels: Vec<String> = vec![host::t("All").to_string()];
        kind_labels.extend(
            KINDS
                .iter()
                .map(|kind| host::t(kind_plural(*kind)).to_string()),
        );
        let apps = self.apps_seen();
        let shown_apps: Vec<(String, String)> = apps.into_iter().take(3).collect();
        let dates: Vec<String> = DatePick::ALL
            .iter()
            .map(|d| d.label().to_string())
            .collect();
        // A first pass for the height, a second to draw.
        let measure = |this: &mut Strip| -> f32 {
            let (_, kinds_h) = this.chip_flow(&kind_labels, 0.0, 0.0, inner);
            let (_, dates_h) = this.chip_flow(&dates, 0.0, 0.0, inner);
            let apps_h = if shown_apps.is_empty() {
                0.0
            } else {
                shown_apps.len() as f32 * 14.0 * k + (shown_apps.len() as f32 - 1.0) * 2.0 * k
            };
            let label = 12.0 * k;
            let spacing = 8.0 * k;
            let mut total = label + spacing + kinds_h + spacing + label;
            if apps_h > 0.0 {
                total += spacing + apps_h;
            }
            total += spacing + label + spacing + dates_h;
            total + 2.0 * pad
        };
        let height = measure(self);
        let frame = Rect {
            x,
            y: top,
            w: width,
            h: height,
        };
        self.popover_frame(canvas, frame);
        let mut y = top + pad;
        let ix = x + pad;
        let (label, spacing) = (12.0 * k, 8.0 * k);
        self.pop_label(canvas, host::t("Type"), ix, y);
        y += label + spacing;
        let (rects, used) = self.chip_flow(&kind_labels, ix, y, inner);
        for (n, rect) in rects.iter().enumerate() {
            let (on, hit) = if n == 0 {
                (self.panel.kinds.is_empty(), PopHit::Kind(None))
            } else {
                (
                    self.panel.kinds.contains(&KINDS[n - 1]),
                    PopHit::Kind(Some(KINDS[n - 1])),
                )
            };
            self.chip(canvas, *rect, &kind_labels[n], on);
            self.hits.push((*rect, hit));
        }
        y += used + spacing;
        self.pop_label(canvas, host::t("App"), ix, y);
        y += label;
        if !shown_apps.is_empty() {
            y += spacing;
            for (id, name) in &shown_apps {
                let on = self.panel.apps.iter().any(|a| a == id);
                let row = Rect {
                    x: ix,
                    y,
                    w: inner,
                    h: 14.0 * k,
                };
                let box_size = 10.0 * k;
                let (bx, by) = (ix, y + (row.h - box_size) / 2.0);
                if on {
                    canvas.fill_round_rect(bx, by, box_size, box_size, [2.0 * k; 4], INK, CONTENT);
                    canvas.stroke_polyline(
                        &[
                            (bx + 2.2 * k, by + 5.2 * k),
                            (bx + 4.2 * k, by + 7.2 * k),
                            (bx + 7.8 * k, by + 2.8 * k),
                        ],
                        1.3 * k,
                        false,
                        0x00_0000,
                        1.0,
                    );
                } else {
                    canvas.stroke_polyline(
                        &[
                            (bx, by),
                            (bx + box_size, by),
                            (bx + box_size, by + box_size),
                            (bx, by + box_size),
                        ],
                        1.0 * k,
                        true,
                        INK,
                        META,
                    );
                }
                self.line(
                    canvas,
                    name,
                    11.0,
                    Face::Regular,
                    (ix + 16.0 * k, y + row.h / 2.0),
                    inner - 16.0 * k,
                    INK,
                    if on { CONTENT } else { META },
                );
                self.hits.push((row, PopHit::App(id.clone())));
                y += row.h + 2.0 * k;
            }
            y += spacing - 2.0 * k;
        } else {
            y += spacing;
        }
        self.pop_label(canvas, host::t("Date"), ix, y);
        y += label + spacing;
        let (rects, _) = self.chip_flow(&dates, ix, y, inner);
        for (n, rect) in rects.iter().enumerate() {
            let date = DatePick::ALL[n];
            self.chip(canvas, *rect, &dates[n], self.panel.date == date);
            self.hits.push((*rect, PopHit::Date(date)));
        }
    }

    fn draw_colour(&mut self, canvas: &mut Canvas, top: f32, board: &str, hex: &str) {
        let k = self.k;
        let (width, pad) = (188.0 * k, 10.0 * k);
        let x = (PAD + 120.0) * k;
        let colour = self
            .snap
            .boards
            .iter()
            .find(|b| b.id == board)
            .map_or_else(String::new, |b| b.colour.clone());
        let current = hex_parts(&colour).map_or(GREY, |(rgb, _)| rgb);
        let height = pad * 2.0 + 12.0 * k + 8.0 * k + 2.0 * 24.0 * k + 6.0 * k + 8.0 * k + 22.0 * k;
        let frame = Rect {
            x,
            y: top,
            w: width,
            h: height,
        };
        self.popover_frame(canvas, frame);
        let (ix, mut y) = (x + pad, top + pad);
        self.pop_label(canvas, host::t("Colour"), ix, y);
        y += 12.0 * k + 8.0 * k;
        for (n, (_, swatch)) in clip::PALETTE.iter().enumerate() {
            let (column, row) = ((n % 4) as f32, (n / 4) as f32);
            let cell = Rect {
                x: ix + column * 30.0 * k,
                y: y + row * 30.0 * k,
                w: 24.0 * k,
                h: 24.0 * k,
            };
            let rgb = hex_parts(swatch).map_or(GREY, |(rgb, _)| rgb);
            let (cx, cy) = (cell.x + cell.w / 2.0, cell.y + cell.h / 2.0);
            canvas.fill_round_rect(
                cx - 11.0 * k,
                cy - 11.0 * k,
                22.0 * k,
                22.0 * k,
                [11.0 * k; 4],
                rgb,
                1.0,
            );
            if rgb == current {
                canvas.stroke_arc(cx, cy, 13.0 * k, 2.0 * k, 1.0, INK, CONTENT);
            }
            self.hits.push((cell, PopHit::Swatch(n)));
        }
        y += 24.0 * k + 6.0 * k + 24.0 * k + 8.0 * k;
        let field = Rect {
            x: ix,
            y,
            w: width - 2.0 * pad,
            h: 22.0 * k,
        };
        canvas.fill_round_rect(
            field.x,
            field.y,
            field.w,
            field.h,
            [field.h / 2.0; 4],
            INK,
            0.10,
        );
        let typed = hex_parts(hex).map(|(rgb, _)| rgb);
        canvas.fill_round_rect(
            field.x + 8.0 * k,
            y + 6.0 * k,
            10.0 * k,
            10.0 * k,
            [5.0 * k; 4],
            typed.unwrap_or(current),
            1.0,
        );
        let shown = if hex.is_empty() { "#" } else { hex };
        let text_x = field.x + 8.0 * k + 10.0 * k + 6.0 * k;
        let drawn = self.line(
            canvas,
            shown,
            11.0,
            Face::Mono,
            (text_x, y + field.h / 2.0),
            field.w - 40.0 * k,
            INK,
            CONTENT,
        );
        canvas.fill_round_rect(
            text_x + drawn + 1.0 * k,
            y + 5.0 * k,
            1.2 * k,
            12.0 * k,
            [0.6 * k; 4],
            INK,
            CONTENT,
        );
        self.hits.push((field, PopHit::Hex));
    }

    /// The boards the picker lists for `needle`, then a row to make a new one.
    pub(super) fn picker_rows(&self, needle: &str) -> Vec<PickRow> {
        let item = self.selected_item();
        let needle = needle.trim();
        let folded = clip_search::fold(needle);
        let mut rows: Vec<PickRow> = self
            .snap
            .boards
            .iter()
            .filter(|b| folded.is_empty() || clip_search::fold(&b.name).contains(&folded))
            .map(|b| PickRow {
                id: b.id.clone(),
                title: b.name.clone(),
                colour: rgb_of(&b.colour),
                ticked: item.is_some_and(|i| i.boards.contains(&b.id)),
                new: false,
            })
            .collect();
        if !needle.is_empty()
            && !self
                .snap
                .boards
                .iter()
                .any(|b| b.name.eq_ignore_ascii_case(needle))
        {
            rows.push(PickRow {
                id: "new".to_string(),
                title: needle.to_string(),
                colour: None,
                ticked: false,
                new: true,
            });
        }
        rows
    }

    fn draw_picker(&mut self, canvas: &mut Canvas, top: f32, query: &str, selected: usize) {
        let k = self.k;
        let (width, pad) = (210.0 * k, 10.0 * k);
        let x = (PAD + 20.0) * k;
        let rows = self.picker_rows(query);
        let height = pad * 2.0 + 22.0 * k + rows.len() as f32 * (22.0 * k + 6.0 * k);
        let frame = Rect {
            x,
            y: top,
            w: width,
            h: height,
        };
        self.popover_frame(canvas, frame);
        let (ix, mut y) = (x + pad, top + pad);
        let inner = width - 2.0 * pad;
        canvas.fill_round_rect(ix, y, inner, 22.0 * k, [11.0 * k; 4], INK, 0.10);
        let shown = if query.is_empty() {
            host::t("Add to board")
        } else {
            query
        };
        let drawn = self.line(
            canvas,
            shown,
            11.0,
            Face::Regular,
            (ix + 8.0 * k, y + 11.0 * k),
            inner - 16.0 * k,
            INK,
            CONTENT,
        );
        // The caret sits at the start of the placeholder and after typed text.
        let caret = if query.is_empty() { 0.0 } else { drawn };
        canvas.fill_round_rect(
            ix + 8.0 * k + caret,
            y + 5.0 * k,
            1.2 * k,
            12.0 * k,
            [0.6 * k; 4],
            INK,
            CONTENT,
        );
        y += 22.0 * k + 6.0 * k;
        for (n, row) in rows.iter().enumerate() {
            let rect = Rect {
                x: ix,
                y,
                w: inner,
                h: 22.0 * k,
            };
            if n == selected {
                canvas.fill_round_rect(rect.x, rect.y, rect.w, rect.h, [11.0 * k; 4], INK, 0.14);
            }
            let mut tx = ix + 8.0 * k;
            if let Some(dot) = row.colour {
                canvas.fill_round_rect(
                    tx,
                    y + 11.0 * k - 3.5 * k,
                    7.0 * k,
                    7.0 * k,
                    [3.5 * k; 4],
                    dot,
                    1.0,
                );
                tx += 13.0 * k;
            }
            let title = if row.new {
                host::tf("New board \u{201C}{}\u{201D}", &[&row.title])
            } else {
                row.title.clone()
            };
            self.line(
                canvas,
                &title,
                11.0,
                Face::Regular,
                (tx, y + 11.0 * k),
                inner - (tx - ix) - 22.0 * k,
                INK,
                CONTENT,
            );
            if row.ticked {
                let (cx, cy) = (ix + inner - 14.0 * k, y + 11.0 * k);
                canvas.stroke_polyline(
                    &[
                        (cx - 3.5 * k, cy),
                        (cx - 1.0 * k, cy + 2.5 * k),
                        (cx + 3.5 * k, cy - 2.5 * k),
                    ],
                    1.3 * k,
                    false,
                    INK,
                    CONTENT,
                );
            }
            self.hits.push((rect, PopHit::PickerRow(n)));
            y += 28.0 * k;
        }
    }

    // ---- the menu, as a picture ---------------------------------------------------------

    /// A context menu drawn as a picture (the Mac's `ClipMenuFacsimile`): a live menu is the
    /// system's own. Used by the picture renderer.
    #[cfg_attr(not(feature = "qa-views"), allow(dead_code))] // used by the qa-views screenshots
    pub(super) fn menu_picture(&mut self, entries: &[Entry]) -> Canvas {
        let k = self.k;
        let (pad, row_h, rule_h) = (6.0 * k, 21.5 * k, 7.0 * k);
        let width = 230.0 * k;
        let mut height = 2.0 * pad;
        for entry in entries {
            height += if entry.separator {
                rule_h
            } else {
                row_h + 1.0 * k
            };
        }
        let mut canvas = Canvas::new(width.ceil() as usize, height.ceil() as usize);
        canvas.fill_round_rect(0.0, 0.0, width, height, [10.0 * k; 4], 0x29_2929, 1.0);
        canvas.stroke_continuous_rect((0.0, 0.0, width, height), 10.0 * k, 1.0, INK, 0.14);
        let mut y = pad;
        for entry in entries {
            if entry.separator {
                canvas.fill_round_rect(
                    pad + 4.0 * k,
                    y + rule_h / 2.0 - 0.5,
                    width - 2.0 * pad - 8.0 * k,
                    1.0,
                    [0.0; 4],
                    INK,
                    0.14,
                );
                y += rule_h;
                continue;
            }
            let (title, shortcut) = entry
                .label
                .split_once('\t')
                .unwrap_or((entry.label.as_str(), ""));
            self.line(
                &mut canvas,
                title,
                13.0,
                Face::Regular,
                (pad + 10.0 * k, y + row_h / 2.0),
                width - 2.0 * pad - 90.0 * k,
                INK,
                CONTENT,
            );
            let right = if entry.children.is_empty() {
                shortcut.to_string()
            } else {
                "\u{25B8}".to_string()
            };
            if !right.is_empty() {
                let text_w = self.width_of(&right, 13.0, Face::Regular);
                self.line(
                    &mut canvas,
                    &right,
                    13.0,
                    Face::Regular,
                    (width - pad - 10.0 * k - text_w, y + row_h / 2.0),
                    text_w + 2.0,
                    INK,
                    META,
                );
            }
            y += row_h + 1.0 * k;
        }
        canvas
    }

    // ---- the preview --------------------------------------------------------------------

    /// The Space preview of `item` as a picture (a solid card up to 560 by 420 DIPs): its size
    /// by kind, as the Mac sizes it. `text` is the item's full text (or its paths, one a line);
    /// `image` its picture when it is one.
    pub(super) fn preview_picture(
        &mut self,
        item: &Item,
        text: &str,
        image: Option<(usize, usize, Arc<Vec<u32>>)>,
    ) -> Canvas {
        let k = self.k;
        let limit = (PREVIEW_MAX.0 * k, PREVIEW_MAX.1 * k);
        let line_count = text.split('\n').count();
        let (width, height) = match item.kind {
            Kind::Image => {
                let (w, h) = item
                    .meta_num("width")
                    .zip(item.meta_num("height"))
                    .map_or((4.0, 3.0), |(w, h)| (w as f32, h as f32));
                let fit = (limit.0 / w).min(limit.1 / h).min(1.0);
                ((160.0 * k).max(w * fit), (120.0 * k).max(h * fit))
            }
            Kind::Colour => (260.0 * k, 160.0 * k),
            Kind::File => {
                let count = item.meta_num("count").map_or(1, |n| n as usize).min(14);
                (
                    limit.0.min(480.0 * k),
                    limit.1.min(40.0 * k + 16.0 * k * count as f32),
                )
            }
            _ => {
                let lines = (line_count + text.chars().count() / 70).clamp(3, 30);
                (limit.0, limit.1.min(32.0 * k + lines as f32 * 16.0 * k))
            }
        };
        let (w, h) = (width.ceil() as usize, height.ceil() as usize);
        let mut canvas = Canvas::new(w, h);
        let radius = (CARD_CORNER * k) + 2.0;
        canvas.fill_continuous_rect((0.0, 0.0, w as f32, h as f32), radius, 0x00_0000, 1.0);
        let tile = Rect {
            x: 0.0,
            y: 0.0,
            w: w as f32,
            h: h as f32,
        };
        match item.kind {
            Kind::Image => {
                canvas.fill_rect_within(
                    (0.0, 0.0, w as f32, h as f32),
                    (0.0, 0.0, w as f32, h as f32),
                    radius,
                    0x11_1111,
                    1.0,
                );
                if let Some(name) = self.arts.get(&item.id).cloned() {
                    super::draw::draw_art(&mut canvas, &name, tile, layout::scale(self.dpi));
                } else if let Some((iw, ih, pixels)) = image {
                    let scale = (tile.w / iw as f32).min(tile.h / ih as f32);
                    let (dw, dh) = (iw as f32 * scale, ih as f32 * scale);
                    canvas.draw_image(
                        &pixels,
                        (iw, ih),
                        (tile.w - dw) / 2.0,
                        (tile.h - dh) / 2.0,
                        dw,
                        dh,
                    );
                }
            }
            Kind::Colour => {
                let hex = item
                    .meta_str("hex")
                    .map_or_else(|| item.preview.clone(), str::to_string);
                let (rgb, alpha) = hex_parts(&hex).unwrap_or((GREY, 1.0));
                canvas.fill_continuous_rect((0.0, 0.0, w as f32, h as f32), radius, rgb, alpha);
                let ink = if is_light(rgb) { 0x00_0000 } else { INK };
                let shown = text.to_uppercase();
                if let Some(mask) = self.mask(&shown, 18.0, Face::Mono) {
                    let x = (w as f32 - mask.width as f32) / 2.0;
                    Self::put(
                        &mut canvas,
                        &mask,
                        x,
                        h as f32 / 2.0,
                        w as f32,
                        ink,
                        CONTENT,
                    );
                }
            }
            kind => {
                // The Mac's pitch: 15 pt for 12 pt text; the first line keeps its place.
                let (size, face, line_h, first) = if kind == Kind::Code {
                    (11.0, Face::Mono, 11.0 * 1.3, 11.0 * 0.65)
                } else {
                    (12.0, Face::Regular, 15.0, 12.0 * 0.65)
                };
                let pad = 14.0 * k;
                let room = w as f32 - 2.0 * pad;
                let lines = (((h as f32 - 2.0 * pad) / (line_h * k)).floor() as usize).max(1);
                let rows = self.wrap(text, size, face, room, lines);
                for (n, row) in rows.iter().enumerate() {
                    let y = pad + (first + n as f32 * line_h) * k;
                    self.line(&mut canvas, row, size, face, (pad, y), room, INK, CONTENT);
                }
            }
        }
        canvas
    }
}
