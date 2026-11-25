//! The command folder a settings owner (a tray or notch app) watches for changes other processes
//! ask for, and the helpers a settings CLI needs around it. One JSON file per command, written
//! whole under a temporary name and renamed into place, then the owner is woken; the owner writes
//! a typed answer to `results/<stem>.json`. The app names the folder and the wake-up; nothing here
//! knows a product.
//!
//! Also here: the value helpers a settings CLI shares (type-checked parsing against the value the
//! owner publishes now, equality that treats `3` and `3.0` alike, and close-match suggestions for
//! a misspelt key).

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Why handing the owner a command failed.
#[derive(Debug)]
pub enum SendError {
    /// The command folder could not be created.
    Folder(std::io::Error),
    /// The command file could not be written.
    Write(std::io::Error),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Folder(e) | Self::Write(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for SendError {}

/// A command folder (for example `<state folder>/hub-commands`).
#[derive(Clone, Debug)]
pub struct CommandFolder {
    dir: PathBuf,
}

impl CommandFolder {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The folder itself.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Writes `body` as `<nanoseconds since the epoch>.json`, whole, through an atomic replace,
    /// creating the folder first; returns the stem the owner answers under. The caller wakes the
    /// owner afterwards ([`wake_owner`]).
    pub fn send(&self, body: &Value) -> Result<String, SendError> {
        std::fs::create_dir_all(&self.dir).map_err(SendError::Folder)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let bytes = serde_json::to_vec(body)
            .map_err(|e| SendError::Write(std::io::Error::other(e.to_string())))?;
        crate::atomic_write(&self.dir.join(format!("{stamp}.json")), &bytes)
            .map_err(SendError::Write)?;
        Ok(stamp.to_string())
    }

    /// Polls `results/<stem>.json` every `poll` for up to `timeout`; returns the owner's `status`
    /// and `reason` once it is written, `None` when the owner gave no typed answer in time.
    pub fn wait_for_result(
        &self,
        stem: &str,
        timeout: Duration,
        poll: Duration,
    ) -> Option<(String, Option<String>)> {
        let file = self.dir.join("results").join(format!("{stem}.json"));
        let deadline = Instant::now() + timeout;
        loop {
            let body = std::fs::read(&file)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
            if let Some(status) = body.as_ref().and_then(|b| b["status"].as_str()) {
                return Some((
                    status.to_string(),
                    body.as_ref()
                        .and_then(|b| b["reason"].as_str())
                        .map(str::to_string),
                ));
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(poll);
        }
    }
}

/// Looks at the published state file at `path` every `poll` for up to `timeout` until
/// `settings[key]` shows `wanted` (numbers compare by value). One read-only view of the file is
/// kept, which re-reads only when the file's stamp changed; a view that cannot be opened yet (file
/// absent or damaged) is tried again on the next look. `namespace` is the view's schema namespace.
pub fn wait_for_setting(
    path: &Path,
    namespace: &str,
    key: &str,
    wanted: &Value,
    timeout: Duration,
    poll: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    let mut view = None;
    loop {
        if view.is_none() {
            view = crate::open_published_state(path, namespace).ok().flatten();
        }
        if let Some(store) = &view {
            let shown = store.get()["settings"][key].clone();
            if !shown.is_null() && same_value(&shown, wanted) {
                return true;
            }
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(poll);
    }
}

/// Wakes the owner after a command was written. On macOS it posts the Darwin notification
/// `mac_notification`; on Windows it signals the auto-reset event `windows_event` (for example
/// `Local\<id>.command`), creating it when absent so an owner that starts later still sees the
/// request, then closes the handle. Elsewhere it does nothing.
#[allow(unused_variables)]
pub fn wake_owner(windows_event: &str, mac_notification: &str) {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn notify_post(name: *const std::ffi::c_char) -> u32;
        }
        if let Ok(name) = std::ffi::CString::new(mac_notification) {
            // SAFETY: `name` is a NUL-terminated string that outlives the call.
            unsafe { notify_post(name.as_ptr()) };
        }
    }
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn CreateEventW(
                attributes: *const std::ffi::c_void,
                manual_reset: i32,
                initial_state: i32,
                name: *const u16,
            ) -> *mut std::ffi::c_void;
            fn SetEvent(event: *mut std::ffi::c_void) -> i32;
            fn CloseHandle(object: *mut std::ffi::c_void) -> i32;
        }
        let name: Vec<u16> = windows_event
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: NUL-terminated name; the handle is closed straight after the signal.
        unsafe {
            let event = CreateEventW(std::ptr::null(), 0, 0, name.as_ptr());
            if !event.is_null() {
                SetEvent(event);
                CloseHandle(event);
            }
        }
    }
}

/// A value as a person reads it: text bare, a list joined with commas, anything else as JSON.
pub fn show_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items.iter().map(show_value).collect::<Vec<_>>().join(", "),
        other => other.to_string(),
    }
}

/// Parses `text` into the same JSON type as `current`, the value the owner publishes now. Lists
/// are comma-separated ids; a list of anything but text, a null or an object cannot be set. The
/// error continues the sentence "`<key>` ...".
pub fn parse_value_like(text: &str, current: &Value) -> Result<Value, String> {
    let trimmed = text.trim();
    match current {
        Value::Bool(_) => match trimmed.to_lowercase().as_str() {
            "true" | "on" => Ok(json!(true)),
            "false" | "off" => Ok(json!(false)),
            _ => Err(format!(
                "is a switch: give true or false (or on or off), not \"{trimmed}\""
            )),
        },
        Value::Number(number) if number.is_f64() => trimmed
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .ok_or_else(|| format!("is a number, and \"{trimmed}\" is not one")),
        Value::Number(_) => trimmed
            .parse::<i64>()
            .map(|whole| json!(whole))
            .map_err(|_| format!("is a whole number, and \"{trimmed}\" is not one")),
        Value::String(_) => Ok(json!(text)),
        Value::Array(published) => {
            if published.iter().any(|item| !item.is_string()) {
                return Err("is a list that the CLI can't set (its items are not text)".into());
            }
            let ids: Vec<Value> = text
                .split(',')
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(|id| Value::String(id.to_string()))
                .collect();
            Ok(Value::Array(ids))
        }
        Value::Null => {
            Err("has no value now, so there is no type to check a new value against".into())
        }
        Value::Object(_) => Err("is a group of values, not one value the CLI can set".into()),
    }
}

/// Equal as JSON values; numbers compare by value, so `3` and `3.0` match.
pub fn same_value(a: &Value, b: &Value) -> bool {
    if a.is_number() && b.is_number() {
        a.as_f64() == b.as_f64()
    } else {
        a == b
    }
}

/// Keys that contain or are contained in `wanted`, or are within three edits of it; closest
/// first, at most five.
pub fn close_matches<'a>(wanted: &str, keys: impl Iterator<Item = &'a String>) -> Vec<String> {
    let wanted = wanted.to_lowercase();
    let mut scored: Vec<(usize, String)> = keys
        .filter_map(|key| {
            let lower = key.to_lowercase();
            let related = lower.contains(wanted.as_str()) || wanted.contains(lower.as_str());
            let score = edit_distance(&wanted, &lower);
            (related || score <= 3).then(|| (score, key.clone()))
        })
        .collect();
    scored.sort_by_key(|(score, key)| (*score, key.clone()));
    scored.into_iter().take(5).map(|(_, key)| key).collect()
}

/// Edit distance between two strings (ASCII case ignored), one row at a time.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            let cost = usize::from(!ca.eq_ignore_ascii_case(cb));
            row[j + 1] = (above + 1).min(row[j] + 1).min(diagonal + cost);
            diagonal = above;
        }
    }
    row[b.len()]
}

// ------------------------------------------------------------------ command files

/// The value a command file carries.
#[derive(Clone, Debug, PartialEq)]
pub enum CommandArg {
    Null,
    Bool(bool),
    Number(f64),
    Text(String),
    List(Vec<String>),
}

/// One command file: `{"command":..,"key":..,"provider":..,"value":..}`.
#[derive(Clone, Debug, PartialEq)]
pub struct HubCommand {
    pub name: String,
    pub key: Option<String>,
    pub provider: Option<String>,
    pub value: CommandArg,
}

/// The bounds [`parse_hub_command`] enforces on a command file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandLimits {
    /// Largest file, in bytes.
    pub max_bytes: usize,
    /// Most members in one object or elements in one array, at any depth.
    pub max_elements: usize,
    /// Deepest container nesting (the root is depth 0).
    pub max_depth: usize,
    /// Most items in a text list value.
    pub max_list_items: usize,
    /// Longest text item in a list value, in bytes.
    pub max_item_bytes: usize,
}

fn within_bounds(value: &Value, depth: usize, limits: &CommandLimits) -> bool {
    depth <= limits.max_depth
        && match value {
            Value::Array(items) => {
                items.len() <= limits.max_elements
                    && items
                        .iter()
                        .all(|item| within_bounds(item, depth + 1, limits))
            }
            Value::Object(members) => {
                members.len() <= limits.max_elements
                    && members
                        .values()
                        .all(|member| within_bounds(member, depth + 1, limits))
            }
            _ => true,
        }
}

/// A list of short ids with repeats dropped; `None` when an item is not text, is too long, or the
/// list is longer than `limits.max_list_items`.
fn command_text_list(items: &[Value], limits: &CommandLimits) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for item in items {
        let text = item.as_str()?;
        if text.len() > limits.max_item_bytes || out.len() >= limits.max_list_items {
            return None;
        }
        if !out.iter().any(|seen| seen == text) {
            out.push(text.to_string());
        }
    }
    Some(out)
}

/// Parses one command file (a byte order mark is tolerated); `None` when it is oversized, repeats
/// a key anywhere, exceeds the nesting or element bounds, or is not an object with a `command`
/// text. A `value` of another shape reads as [`CommandArg::Null`].
pub fn parse_hub_command(bytes: &[u8], limits: &CommandLimits) -> Option<HubCommand> {
    if bytes.len() > limits.max_bytes || has_duplicate_keys(bytes) {
        return None;
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let root: Value = serde_json::from_slice(bytes).ok()?;
    if !within_bounds(&root, 0, limits) {
        return None;
    }
    let Value::Object(members) = root else {
        return None;
    };
    let text = |name: &str| {
        members
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let value = match members.get("value") {
        Some(Value::Bool(value)) => CommandArg::Bool(*value),
        Some(Value::Number(number)) => number.as_f64().map_or(CommandArg::Null, CommandArg::Number),
        Some(Value::String(text)) => CommandArg::Text(text.clone()),
        Some(Value::Array(items)) => {
            command_text_list(items, limits).map_or(CommandArg::Null, CommandArg::List)
        }
        _ => CommandArg::Null,
    };
    Some(HubCommand {
        name: text("command")?,
        key: text("key"),
        provider: text("provider"),
        value,
    })
}

/// Whether any object in the JSON document repeats a key, at any depth. Text that is not JSON is
/// not this check's concern (a reader refuses it); a byte order mark is tolerated.
pub fn has_duplicate_keys(bytes: &[u8]) -> bool {
    use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
    struct Scan(bool);
    impl<'de> Deserialize<'de> for Scan {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct Any;
            impl<'de> Visitor<'de> for Any {
                type Value = Scan;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("JSON")
                }
                fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Scan, A::Error> {
                    let mut seen = std::collections::BTreeSet::new();
                    let mut repeated = false;
                    while let Some(key) = map.next_key::<String>()? {
                        repeated |= !seen.insert(key);
                        repeated |= map.next_value::<Scan>()?.0;
                    }
                    Ok(Scan(repeated))
                }
                fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Scan, A::Error> {
                    let mut repeated = false;
                    while let Some(item) = seq.next_element::<Scan>()? {
                        repeated |= item.0;
                    }
                    Ok(Scan(repeated))
                }
                fn visit_bool<E>(self, _: bool) -> Result<Scan, E> {
                    Ok(Scan(false))
                }
                fn visit_i64<E>(self, _: i64) -> Result<Scan, E> {
                    Ok(Scan(false))
                }
                fn visit_u64<E>(self, _: u64) -> Result<Scan, E> {
                    Ok(Scan(false))
                }
                fn visit_f64<E>(self, _: f64) -> Result<Scan, E> {
                    Ok(Scan(false))
                }
                fn visit_str<E>(self, _: &str) -> Result<Scan, E> {
                    Ok(Scan(false))
                }
                fn visit_unit<E>(self) -> Result<Scan, E> {
                    Ok(Scan(false))
                }
            }
            deserializer.deserialize_any(Any)
        }
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    serde_json::from_slice::<Scan>(bytes).is_ok_and(|scan| scan.0)
}
