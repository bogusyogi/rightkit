//! Search, ranking and the words for dates in the clipboard history (spec sections 5 and
//! 7.13). One function, `search`, serves the strip and the launcher's clip mode, so the order
//! is the same in both. Pure code over the items in memory: no Win32 and no disk.
//!
//! Matching is a case- and diacritic-insensitive substring of the preview, the file names,
//! the source app's name, the link's host, a colour's hex, the kind's name and the names of
//! the item's boards; every term must match. The spec names the `unicode-normalization`
//! crate for the folding; this folds the Latin letters with a table of its own so that no
//! dependency is added (the same result for the Latin alphabets; other scripts compare
//! after lower-casing only).

use crate::model::{Board, Item, Kind};

/// Which items the strip lists before the query narrows them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum View {
    /// Everything, newest use first.
    History,
    /// Items with `pinned` set.
    Pinned,
    /// The items on one board (its id), in the board's own order.
    Board(String),
}

/// What the query's prefixes ask for (`type:link`, `app:chrome`, `board:work`, `is:pinned`,
/// `before:2026-10-01`, `after:2026-10-01`); all of them combine with AND.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filters {
    pub types: Vec<Kind>,
    /// Folded text the source app's id or name must contain, one entry per `app:`.
    pub apps: Vec<String>,
    /// Folded board names, one entry per `board:`.
    pub boards: Vec<String>,
    /// Source app ids picked in the filter popover: any one of them (the popover's own, not a
    /// prefix of the query).
    pub app_ids: Vec<String>,
    pub pinned: bool,
    /// Unix seconds: created before this.
    pub before: Option<f64>,
    /// Unix seconds: created at or after this.
    pub after: Option<f64>,
}

/// A query split into its plain terms (folded) and its prefix filters.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query {
    pub terms: Vec<String>,
    /// The prefixes the query holds, as typed (`type:link`), for the chips beside the text.
    pub chips: Vec<String>,
    pub filters: Filters,
}

/// Lower-case with the accents taken off Latin letters (and combining marks dropped).
pub fn fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars().flat_map(char::to_lowercase) {
        if ('\u{300}'..='\u{36f}').contains(&c) {
            continue;
        }
        out.push(base_letter(c));
    }
    out
}

fn base_letter(c: char) -> char {
    if c.is_ascii() {
        return c;
    }
    const GROUPS: [(&str, char); 15] = [
        ("àáâãäåāăąǎ", 'a'),
        ("çćĉċč", 'c'),
        ("ďđ", 'd'),
        ("èéêëēĕėęě", 'e'),
        ("ĝğġģ", 'g'),
        ("ìíîïĩīĭįıǐ", 'i'),
        ("ĺļľł", 'l'),
        ("ñńņňŉ", 'n'),
        ("òóôõöøōŏőǒ", 'o'),
        ("ŕŗř", 'r'),
        ("śŝşšß", 's'),
        ("ţťŧ", 't'),
        ("ùúûüũūŭůűųǔ", 'u'),
        ("ýÿŷ", 'y'),
        ("źżž", 'z'),
    ];
    GROUPS
        .iter()
        .find(|(set, _)| set.contains(c))
        .map_or(c, |(_, base)| *base)
}

/// Reads `text` into terms and prefix filters. `offset_secs` is the local time zone's offset
/// from UTC, for `before:` and `after:` (local day boundaries).
pub fn parse_query(text: &str, offset_secs: i64) -> Query {
    let mut query = Query::default();
    for word in text.split_whitespace() {
        if let Some((prefix, value)) = word.split_once(':')
            && !value.is_empty()
            && apply_prefix(
                &mut query.filters,
                &prefix.to_ascii_lowercase(),
                value,
                offset_secs,
            )
        {
            query
                .chips
                .push(format!("{}:{value}", prefix.to_ascii_lowercase()));
            continue;
        }
        query.terms.push(fold(word));
    }
    query
}

/// True when `prefix:value` was a filter (it is then not a plain term).
fn apply_prefix(filters: &mut Filters, prefix: &str, value: &str, offset_secs: i64) -> bool {
    match prefix {
        "type" | "kind" => {
            let name = fold(value);
            let name = name.trim_end_matches('s');
            let found = match name {
                "colour" | "color" => Some(Kind::Colour),
                other => Kind::parse(other),
            };
            match found {
                Some(kind) => {
                    filters.types.push(kind);
                    true
                }
                None => false,
            }
        }
        "app" => {
            filters.apps.push(fold(value));
            true
        }
        "board" => {
            filters.boards.push(fold(value));
            true
        }
        "is" if value.eq_ignore_ascii_case("pinned") => {
            filters.pinned = true;
            true
        }
        "before" | "after" => match day_start(value, offset_secs) {
            Some(at) => {
                if prefix == "before" {
                    filters.before = Some(at);
                } else {
                    filters.after = Some(at);
                }
                true
            }
            None => false,
        },
        _ => false,
    }
}

/// Unix seconds at the local midnight that starts `YYYY-MM-DD`.
fn day_start(text: &str, offset_secs: i64) -> Option<f64> {
    let mut parts = text.split('-');
    let year = parts.next()?.parse::<i64>().ok()?;
    let month = parts.next()?.parse::<i64>().ok()?;
    let day = parts.next()?.parse::<i64>().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(
        (rightkit_time::days_from_civil(year, month as u32, day as u32) * 86_400 - offset_secs)
            as f64,
    )
}

// ---- matching and ranking ----------------------------------------------------------------

/// Everything a term may match in one item, folded.
struct Haystack {
    preview: String,
    first_line: String,
    fields: Vec<String>,
    boards: Vec<String>,
    kind: &'static str,
}

fn haystack(item: &Item, boards: &[Board]) -> Haystack {
    let preview = fold(&item.preview);
    let first_line = preview.split('\n').next().unwrap_or("").to_string();
    let mut fields = Vec::new();
    if let Some(source) = &item.source {
        fields.push(fold(&source.name));
    }
    for key in ["host", "hex"] {
        if let Some(text) = item.meta_str(key) {
            fields.push(fold(text));
        }
    }
    fields.extend(item.meta_names().iter().map(|name| fold(name)));
    let names = boards
        .iter()
        .filter(|board| item.boards.contains(&board.id))
        .map(|board| fold(&board.name))
        .collect();
    Haystack {
        preview,
        first_line,
        fields,
        boards: names,
        kind: item.kind.as_str(),
    }
}

/// The points one term earns in an item, or `None` when it matches nowhere.
fn term_points(term: &str, hay: &Haystack) -> Option<u32> {
    let mut best: Option<u32> = None;
    let mut give = |points: u32| best = Some(best.map_or(points, |old| old.max(points)));
    if hay.first_line.starts_with(term) {
        give(40);
    } else if hay.preview.match_indices(term).any(|(at, _)| {
        hay.preview[..at]
            .chars()
            .next_back()
            .is_none_or(|before| !before.is_alphanumeric())
    }) {
        give(25);
    } else if hay.preview.contains(term) {
        give(15);
    }
    if hay.fields.iter().any(|field| field.contains(term)) {
        give(20);
    }
    if hay.boards.iter().any(|name| name.contains(term)) {
        give(10);
    }
    if hay.kind.contains(term) {
        give(0);
    }
    best
}

fn passes(item: &Item, boards: &[Board], filters: &Filters) -> bool {
    if !filters.types.is_empty() && !filters.types.contains(&item.kind) {
        return false;
    }
    if filters.pinned && !item.pinned {
        return false;
    }
    if filters.before.is_some_and(|at| item.created >= at)
        || filters.after.is_some_and(|at| item.created < at)
    {
        return false;
    }
    let source_has = |wanted: &str| {
        item.source
            .as_ref()
            .is_some_and(|app| fold(&app.id).contains(wanted) || fold(&app.name).contains(wanted))
    };
    if !filters.apps.iter().all(|wanted| source_has(wanted)) {
        return false;
    }
    if !filters.app_ids.is_empty()
        && !item.source.as_ref().is_some_and(|app| {
            filters
                .app_ids
                .iter()
                .any(|id| id.eq_ignore_ascii_case(&app.id))
        })
    {
        return false;
    }
    filters.boards.iter().all(|wanted| {
        boards
            .iter()
            .any(|board| item.boards.contains(&board.id) && fold(&board.name).contains(wanted))
    })
}

/// The indexes into `items` of what the view and the query list, best first: with an empty
/// query by `used` (a board in its own order); with a query by score, then `used`. `limit`
/// cuts the list.
pub fn search(
    items: &[Item],
    boards: &[Board],
    view: &View,
    query: &Query,
    now: f64,
    limit: usize,
) -> Vec<usize> {
    let in_view = |item: &Item| match view {
        View::History => true,
        View::Pinned => item.pinned,
        View::Board(id) => item.boards.contains(id),
    };
    let mut found: Vec<(usize, f64)> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if !in_view(item) || !passes(item, boards, &query.filters) {
            continue;
        }
        if query.terms.is_empty() {
            found.push((index, 0.0));
            continue;
        }
        let hay = haystack(item, boards);
        let mut score = 0.0f64;
        let mut all = true;
        for term in &query.terms {
            match term_points(term, &hay) {
                Some(points) => score += f64::from(points),
                None => {
                    all = false;
                    break;
                }
            }
        }
        if !all {
            continue;
        }
        if item.pinned {
            score += 10.0;
        }
        let age_days = ((now - item.used) / 86_400.0).max(0.0);
        score += 10.0 * (-age_days / 14.0).exp();
        found.push((index, score));
    }
    if query.terms.is_empty() {
        found.sort_by(|a, b| items[b.0].used.total_cmp(&items[a.0].used));
        if let View::Board(id) = view
            && let Some(board) = boards.iter().find(|board| board.id == *id)
        {
            // The board's own order first; the rest after, by use.
            let place = |index: usize| {
                board
                    .item_order
                    .iter()
                    .position(|listed| *listed == items[index].id)
                    .unwrap_or(usize::MAX)
            };
            found.sort_by_key(|(index, _)| place(*index));
        }
    } else {
        found.sort_by(|a, b| {
            b.1.total_cmp(&a.1)
                .then(items[b.0].used.total_cmp(&items[a.0].used))
        });
    }
    found.truncate(limit);
    found.into_iter().map(|(index, _)| index).collect()
}

// ---- relative time -----------------------------------------------------------------------

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// How long ago `used` was, in the words of the spec's table (7.13), at `now`; `offset_secs`
/// is the local time zone's offset from UTC.
pub fn relative(used: f64, now: f64, offset_secs: i64) -> String {
    let age = (now - used).max(0.0);
    if age < 60.0 {
        return "Just now".to_string();
    }
    if age < 3600.0 {
        return format!("{} min", (age / 60.0).floor() as u64);
    }
    if age < 86_400.0 {
        return format!("{} h", (age / 3600.0).floor() as u64);
    }
    let day_of = |at: f64| (at as i64 + offset_secs).div_euclid(86_400);
    let (then, today) = (day_of(used), day_of(now));
    if today - then == 1 {
        return "Yesterday".to_string();
    }
    if age < 7.0 * 86_400.0 {
        return WEEKDAYS[(then + 4).rem_euclid(7) as usize].to_string();
    }
    let (year, month, day) = rightkit_time::civil_from_days(then);
    let (this_year, _, _) = rightkit_time::civil_from_days(today);
    let name = MONTHS[(month - 1).clamp(0, 11) as usize];
    if year == this_year {
        format!("{day} {name}")
    } else {
        format!("{day} {name} {year}")
    }
}
