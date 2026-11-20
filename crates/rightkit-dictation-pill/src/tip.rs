//! The one-time "tip" cadence of a dictation pill: count the deliveries that qualify (the target
//! application matches the host's needle lists), persist the counter, and say when the tip card is
//! due. Plus the hint-catalog reader that turns a catalog entry into card text.
//!
//! Nothing here names a product, a file, a key, an event, a tip or a word: the threshold, the
//! needle lists, the JSON keys, the size bounds, the catalog vocabulary, the event names and the
//! atomic file writer all come from the host. The persisted file is a JSON object holding a count
//! and a seen flag under the host's keys (pretty-printed, keys sorted), so a file an earlier host
//! build wrote stays readable.

use std::io::Read;
use std::path::Path;

use serde_json::{json, Map, Value};

/// Lower-case substrings of the target's process name and window title. Empty lists match nothing.
#[derive(Clone, Copy, Debug)]
pub struct TargetNeedles<'a> {
    /// Matches in `process + " " + title` alone.
    pub any: &'a [&'a str],
    /// Process names that count only when the title matches `terminal_title`.
    pub terminal_or_editor: &'a [&'a str],
    pub terminal_title: &'a [&'a str],
    /// Browser process names; the title must then match `browser_title`.
    pub browsers: &'a [&'a str],
    pub browser_title: &'a [&'a str],
}

/// Everything the host decides about a tip.
#[derive(Clone, Copy, Debug)]
pub struct TipPolicy<'a> {
    /// The tip is offered on this many qualifying deliveries (once).
    pub threshold: u32,
    /// JSON key of the counter in the persisted file.
    pub count_key: &'a str,
    /// JSON key of the "already shown" flag in the persisted file.
    pub seen_key: &'a str,
    /// The counter file is a few dozen bytes; anything larger is not the host's and is read as
    /// absent.
    pub max_store_bytes: u64,
    /// Characters of a process name or window title that are examined.
    pub max_target_chars: usize,
    pub targets: TargetNeedles<'a>,
}

/// Event names a delivery report is sent under (`sink(name, detail)`).
#[derive(Clone, Copy, Debug)]
pub struct TipEvents<'a> {
    /// The counter file could not be written; the detail is the error kind.
    pub store_error: &'a str,
    /// A qualifying delivery was counted; the detail is `count=<n> outcome=<open|below_threshold>`.
    pub trigger: &'a str,
}

fn any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

fn lowered(value: Option<&str>, max_chars: usize) -> String {
    value
        .unwrap_or_default()
        .chars()
        .take(max_chars)
        .collect::<String>()
        .to_lowercase()
}

/// Substring matches on the lower-cased process name and window title.
pub fn is_likely_target(
    policy: &TipPolicy<'_>,
    process: Option<&str>,
    title: Option<&str>,
) -> bool {
    let process = lowered(process, policy.max_target_chars);
    let title = lowered(title, policy.max_target_chars);
    let haystack = format!("{process} {title}");
    if haystack.trim().is_empty() {
        return false;
    }
    let targets = &policy.targets;
    if any(&haystack, targets.any) {
        return true;
    }
    if any(&process, targets.terminal_or_editor) {
        return any(&title, targets.terminal_title);
    }
    if any(&process, targets.browsers) {
        return any(&title, targets.browser_title);
    }
    false
}

/// The persisted counter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TipStore {
    /// Qualifying deliveries seen so far.
    pub count: u32,
    /// The tip was already shown once.
    pub seen: bool,
}

impl TipStore {
    /// Record one delivery. Returns `true` when the tip should be offered now: the delivery
    /// qualifies, the tip was not shown before and the count reached the threshold.
    pub fn record(&mut self, policy: &TipPolicy<'_>, likely_target: bool) -> bool {
        if !likely_target || self.seen {
            return false;
        }
        self.count = self.count.saturating_add(1);
        self.count >= policy.threshold
    }

    /// Read a store from its JSON object. A missing or mistyped field reads as zero or false;
    /// unrelated keys are ignored.
    pub fn from_json(policy: &TipPolicy<'_>, value: &Value) -> TipStore {
        TipStore {
            count: value
                .get(policy.count_key)
                .and_then(Value::as_u64)
                .map_or(0, |count| count.min(u64::from(u32::MAX)) as u32),
            seen: value
                .get(policy.seen_key)
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }
    }

    /// The JSON object to persist.
    pub fn to_json(&self, policy: &TipPolicy<'_>) -> Value {
        let mut map = Map::new();
        map.insert(policy.count_key.to_string(), json!(self.count));
        map.insert(policy.seen_key.to_string(), json!(self.seen));
        Value::Object(map)
    }
}

/// An earlier implementation's tip state: the delivery count, whether the tip was shown, and
/// whether the user switched tips off.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LegacyTipState {
    pub count: u32,
    pub seen: bool,
    pub disabled: bool,
}

/// The store to write for a legacy state, or `None` when a store already exists and stays as it
/// is. A tip the user switched off or was already shown counts as seen, so an upgrade does not
/// offer it a second time.
pub fn legacy_import(existing: Option<TipStore>, legacy: LegacyTipState) -> Option<TipStore> {
    existing.is_none().then_some(TipStore {
        count: legacy.count,
        seen: legacy.seen || legacy.disabled,
    })
}

/// The most bytes of store file [`read_store`] ever reads, whatever the policy says.
pub const MAX_STORE_BYTES_LIMIT: u64 = 64 * 1024;

/// Read the store through one handle, at most `max_store_bytes` bytes: no metadata check followed
/// by a reopen, no read proportional to a file someone else made large. `None` when the file is
/// absent, unreadable, too large or not a JSON object.
pub fn read_store(policy: &TipPolicy<'_>, path: &Path) -> Option<TipStore> {
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    // Bounded by `MAX_STORE_BYTES_LIMIT` and never `+ 1` on an unbounded value.
    let cap = policy.max_store_bytes.min(MAX_STORE_BYTES_LIMIT);
    file.take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > cap {
        return None;
    }
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value
        .is_object()
        .then(|| TipStore::from_json(policy, &value))
}

/// Persist the store with the host's atomic file writer (`replace(path, bytes)`).
pub fn write_store(
    policy: &TipPolicy<'_>,
    path: &Path,
    store: &TipStore,
    replace: impl FnOnce(&Path, &[u8]) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(&store.to_json(policy))
        .map_err(|_| std::io::Error::other("serialize"))?;
    replace(path, &bytes)
}

/// One-time carry-over of an earlier implementation's tip state into `path`. Returns whether it
/// wrote.
pub fn import_legacy(
    policy: &TipPolicy<'_>,
    path: &Path,
    legacy: LegacyTipState,
    replace: impl FnOnce(&Path, &[u8]) -> std::io::Result<()>,
) -> bool {
    let existing = read_store(policy, path);
    // An unreadable file is not "absent": leave it for the normal flow to rewrite.
    if existing.is_none() && path.exists() {
        return false;
    }
    let Some(store) = legacy_import(existing, legacy) else {
        return false;
    };
    write_store(policy, path, &store, replace).is_ok()
}

/// Count one delivery and persist the counter. Returns `true` when the tip should be shown now.
/// The "seen" flag is persisted before the card is shown: the tip is offered at most once and
/// never repeated; a write failure is reported and the tip may then repeat or be late.
pub fn on_delivery_recorded(
    policy: &TipPolicy<'_>,
    path: &Path,
    process: Option<&str>,
    title: Option<&str>,
    replace: impl FnOnce(&Path, &[u8]) -> std::io::Result<()>,
    events: TipEvents<'_>,
    sink: impl Fn(&str, &str),
) -> bool {
    if !is_likely_target(policy, process, title) {
        return false;
    }
    let mut store = read_store(policy, path).unwrap_or_default();
    let show = store.record(policy, true);
    if show {
        store.seen = true;
    }
    if let Err(error) = write_store(policy, path, &store, replace) {
        sink(events.store_error, &error.kind().to_string());
    }
    sink(
        events.trigger,
        &format!(
            "count={} outcome={}",
            store.count,
            if show { "open" } else { "below_threshold" }
        ),
    );
    show
}

/// The host's catalog vocabulary: `{ "<kind>": { "title": .., "body": .., "platform": .. } }`
/// where title and body are a string or an object keyed by platform name. Keys starting with
/// `skip_prefix` are not entries; a `platform` other than `any_platform` or `platform` hides the
/// entry.
#[derive(Clone, Copy, Debug)]
pub struct CatalogSchema<'a> {
    /// The platform this host is: the key of a platform-specific title or body, and a value of an
    /// entry's `platform` that shows it.
    pub platform: &'a str,
    /// The value of an entry's `platform` that shows it everywhere.
    pub any_platform: &'a str,
    /// A key starting with this is a comment, not an entry.
    pub skip_prefix: char,
}

fn catalog_text(schema: &CatalogSchema<'_>, value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Object(_) => value
            .get(schema.platform)
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => None,
    }
}

/// The largest catalog text [`catalog_entry`] parses; a larger one has no entries.
pub const MAX_CATALOG_BYTES: usize = 1024 * 1024;

/// Title and body of the catalog entry for `kind`, with this platform's variant selected; `None`
/// for a key that is a comment, an entry scoped to another platform, an entry without both texts
/// and an unknown kind.
pub fn catalog_entry(
    schema: &CatalogSchema<'_>,
    catalog_json: &str,
    kind: &str,
) -> Option<(String, String)> {
    if kind.starts_with(schema.skip_prefix) {
        return None;
    }
    if catalog_json.len() > MAX_CATALOG_BYTES {
        return None;
    }
    let root: Value = serde_json::from_str(catalog_json).ok()?;
    let entry = root.get(kind)?;
    if let Some(platform) = entry.get("platform").and_then(Value::as_str) {
        if platform != schema.any_platform && platform != schema.platform {
            return None;
        }
    }
    Some((
        catalog_text(schema, entry.get("title")?)?,
        catalog_text(schema, entry.get("body")?)?,
    ))
}
