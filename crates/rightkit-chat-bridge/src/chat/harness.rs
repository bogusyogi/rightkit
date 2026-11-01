//! Registered hook harnesses: the one place that knows which harnesses the host app lists besides
//! Claude and Codex, and where each keeps its sessions.
//!
//! Muse is the built-in registration (`muse.rs` discovers its sessions). Any other harness is
//! registered by its plugin install writing `<chat folder>/harnesses/<name>.json`:
//! `{"name": "<name>", "sessionsDir": "<absolute folder>"}`. the host app lists the `*.jsonl` files found
//! up to four levels under `sessionsDir` (a `session.jsonl` is named by its folder, any other file
//! by its stem) as that harness's chats, and reaches them through the hook queue (`deliver_hook`)
//! like Muse. A record without `sessionsDir` still works: its chats are listed once their hook has
//! registered them. The record keeps no history reader; `read` and `said` say so.

use super::muse::{self, MuseSession};
use super::{codex_hook, store::Store};
use std::path::{Path, PathBuf};

/// Most files one walk looks at, so a huge folder cannot stall the roster.
const WALK_FILES: usize = 5000;
const WALK_DEPTH: usize = 4;

/// The harness names the host app lists chats for: Muse, then every valid registration record.
pub fn kinds(store: &Store) -> Vec<String> {
    let mut kinds = vec![muse::KIND.to_string()];
    for entry in std::fs::read_dir(store.harnesses_dir())
        .into_iter()
        .flatten()
        .flatten()
    {
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if path.extension().is_some_and(|e| e == "json")
            && codex_hook::valid_hook_harness(stem)
            && !kinds.iter().any(|k| k == stem)
        {
            kinds.push(stem.to_string());
        }
    }
    kinds
}

/// Write the registration record of a hook harness whose hook has just run on this computer
/// (`{"name": "<kind>"}`, #434), so the harness stays registered after its process exits. Muse is
/// built in and never gets one. A record that already names a `sessionsDir` is kept. Otherwise
/// the folder is taken from the hook payload: the folder holding its transcript, else its working
/// folder (`cwd`), when either is an absolute folder. Best effort: a failed write leaves the chat
/// listed through its registration alone.
pub fn register(store: &Store, kind: &str, transcript: Option<&str>, cwd: Option<&str>) {
    if !codex_hook::valid_hook_harness(kind) {
        return;
    }
    let path = store.harnesses_dir().join(format!("{kind}.json"));
    let existing: Option<serde_json::Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    let names_dir = existing
        .as_ref()
        .and_then(|v| v["sessionsDir"].as_str())
        .is_some_and(|d| Path::new(d).is_absolute());
    if names_dir || rightkit_fs::private_cached::ensure_dir(&store.harnesses_dir()).is_err() {
        return;
    }
    let from_transcript = transcript
        .map(Path::new)
        .filter(|t| t.is_absolute() && t.is_file())
        .and_then(Path::parent);
    let from_cwd = cwd.map(Path::new).filter(|c| c.is_absolute() && c.is_dir());
    let mut record = serde_json::json!({ "name": kind });
    if let Some(dir) = from_transcript.or(from_cwd) {
        record["sessionsDir"] = serde_json::Value::String(dir.display().to_string());
    } else if existing.is_some() {
        return;
    }
    let _ = std::fs::write(&path, record.to_string());
}

/// The transcript of a registered harness's chat `id`, found by walking its `sessionsDir` the way
/// `list` does (`<id>.jsonl`, or `<id>/session.jsonl`). `None` for Muse, a chat not found, or a
/// harness whose record names no folder.
pub fn transcript_path(store: &Store, kind: &str, id: &str) -> Option<PathBuf> {
    if kind == muse::KIND || !codex_hook::safe_component(id) {
        return None;
    }
    let dir = sessions_dir(store, kind)?;
    find_transcript(&dir, 0, id, &mut 0)
}

fn find_transcript(dir: &Path, depth: usize, id: &str, seen: &mut usize) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if *seen >= WALK_FILES {
            return None;
        }
        *seen += 1;
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() && depth < WALK_DEPTH {
            if let Some(found) = find_transcript(&path, depth + 1, id, seen) {
                return Some(found);
            }
        } else if kind.is_file() && path.extension().is_some_and(|e| e == "jsonl") {
            let name = if path.file_name().is_some_and(|n| n == "session.jsonl") {
                path.parent().and_then(Path::file_name)
            } else {
                path.file_stem()
            };
            if name.and_then(|n| n.to_str()) == Some(id) {
                return Some(path);
            }
        }
    }
    None
}

/// The folder a registered harness keeps its sessions in, when its record names one.
fn sessions_dir(store: &Store, kind: &str) -> Option<PathBuf> {
    let text = std::fs::read_to_string(store.harnesses_dir().join(format!("{kind}.json"))).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    if value["name"].as_str() != Some(kind) {
        return None;
    }
    let dir = PathBuf::from(value["sessionsDir"].as_str()?);
    (dir.is_absolute() && dir.is_dir()).then_some(dir)
}

fn mtime_ms(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0))
}

fn walk(dir: &Path, depth: usize, seen: &mut usize, out: &mut Vec<MuseSession>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if *seen >= WALK_FILES {
            return;
        }
        *seen += 1;
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() && depth < WALK_DEPTH {
            walk(&path, depth + 1, seen, out);
        } else if kind.is_file() && path.extension().is_some_and(|e| e == "jsonl") {
            let id = if path.file_name().is_some_and(|n| n == "session.jsonl") {
                path.parent().and_then(Path::file_name)
            } else {
                path.file_stem()
            };
            if let Some(id) = id
                .and_then(|s| s.to_str())
                .filter(|s| codex_hook::safe_component(s))
            {
                out.push(MuseSession {
                    id: id.to_string(),
                    title: String::new(),
                    cwd: String::new(),
                    updated_ms: mtime_ms(&path),
                });
            }
        }
    }
}

/// The newest `limit` chats of `kind`, newest first.
pub fn list(store: &Store, kind: &str, limit: usize) -> Vec<MuseSession> {
    if kind == muse::KIND {
        return muse::list_sessions(limit);
    }
    let Some(dir) = sessions_dir(store, kind) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    walk(&dir, 0, &mut 0, &mut found);
    found.sort_by_key(|s| std::cmp::Reverse(s.updated_ms));
    found.truncate(limit);
    found
}

/// The chat of `kind` with exactly this id.
pub fn find(store: &Store, kind: &str, id: &str) -> Option<MuseSession> {
    if kind == muse::KIND {
        return muse::find_session(id);
    }
    list(store, kind, usize::MAX)
        .into_iter()
        .find(|s| s.id == id)
}
