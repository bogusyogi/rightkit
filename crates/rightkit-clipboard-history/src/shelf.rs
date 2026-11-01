//! The received-files shelf that outlives its card: up to `MAX_ITEMS` newest received files kept
//! in `nearby-shelf.json` in the host data folder (the Mac's `nearby-shelf.json`): path, sender
//! device name, received-at in unix milliseconds. Reading tolerates a missing or damaged file;
//! entries whose file is gone are dropped; every write is atomic. It never moves, deletes or runs
//! a file.

use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const FILE: &str = "nearby-shelf.json";
const MAX_ITEMS: usize = 20;
const MAX_BYTES: usize = 1024 * 1024;
/// A kept message is cut to this many bytes.
const TEXT_CAP: usize = 4 * 1024;

static GUARD: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub path: String,
    pub sender: String,
    /// Unix milliseconds.
    pub at_ms: u64,
    /// A received message (#467): its text, with an empty `path`.
    pub text: Option<String>,
}

fn json_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn read() -> Vec<Entry> {
    let Some(dir) = crate::host::data_dir() else {
        return Vec::new();
    };
    let Ok(bytes) = std::fs::read(dir.join(FILE)) else {
        return Vec::new();
    };
    let Some(root) = crate::json::parse(&bytes, MAX_BYTES) else {
        return Vec::new();
    };
    let list = root.get("items").and_then(|v| v.as_array());
    let mut out: Vec<Entry> = Vec::new();
    for item in list.unwrap_or_default() {
        let text = item
            .get("text")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let (Some(path), Some(sender), Some(at)) = (
            item.get("path").and_then(|v| v.as_str()),
            item.get("sender").and_then(|v| v.as_str()),
            item.get("at")
                .and_then(|v| v.as_f64())
                .filter(|n| *n >= 0.0),
        ) else {
            continue;
        };
        let entry = Entry {
            path: path.to_string(),
            sender: sender.to_string(),
            at_ms: at as u64,
            text,
        };
        let fresh = if entry.text.is_some() {
            entry.path.is_empty() && !out.iter().any(|e| e.text == entry.text)
        } else {
            Path::new(&entry.path).is_absolute()
                && !entry.path.chars().any(|c| c.is_control())
                && !out.iter().any(|e| e.path == entry.path)
        };
        if fresh {
            out.push(entry);
        }
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.at_ms));
    out.truncate(MAX_ITEMS);
    out
}

fn write(items: &[Entry]) {
    let Some(dir) = crate::host::data_dir() else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let rows: Vec<String> = items
        .iter()
        .map(|e| {
            let text = e
                .text
                .as_ref()
                .map(|t| format!(",\"text\":{}", json_text(t)))
                .unwrap_or_default();
            format!(
                "{{\"path\":{},\"sender\":{},\"at\":{}{}}}",
                json_text(&e.path),
                json_text(&e.sender),
                e.at_ms,
                text
            )
        })
        .collect();
    let body = format!("{{\"items\":[{}]}}", rows.join(","));
    let _ = rightkit_fs::atomic_write(&dir.join(FILE), body.as_bytes());
}

fn alive(items: Vec<Entry>) -> Vec<Entry> {
    items
        .into_iter()
        .filter(|e| e.text.is_some() || Path::new(&e.path).is_file())
        .collect()
}

/// The shelf, newest first, without entries whose file is gone (the file is rewritten when any
/// were dropped). The Send card's idle device list reads it.
pub fn list() -> Vec<Entry> {
    let _lock = GUARD.lock().unwrap_or_else(|p| p.into_inner());
    let stored = read();
    let kept = alive(stored.clone());
    if kept.len() != stored.len() {
        write(&kept);
    }
    kept
}

/// Adds files received from `sender` now, newest first, and returns the shelf.
pub fn record(files: &[String], sender: &str) -> Vec<Entry> {
    let _lock = GUARD.lock().unwrap_or_else(|p| p.into_inner());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let mut items: Vec<Entry> = files
        .iter()
        .filter(|p| Path::new(p.as_str()).is_absolute() && Path::new(p.as_str()).is_file())
        .map(|p| Entry {
            path: p.clone(),
            sender: sender.to_string(),
            at_ms: now,
            text: None,
        })
        .collect();
    for old in read() {
        if !items.iter().any(|e| e.path == old.path) {
            items.push(old);
        }
    }
    let mut kept = alive(items);
    kept.truncate(MAX_ITEMS);
    write(&kept);
    kept
}

/// Adds a received message from `sender` now, newest first (#467). Only a short head of the
/// text is kept: the tile shows a preview and a click copies what is kept.
pub fn record_text(text: &str, sender: &str) {
    if text.trim().is_empty() {
        return;
    }
    let _lock = GUARD.lock().unwrap_or_else(|p| p.into_inner());
    let mut end = text.len().min(TEXT_CAP);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let mut items = vec![Entry {
        path: String::new(),
        sender: sender.to_string(),
        at_ms: now,
        text: Some(text[..end].to_string()),
    }];
    for old in read() {
        if old.text.is_none() || old.text != items[0].text {
            items.push(old);
        }
    }
    let mut kept = alive(items);
    kept.truncate(MAX_ITEMS);
    write(&kept);
}
