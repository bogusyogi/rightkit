//! The launcher's notes. Each note is a Markdown file in `launcher\notes` under the
//! application's folder; its title is its first line of text. A note opens in the
//! program Windows has for the file, which is where it is edited. A deleted note moves
//! to `notes\trash` with the time in its name and is removed for good 30 days later. Pinned
//! notes list first; which are pinned is kept in `notes\notes-index.json` beside them (a file
//! cannot carry it), as the Mac's `notes-index.json` does.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// A deleted note is kept this long.
const KEPT_SECONDS: u64 = 30 * 24 * 60 * 60;
/// Notes looked at in one listing; more than this is not a list to read in a launcher.
const MOST: usize = 500;
/// Bytes of a note read to find its title or a searched word.
const READ: u64 = 64 * 1024;
const TITLE_CHARS: usize = 90;
/// The longest text a new note may start with.
pub const NEW_MAX: usize = 16 * 1024;

pub struct Note {
    pub path: PathBuf,
    pub title: String,
    /// When it was last written (or, in the trash, deleted): seconds since 1970.
    pub at: u64,
    /// Listed before the others (never true in the trash).
    pub pinned: bool,
}

/// The sidecar that keeps which notes are pinned, by file name.
const INDEX: &str = "notes-index.json";

/// The file names of the pinned notes.
fn pinned_names(base: &Path) -> Vec<String> {
    let Ok(bytes) = fs::read(folder(base, false).join(INDEX)) else {
        return Vec::new();
    };
    crate::json::parse(&bytes, READ as usize)
        .and_then(|root| {
            root.get("pinned")?.as_array().map(|names| {
                names
                    .iter()
                    .filter_map(|name| name.as_str().map(str::to_string))
                    .collect()
            })
        })
        .unwrap_or_default()
}

fn save_pinned(base: &Path, names: &[String]) -> bool {
    let mut out = String::from("{\"pinned\":[");
    for (position, name) in names.iter().enumerate() {
        if position > 0 {
            out.push(',');
        }
        out.push('"');
        for c in name.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                c if (c as u32) < 0x20 => out.push(' '),
                c => out.push(c),
            }
        }
        out.push('"');
    }
    out.push_str("]}\n");
    let dir = folder(base, false);
    fs::create_dir_all(&dir).is_ok()
        && rightkit_fs::atomic_write(&dir.join(INDEX), out.as_bytes()).is_ok()
}

/// Pins or unpins a note (Alt+P in the launcher; the Mac's Command-P). True once it is kept.
pub fn set_pinned(base: &Path, note: &Path, pinned: bool) -> bool {
    let Some(name) = note.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if note.parent() != Some(folder(base, false).as_path()) {
        return false;
    }
    let mut names = pinned_names(base);
    let was = names.iter().any(|kept| kept == name);
    if was == pinned {
        return true;
    }
    if pinned {
        names.push(name.to_string());
    } else {
        names.retain(|kept| kept != name);
    }
    save_pinned(base, &names)
}

fn folder(base: &Path, trash: bool) -> PathBuf {
    let notes = base.join("notes");
    if trash { notes.join("trash") } else { notes }
}

/// The start of a note's text, at most `READ` bytes.
fn head(path: &Path) -> String {
    let mut text = Vec::new();
    if let Ok(file) = fs::File::open(path) {
        let _ = file.take(READ).read_to_end(&mut text);
    }
    String::from_utf8_lossy(&text).into_owned()
}

/// The first line with text in it, without Markdown's heading marks.
fn title_of(text: &str) -> String {
    let line = text
        .lines()
        .map(|line| line.trim().trim_start_matches('#').trim())
        .find(|line| !line.is_empty())
        .unwrap_or("Empty note");
    line.chars().take(TITLE_CHARS).collect()
}

/// When a trashed note was deleted: the number its file name starts with.
fn deleted_at(path: &Path) -> Option<u64> {
    let name = path.file_name()?.to_str()?;
    name.split('-').next()?.parse().ok()
}

/// The notes under `base` (the trash's with `trash`), pinned ones first and then newest first.
/// With `query`, only those whose text holds it.
pub fn list(base: &Path, trash: bool, query: &str) -> Vec<Note> {
    let wanted = query.trim().to_lowercase();
    let pinned = if trash {
        Vec::new()
    } else {
        pinned_names(base)
    };
    let Ok(entries) = fs::read_dir(folder(base, trash)) else {
        return Vec::new();
    };
    let mut notes: Vec<Note> = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|e| e == "md"))
        .take(MOST)
        .filter_map(|entry| {
            let path = entry.path();
            let text = head(&path);
            if !wanted.is_empty() && !text.to_lowercase().contains(&wanted) {
                return None;
            }
            let written = entry
                .metadata()
                .ok()
                .and_then(|meta| meta.modified().ok())
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |since| since.as_secs());
            let at = if trash {
                deleted_at(&path).unwrap_or(written)
            } else {
                written
            };
            let is_pinned = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| pinned.iter().any(|kept| kept == name));
            Some(Note {
                title: title_of(&text),
                path,
                at,
                pinned: is_pinned,
            })
        })
        .collect();
    notes.sort_by_key(|note| (std::cmp::Reverse(note.pinned), std::cmp::Reverse(note.at)));
    notes
}

/// A file name for a note: the time, then the title's letters and digits.
fn file_name(title: &str, now: u64) -> String {
    let mut slug = String::new();
    for c in title.chars().take(40) {
        if c.is_alphanumeric() {
            slug.extend(c.to_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        format!("{now}.md")
    } else {
        format!("{now}-{slug}.md")
    }
}

/// Makes a note that starts with `text` and returns its file. `None` when the folder or
/// the file cannot be made, or one of that name is already there.
pub fn create(base: &Path, text: &str, now: u64) -> Option<PathBuf> {
    let dir = folder(base, false);
    fs::create_dir_all(&dir).ok()?;
    let text: String = text.chars().take(NEW_MAX).collect();
    let path = dir.join(file_name(&title_of(&text), now));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .ok()?;
    let body = if text.is_empty() {
        String::new()
    } else {
        format!("{text}\n")
    };
    std::io::Write::write_all(&mut file, body.as_bytes()).ok()?;
    Some(path)
}

/// Moves a note to the trash. Nothing already in the trash is replaced.
pub fn delete(base: &Path, note: &Path, now: u64) -> bool {
    let trash = folder(base, true);
    let Some(name) = note.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if note.parent() != Some(folder(base, false).as_path()) || fs::create_dir_all(&trash).is_err() {
        return false;
    }
    let target = trash.join(format!("{now}-{name}"));
    let moved = rightkit_fs::rename_exclusive(note, &target).is_ok();
    // A deleted note is no longer pinned.
    if moved {
        set_pinned(base, note, false);
    }
    moved
}

/// Puts a trashed note back among the notes. A note of the same name is not replaced.
pub fn restore(base: &Path, trashed: &Path) -> bool {
    let Some(name) = trashed.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if trashed.parent() != Some(folder(base, true).as_path()) {
        return false;
    }
    // The name it had before the deletion time was put in front.
    let original = match (deleted_at(trashed), name.split_once('-')) {
        (Some(_), Some((_, rest))) => rest,
        _ => name,
    };
    let target = folder(base, false).join(original);
    rightkit_fs::rename_exclusive(trashed, &target).is_ok()
}

/// Removes for good the trashed notes deleted more than 30 days before `now`.
pub fn purge(base: &Path, now: u64) {
    let Ok(entries) = fs::read_dir(folder(base, true)) else {
        return;
    };
    for entry in entries.flatten().take(MOST) {
        let path = entry.path();
        let old = deleted_at(&path).is_some_and(|at| now.saturating_sub(at) > KEPT_SECONDS);
        if old && path.extension().is_some_and(|e| e == "md") {
            let _ = fs::remove_file(path);
        }
    }
}

/// "5 min ago", "3 h ago", "2 days ago" for a time `at` before `now`.
pub fn ago(at: u64, now: u64) -> String {
    let seconds = now.saturating_sub(at);
    match seconds {
        0..=59 => crate::host::t("just now").to_string(),
        60..=3599 => {
            let minutes = crate::host::tf("{} min", &[&(seconds / 60).to_string()]);
            crate::host::tf("{} ago", &[&minutes])
        }
        3600..=86_399 => {
            let hours = crate::host::tf("{} h", &[&(seconds / 3600).to_string()]);
            crate::host::tf("{} ago", &[&hours])
        }
        86_400..=172_799 => crate::host::t("1 day ago"),
        _ => crate::host::tf("{} days ago", &[&(seconds / 86_400).to_string()]),
    }
}
