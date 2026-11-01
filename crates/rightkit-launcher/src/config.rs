//! The launcher's user-made items (the Mac's `LauncherConfig`): one JSON text the hub writes
//! through the `launcherConfig` key. Read leniently: a missing or malformed text is an empty
//! config, an unknown member is kept as it is when the notch writes the text back (pins).
//!
//! Honoured here: `pinnedApps`, `quicklinks`, `snippets`, `fileFolders` (`launcher_files.rs`),
//! `commands` (`launcher_commands.rs`), `notesEnabled` (`notes_off`)
//! and `appHotkeys` (`app_hotkeys`, held by `launcher.rs`).

use crate::json::{self, Value};

/// The largest config text accepted (the settings file itself is capped at 64 KiB).
pub const MAX_BYTES: usize = 32 * 1024;
/// How deep `{snippet:Name}` may nest, as on the Mac.
const MAX_NESTING: usize = 8;

/// A URL, search or deep link with placeholders: `{query}` (the text after the keyword),
/// `{clipboard}` and `{date}`.
#[derive(Clone)]
pub struct Quicklink {
    pub name: String,
    pub keyword: String,
    pub template: String,
}

/// Text pasted into the app that was in front. Placeholders: `{date}`, `{clipboard}`,
/// `{argument}`, and `{snippet:Name}` to include another snippet by name or keyword.
#[derive(Clone)]
pub struct Snippet {
    pub name: String,
    pub keyword: String,
    pub body: String,
}

/// A program with its own shortcut (the Mac's `LauncherAppHotkey`). `hotkey` is such as
/// "opt+1" or "cmd+shift+k"; both parts are needed.
#[derive(Clone, Debug, PartialEq)]
pub struct AppHotkey {
    pub path: String,
    pub hotkey: String,
}

#[derive(Default, Clone)]
pub struct Config {
    pub pinned: Vec<String>,
    pub quicklinks: Vec<Quicklink>,
    pub snippets: Vec<Snippet>,
    /// Whether "notes" and "note" work in the launcher. On unless the hub turned it off.
    pub notes_off: bool,
    /// Folders whose file names the launcher searches; empty turns file search off.
    pub file_folders: Vec<String>,
    /// Command lines run from the launcher, their output shown in it.
    pub commands: Vec<crate::commands::Command>,
    /// Programs with a shortcut of their own; rows missing a path or a shortcut are skipped.
    pub app_hotkeys: Vec<AppHotkey>,
}

fn text_of(item: &Value, key: &str) -> String {
    item.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn list<'a>(root: &'a Value, key: &str) -> &'a [Value] {
    root.get(key).and_then(Value::as_array).unwrap_or_default()
}

/// The config in `text`; empty when the text is missing or malformed.
pub fn parse(text: &str) -> Config {
    let Some(root) = json::parse(text.as_bytes(), MAX_BYTES) else {
        return Config::default();
    };
    Config {
        pinned: list(&root, "pinnedApps")
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        quicklinks: list(&root, "quicklinks")
            .iter()
            .map(|item| Quicklink {
                name: text_of(item, "name"),
                keyword: text_of(item, "keyword"),
                template: text_of(item, "template"),
            })
            .collect(),
        snippets: list(&root, "snippets")
            .iter()
            .map(|item| Snippet {
                name: text_of(item, "name"),
                keyword: text_of(item, "keyword"),
                body: text_of(item, "body"),
            })
            .collect(),
        notes_off: matches!(root.get("notesEnabled"), Some(Value::Bool(false))),
        file_folders: list(&root, "fileFolders")
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .filter(|folder| !folder.is_empty())
            .collect(),
        commands: crate::commands::parse(&root),
        app_hotkeys: list(&root, "appHotkeys")
            .iter()
            .map(|item| AppHotkey {
                path: text_of(item, "path"),
                hotkey: text_of(item, "hotkey"),
            })
            .filter(|row| !row.path.is_empty() && !row.hotkey.is_empty())
            .collect(),
    }
}

/// True when `text` is something the notch will store: empty, or a JSON object within the cap.
pub fn acceptable(text: &str) -> bool {
    text.is_empty()
        || (text.len() <= MAX_BYTES
            && matches!(
                json::parse(text.as_bytes(), MAX_BYTES),
                Some(Value::Object(_))
            ))
}

pub fn quoted(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn encode(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Number(number) if number.is_finite() => out.push_str(&number.to_string()),
        Value::Number(_) => out.push_str("null"),
        Value::Text(text) => out.push_str(&quoted(text)),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                encode(item, out);
            }
            out.push(']');
        }
        Value::Object(members) => {
            out.push('{');
            for (index, (key, member)) in members.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&quoted(key));
                out.push(':');
                encode(member, out);
            }
            out.push('}');
        }
    }
}

/// `text` with its `pinnedApps` replaced by `pins`, every other member kept. `None` when the
/// result would not fit.
pub fn with_pins(text: &str, pins: &[String]) -> Option<String> {
    let mut members = match json::parse(text.as_bytes(), MAX_BYTES) {
        Some(Value::Object(members)) => members,
        _ => Vec::new(),
    };
    let list = Value::Array(pins.iter().cloned().map(Value::Text).collect());
    match members.iter_mut().find(|(key, _)| key == "pinnedApps") {
        Some((_, slot)) => *slot = list,
        None => members.push(("pinnedApps".to_string(), list)),
    }
    let mut out = String::new();
    encode(&Value::Object(members), &mut out);
    (out.len() <= MAX_BYTES).then_some(out)
}

/// Fills `{name}` placeholders in one pass, so inserted text is never read again. A name
/// that has no value stays as written.
pub fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut output = String::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        output.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            output.push_str(&rest[open..]);
            return output;
        };
        let name = &after[..close];
        match values.iter().find(|(key, _)| *key == name) {
            Some((_, value)) => output.push_str(value),
            None => output.push_str(&rest[open..open + close + 2]),
        }
        rest = &after[close + 1..];
    }
    output.push_str(rest);
    output
}

/// Percent-encodes a value for a URL query: only unreserved characters pass.
pub fn encode_query(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `body` with each `{snippet:Name}` replaced by that snippet's own (expanded) body. A
/// snippet that would include itself, an unknown name, or nesting past eight levels is left
/// as written.
pub fn expand(body: &str, snippets: &[Snippet], visiting: &[String]) -> String {
    const MARKER: &str = "{snippet:";
    if visiting.len() >= MAX_NESTING {
        return body.to_string();
    }
    let mut output = String::new();
    let mut rest = body;
    while let Some(start) = rest.find(MARKER) {
        output.push_str(&rest[..start]);
        let after = &rest[start + MARKER.len()..];
        let Some(close) = after.find('}') else {
            output.push_str(&rest[start..]);
            return output;
        };
        let key = after[..close].trim().to_lowercase();
        let target = snippets
            .iter()
            .find(|s| s.name.to_lowercase() == key || s.keyword.to_lowercase() == key)
            .filter(|_| !key.is_empty() && !visiting.contains(&key));
        match target {
            Some(target) => {
                let mut deeper = visiting.to_vec();
                deeper.push(key);
                output.push_str(&expand(&target.body, snippets, &deeper));
            }
            None => output.push_str(&rest[start..start + MARKER.len() + close + 1]),
        }
        rest = &after[close + 1..];
    }
    output.push_str(rest);
    output
}
