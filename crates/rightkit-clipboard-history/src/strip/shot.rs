//! The clipboard strip as a picture for the CI view renderer (`qa/notch-views.json` entries of
//! kind `clipboard-strip`, spec 10): the strip's own drawing, put through the card's emergence
//! settled and out, on a notch drawn as a plain stub on the fixture's edge (as the Mac's
//! `ClipboardStripPicture` does), with the Space preview and the menu's stand-in beside it
//! when the fixture asks. The fixture holds every string; every date is worked from its `now`
//! in UTC, never from the clock. Nothing here touches the history on disk.

use super::*;

type Parts = Vec<(Canvas, i32, i32)>;

/// The fixture's items need ids the store would accept: these stand in for the fixture's own.
fn stand_in_id(n: usize) -> String {
    format!("{:08x}-0000-4000-8000-{:012x}", n + 1, n + 1)
}

/// The fixture keys the item reader takes as booleans.
const BOOLEANS: [&str; 6] = [
    "pinned",
    "concealed",
    "anyMissing",
    "screenshot",
    "hasHtml",
    "hasRtf",
];

/// `value` with its `id` replaced, and its booleans (which the view renderer hands over as 1
/// and 0) read as booleans again, down into its `meta`.
fn with_id(value: &json::Value, id: &str) -> json::Value {
    match value {
        json::Value::Object(members) => json::Value::Object(
            members
                .iter()
                .map(|(key, member)| {
                    let member = match member {
                        json::Value::Number(n) if BOOLEANS.contains(&key.as_str()) => {
                            json::Value::Bool(*n >= 1.0)
                        }
                        json::Value::Object(_) if key == "meta" => with_id(member, ""),
                        other => other.clone(),
                    };
                    if key == "id" && !id.is_empty() {
                        (key.clone(), json::Value::Text(id.to_string()))
                    } else {
                        (key.clone(), member)
                    }
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The picture of the strip a fixture describes, as parts to lay over a backdrop: `(canvas, x,
/// y)` from the top-left of the first. The strip open (`open` not false); the toast of a
/// closed one is the notch's own card and is drawn by the caller.
pub fn shot(fixture: &json::Value, dpi: u32) -> Result<Parts, &'static str> {
    let painter = TextPainter::new().ok_or("No text rasteriser")?;
    let mut strip = Strip::blank(
        OwnedWindow::adopt(HWND::default()),
        OwnedWindow::adopt(HWND::default()),
        painter,
    );
    let text = |key: &str| fixture.get(key).and_then(json::Value::as_str);
    let number = |key: &str| fixture.get(key).and_then(json::Value::as_f64);
    let edge = Edge::parse(text("edge").unwrap_or("top")).ok_or("Unknown notch edge")?;
    let bottom = text("placement") == Some("bottom");
    let scale = number("scale").unwrap_or(1.0) as f32;
    strip.size_class = CardSize::named(text("size").unwrap_or("medium"));
    // The view renderer hands booleans over as 1 and 0.
    strip.numbers = number("showNumbers").is_none_or(|n| n >= 1.0);
    strip.fixed_now = Some(number("now").unwrap_or(1_791_800_000.0));
    strip.offset = 0;
    strip.dpi = dpi;
    strip.still = true;

    // The boards, in the fixture's order.
    for (order, entry) in fixture
        .get("boards")
        .and_then(json::Value::as_array)
        .unwrap_or(&[])
        .iter()
        .enumerate()
    {
        let mut board = clip::parse_board(entry).ok_or("A fixture board is not valid")?;
        board.order = order as i64;
        strip.snap.boards.push(board);
    }
    // The items, in the fixture's order: the cards are made from them one to one.
    let mut stacks: Vec<usize> = Vec::new();
    for (n, entry) in fixture
        .get("items")
        .and_then(json::Value::as_array)
        .unwrap_or(&[])
        .iter()
        .enumerate()
    {
        let id = stand_in_id(n);
        let mut item =
            clip::parse_item(&with_id(entry, &id)).ok_or("A fixture item is not valid")?;
        item.created = item.used;
        if let Some(app) = &item.source
            && let Some(rgb) = entry
                .get("appColour")
                .and_then(json::Value::as_str)
                .and_then(rgb_of)
        {
            strip.colours.insert(app.id.clone(), rgb);
        }
        if let Some(name) = entry.get("thumb").and_then(json::Value::as_str) {
            strip.arts.insert(id, name.to_string());
        }
        stacks.push(
            entry
                .get("stackCount")
                .and_then(json::Value::as_f64)
                .map_or(1, |count| count.max(1.0) as usize),
        );
        strip.snap.items.push(item);
    }
    strip.colours_loaded = true;
    strip.cards = stacks
        .iter()
        .enumerate()
        .map(|(item, stack)| Card {
            item,
            stack: *stack,
        })
        .collect();
    strip.total = strip.cards.len();
    strip.hover_scale = vec![Spring::at(0.0); strip.cards.len()];
    strip.selected = if strip.cards.is_empty() {
        0
    } else {
        (number("selected").unwrap_or(0.0).max(0.0) as usize).min(strip.cards.len() - 1)
    };

    // The view, the search and the filters.
    let board = text("board").unwrap_or("history");
    strip.view = match board {
        "history" => View::History,
        "pinned" => View::Pinned,
        id => View::Board(id.to_string()),
    };
    strip.query = text("query").unwrap_or("").chars().collect();
    strip.searching = !strip.query.is_empty();
    if let Some(filters) = fixture.get("filters") {
        for name in filters
            .get("types")
            .and_then(json::Value::as_array)
            .unwrap_or(&[])
        {
            if let Some(kind) = name
                .as_str()
                .and_then(|name| Kind::parse(name.to_lowercase().trim_end_matches('s')))
            {
                strip.panel.kinds.push(kind);
            }
        }
        for id in filters
            .get("apps")
            .and_then(json::Value::as_array)
            .unwrap_or(&[])
        {
            if let Some(id) = id.as_str() {
                strip.panel.apps.push(id.to_string());
            }
        }
        let date = filters
            .get("date")
            .and_then(json::Value::as_str)
            .unwrap_or("any")
            .to_lowercase();
        strip.panel.date = if date.contains("yesterday") {
            DatePick::Yesterday
        } else if date.contains("today") {
            DatePick::Today
        } else if date.contains("30") {
            DatePick::Last30
        } else if date.contains('7') {
            DatePick::Last7
        } else {
            DatePick::Any
        };
    }
    strip.popover = match text("popover") {
        Some("filters") => Some(Popover::Filters),
        Some("boardColour") => {
            let id = match &strip.view {
                View::Board(id) => Some(id.clone()),
                _ => strip.snap.boards.first().map(|b| b.id.clone()),
            };
            id.map(|board| Popover::Colour {
                board,
                hex: String::new(),
            })
        }
        Some("context") => Some(Popover::Context),
        _ => None,
    };
    strip.fixed_footer = text("footer").map(str::to_string);
    if strip.cards.is_empty() {
        strip.fixed_empty = text("message").map(|message| {
            (
                message.to_string(),
                text("detail").map(str::to_string),
                text("stripAction").map(str::to_string),
            )
        });
    }

    // Where it hangs: a display wide enough for the Mac's picture (a row at most 1392 long, a
    // column of three and a half cards) and a notch stub at its middle on the fixture's edge.
    let s = layout::scale(dpi);
    let k = s * scale.clamp(0.8, 1.25);
    let margin = (EDGE_MARGIN * s).round() as i32;
    let vertical = matches!(edge, Edge::Left | Edge::Right) && !bottom;
    let (wide, high) = if vertical {
        let length =
            (PAD * 2.0 + HEADER + HEADER_GAP + 3.5 * strip.size_class.side() + 3.0 * SPACING) * k;
        ((900.0 * s) as i32, length.round() as i32 + 2 * margin)
    } else {
        ((1392.0 * s).round() as i32 + 2 * margin, (900.0 * s) as i32)
    };
    let (long, depth) = ((320.0 * s).round() as i32, (32.0 * s).round() as i32);
    let notch = match edge {
        Edge::Top => (wide / 2 - long / 2, 0, wide / 2 + long / 2, depth),
        Edge::Bottom => (wide / 2 - long / 2, high - depth, wide / 2 + long / 2, high),
        Edge::Left => (0, high / 2 - long / 2, depth, high / 2 + long / 2),
        Edge::Right => (wide - depth, high / 2 - long / 2, wide, high / 2 + long / 2),
    };
    let spot = NotchSpot { edge, notch, scale };
    strip.layout(Some(spot), (0, 0, wide, high), dpi, bottom);
    strip.scroll = number("scrollOffset").unwrap_or(0.0) as f32 * strip.k;
    strip.clamp_scroll();
    strip.ring.settle(strip.ring_goal());
    let frame = strip.render();

    let mut parts: Parts = Vec::new();
    let radius = CORNER * strip.k;
    match (strip.pose, strip.tail()) {
        (Some(pose), Some(tail)) => {
            // The notch, then the strip on its neck, settled out and open.
            parts.push((stub(edge, notch, s), notch.0, notch.1));
            let window =
                render::render_emerging_picture(&frame, tail, &pose, (1.0, 1.0), radius, dpi);
            parts.push((window, pose.window.0, pose.window.1));
        }
        _ => parts.push((frame, strip.place.0, strip.place.1)),
    }
    // The Space preview, hung from the strip.
    if number("preview").is_some_and(|n| n >= 1.0)
        && let Some(item) = strip.selected_item().cloned()
    {
        let picture = strip.preview_picture(&item, &item.preview, None);
        let (pose, edge) = strip.preview_pose(&picture);
        let tail = Tail { edge, offset: 0 };
        let canvas = render::render_emerging_picture(
            &picture,
            tail,
            &pose,
            (1.0, 1.0),
            CARD_CORNER * strip.k + 2.0,
            dpi,
        );
        parts.push((canvas, pose.window.0, pose.window.1));
    }
    // The menu's stand-in, over the strip and past its edge.
    if matches!(strip.popover, Some(Popover::Context))
        && let Some(item) = strip.selected_item()
    {
        let id = item.id.clone();
        let entries = strip.menu_entries(&MenuFor::Card(id));
        let menu = strip.menu_picture(&entries);
        let (ox, oy) = strip.screen_origin();
        let k = strip.k;
        parts.push((
            menu,
            ox + ((PAD + 40.0) * k).round() as i32,
            oy + ((PAD + HEADER + 34.0) * k).round() as i32,
        ));
    }
    // From the top-left of what was drawn.
    let left = parts.iter().map(|(_, x, _)| *x).min().unwrap_or(0);
    let top = parts.iter().map(|(_, _, y)| *y).min().unwrap_or(0);
    Ok(parts
        .into_iter()
        .map(|(canvas, x, y)| (canvas, x - left, y - top))
        .collect())
}

/// The notch as a plain black stub on its edge: round on the side away from the screen's edge.
pub fn stub(edge: Edge, notch: (i32, i32, i32, i32), s: f32) -> Canvas {
    let (w, h) = ((notch.2 - notch.0).max(1), (notch.3 - notch.1).max(1));
    let mut canvas = Canvas::new(w as usize, h as usize);
    let r = 14.0 * s;
    let radii = match edge {
        Edge::Top => [0.0, 0.0, r, r],
        Edge::Bottom => [r, r, 0.0, 0.0],
        Edge::Left => [0.0, r, r, 0.0],
        Edge::Right => [r, 0.0, 0.0, r],
    };
    canvas.fill_round_rect(0.0, 0.0, w as f32, h as f32, radii, 0x00_0000, 1.0);
    canvas
}
