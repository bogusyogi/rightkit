//! The Claude account book: the last usage reading of every Claude account this computer has seen,
//! kept by account id, and the list a hub's Accounts section shows.
//!
//! Only the signed-in account can be read live, so the others are shown from here: their last
//! percentages and when each window resets. Nothing is invented for them. Every account folder
//! Claude Desktop has is listed too, signed in or not, so it can be named before its first
//! reading. The book holds no secrets: ids, a sign-in address Claude Code already wrote in its own
//! config, a plan name and window numbers.
//!
//! One writer (the app's resident process) records readings; renames and forgets arrive as hub
//! command files ([`apply_command`]). The file is `claude-account-usage.json` in the host app's
//! state folder (`{"schema":1,"accounts":[...]}`, epoch-second dates, the schema the Mac notch
//! writes too). Names come from [`crate::names`]: a rename is recorded there and in the book.
//!
//! The book is process-global and loaded on first use; the file is a cache, so a failed write is
//! dropped, never an error.
//!
//! Ported from Pulse `windows/src/claude_accounts.rs` (#454 first-seen letters, #470 rings only for
//! running Desktop windows, #156 windows past their reset kept as unknown by the caller).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::desktop::{state_base, write_private};
use crate::names::{self, Names, is_account_id};

const FILE: &str = "claude-account-usage.json";
const SCAN_FILE: &str = "claude-accounts-scan.json";
const FILE_MAX_BYTES: u64 = 1024 * 1024;
const COMMAND_MAX_BYTES: usize = 16 * 1024;
const NAME_MAX_CHARS: usize = 60;
/// Unchanged numbers are not rewritten more than once a minute.
const REWRITE_SECONDS: u64 = 60;
/// A scan older than this means the engine that writes it is not running.
const SCAN_MAX_AGE_MS: f64 = 10.0 * 60.0 * 1000.0;
/// The running-accounts list is read again at most this often.
const RUNNING_CACHE: Duration = Duration::from_secs(5);

/// A Session window's length.
pub const SESSION_SECONDS: u64 = 5 * 3600;
/// A Weekly window's length.
pub const WEEK_SECONDS: u64 = 7 * 24 * 3600;

/// The length a reader's window key implies: `session` is five hours, `weekly...` a week, any
/// other key unknown. The hub tells Session from Weekly by this length.
pub fn window_seconds(key: &str) -> Option<u64> {
    match key {
        "session" => Some(SESSION_SECONDS),
        key if key.starts_with("weekly") => Some(WEEK_SECONDS),
        _ => None,
    }
}

/// One limit window of a saved reading.
#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub label: String,
    /// Used fraction, 0.0 to 1.0.
    pub used: f64,
    /// Epoch seconds the window resets.
    pub resets_at: u64,
    pub seconds: Option<u64>,
}

/// One account in the book.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: String,
    /// Order the account was first seen in (1 and up; 0 for none yet).
    pub ordinal: u32,
    /// Chosen in the hub; `None` means the automatic name.
    pub custom_name: Option<String>,
    pub email: Option<String>,
    pub plan: Option<String>,
    pub windows: Vec<Window>,
    /// Epoch 0 with no windows: named before any reading.
    pub captured_at: u64,
}

impl Entry {
    pub fn has_reading(&self) -> bool {
        !self.windows.is_empty()
    }

    fn stub(id: &str) -> Self {
        Self {
            id: id.to_string(),
            ordinal: 0,
            custom_name: None,
            email: None,
            plan: None,
            windows: Vec::new(),
            captured_at: 0,
        }
    }

    /// The chosen name (the newest rename on either computer, [`names`]), else the shared
    /// automatic name. An account the names file has not met yet is named by its custom name, its
    /// first-seen order here (`Claude A`), or [`names::label_in`]'s fallbacks.
    pub fn name(&self, shared: &Names) -> String {
        if !shared.contains_key(&self.id) {
            if let Some(custom) = self.custom_name.as_deref().filter(|n| !n.is_empty()) {
                return custom.to_string();
            }
            if self.ordinal > 0 {
                return letter_name(self.ordinal);
            }
        }
        names::label_in(shared, &self.id)
    }
}

/// `Claude A`, `Claude B`, ... in the order accounts were first seen (#454); past Z, the number.
fn letter_name(ordinal: u32) -> String {
    match u8::try_from(ordinal) {
        Ok(n @ 1..=26) => format!("Claude {}", char::from(b'A' + n - 1)),
        _ => format!("Claude {ordinal}"),
    }
}

/// `Claude <first 8 of the id>`: the name of an account nothing else names.
fn default_name(id: &str) -> String {
    format!("Claude {}", id.get(..8).unwrap_or(id))
}

struct Book {
    loaded: bool,
    entries: Vec<Entry>,
}

static BOOK: Mutex<Book> = Mutex::new(Book {
    loaded: false,
    entries: Vec::new(),
});

fn file_path() -> Option<PathBuf> {
    state_base().ok().map(|base| base.join(FILE))
}

fn with_book<R>(action: impl FnOnce(&mut Book) -> R) -> R {
    let mut book = BOOK.lock().unwrap_or_else(PoisonError::into_inner);
    if !book.loaded {
        book.loaded = true;
        book.entries = load().unwrap_or_default();
    }
    action(&mut book)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The file's bytes when it is a regular file of at most `max` bytes.
fn read_bounded(path: &Path, max: u64) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > max {
        return None;
    }
    std::fs::read(path).ok()
}

// ------------------------------------------------------------------ file

fn text(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|text| !text.is_empty())
}

fn load() -> Option<Vec<Entry>> {
    let bytes = read_bounded(&file_path()?, FILE_MAX_BYTES)?;
    let root: Value = serde_json::from_slice(&bytes).ok()?;
    let mut entries = Vec::new();
    for item in root.get("accounts")?.as_array()? {
        let Some(id) = text(item.get("id")) else {
            continue;
        };
        if !is_account_id(&id) {
            continue;
        }
        let windows = item
            .get("windows")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .filter_map(|window| {
                Some(Window {
                    label: text(window.get("label"))?,
                    used: window.get("usedFraction")?.as_f64()?,
                    resets_at: window.get("resetsAt")?.as_f64()? as u64,
                    seconds: window
                        .get("seconds")
                        .and_then(Value::as_f64)
                        .map(|seconds| seconds as u64),
                })
            })
            .collect();
        entries.push(Entry {
            id,
            ordinal: item
                .get("ordinal")
                .and_then(Value::as_f64)
                .map_or(0, |n| n as u32),
            custom_name: text(item.get("customName")),
            email: text(item.get("email")),
            plan: text(item.get("plan")),
            windows,
            captured_at: item
                .get("capturedAt")
                .and_then(Value::as_f64)
                .map_or(0, |n| n as u64),
        });
    }
    Some(entries)
}

fn window_json(window: &Window) -> Value {
    let used = if window.used.is_finite() {
        window.used
    } else {
        0.0
    };
    let mut out = json!({
        "label": window.label,
        "usedFraction": used,
        "resetsAt": window.resets_at,
    });
    if let Some(seconds) = window.seconds {
        out["seconds"] = json!(seconds);
    }
    out
}

fn windows_json(windows: &[Window]) -> Value {
    Value::Array(windows.iter().map(window_json).collect())
}

fn put_text(out: &mut Value, key: &str, value: &Option<String>) {
    if let Some(value) = value {
        out[key] = json!(value);
    }
}

/// Writes the saved entries (temp file, then rename over). Best effort: the book is a cache.
fn save(entries: &[Entry]) {
    let Some(path) = file_path() else {
        return;
    };
    let rows: Vec<Value> = entries
        .iter()
        .map(|entry| {
            let mut row = json!({
                "capturedAt": entry.captured_at,
                "id": entry.id,
                "ordinal": entry.ordinal,
                "windows": windows_json(&entry.windows),
            });
            put_text(&mut row, "customName", &entry.custom_name);
            put_text(&mut row, "email", &entry.email);
            put_text(&mut row, "plan", &entry.plan);
            row
        })
        .collect();
    let doc = json!({"accounts": rows, "schema": 1});
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut bytes) = serde_json::to_vec(&doc) {
        bytes.push(b'\n');
        let _ = write_private(&path, &bytes, false);
    }
}

fn next_ordinal(entries: &[Entry]) -> u32 {
    entries.iter().map(|entry| entry.ordinal).max().unwrap_or(0) + 1
}

// ------------------------------------------------------------------ reading

/// What the card calls an account: the chosen name, else the sign-in address when there is
/// one, else `Claude <first 8 of the id>`. `email` is Claude Code's address and applies only
/// to Claude Code's own account.
pub fn label(id: &str, email: Option<&str>) -> String {
    let shared = names::load();
    let saved = with_book(|book| {
        book.entries
            .iter()
            .find(|e| e.id == id)
            .map(|entry| entry.name(&shared))
    });
    saved
        .or_else(|| email.filter(|e| !e.is_empty()).map(str::to_string))
        .unwrap_or_else(|| default_name(id))
}

/// The accounts signed in to a Claude Desktop window running now, from the account scan
/// (`claude-accounts-scan.json`, `found.running`: one id per running main process, the Store app
/// and each `--user-data-dir` profile such as Claude B). `None` when the scan is missing, old
/// (over 10 minutes: the engine is not running) or has no such list, so a ring is never hidden
/// on a guess. Re-read at most every 5 seconds.
pub fn running_accounts() -> Option<Vec<String>> {
    type Cached = Option<(Instant, Option<Vec<String>>)>;
    static CACHE: Mutex<Cached> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((at, ids)) = cache.as_ref()
        && at.elapsed() < RUNNING_CACHE
    {
        return ids.clone();
    }
    let ids = (|| {
        let path = state_base().ok()?.join(SCAN_FILE);
        let bytes = read_bounded(&path, FILE_MAX_BYTES)?;
        let root: Value = serde_json::from_slice(&bytes).ok()?;
        let scanned_ms = root.get("scanned_ms")?.as_f64()?;
        if now_ms() as f64 - scanned_ms > SCAN_MAX_AGE_MS {
            return None;
        }
        let list = root.pointer("/found/running")?.as_array()?;
        Some(
            list.iter()
                .filter_map(|v| v.as_str().map(str::to_ascii_lowercase))
                .collect::<Vec<String>>(),
        )
    })();
    *cache = Some((Instant::now(), ids.clone()));
    ids
}

/// Ids of every saved account except `live`, in the order they were first seen (#454): the
/// notch's per-account rings, after the live account's own.
pub fn other_ids(live: Option<&str>) -> Vec<String> {
    let running = running_accounts();
    with_book(|book| {
        let mut others: Vec<&Entry> = book
            .entries
            .iter()
            .filter(|entry| Some(entry.id.as_str()) != live)
            // Only accounts signed in and read: org and leftover folders never become rings.
            .filter(|entry| entry.email.is_some() || entry.has_reading())
            // Only an account whose Claude Desktop window is open has a ring (#470).
            .filter(|entry| running.as_ref().is_none_or(|ids| ids.contains(&entry.id)))
            .collect();
        others.sort_by_key(|entry| entry.ordinal);
        others.into_iter().map(|entry| entry.id.clone()).collect()
    })
}

/// The account's name for its ring (`Claude A`) and the short tag drawn on it (`A`): the last
/// word of a one-letter name, else the first letter. `None` for an account not in the book.
pub fn name_and_tag(id: &str) -> Option<(String, String)> {
    let shared = names::load();
    with_book(|book| {
        let name = book.entries.iter().find(|e| e.id == id)?.name(&shared);
        let last = name.rsplit(' ').next().unwrap_or(&name);
        let tag = if last.chars().count() == 1 {
            last.to_uppercase()
        } else {
            name.chars().take(1).flat_map(char::to_uppercase).collect()
        };
        Some((name, tag))
    })
}

// ------------------------------------------------------------------ changes

/// Saves the account's fresh reading. The caller leaves out windows without a reset time: with
/// none there is no way to say when a saved percentage stops being true. A reading with no
/// windows changes nothing.
pub fn record(
    id: &str,
    email: Option<&str>,
    plan: Option<&str>,
    windows: Vec<Window>,
    captured_at: u64,
) {
    if windows.is_empty() {
        return;
    }
    let address = email.filter(|e| !e.is_empty()).map(str::to_string);
    let plan = plan.map(str::to_string);
    with_book(|book| {
        if let Some(entry) = book.entries.iter_mut().find(|e| e.id == id) {
            let new_email = address.or_else(|| entry.email.clone());
            let new_plan = plan.or_else(|| entry.plan.clone());
            if entry.windows == windows
                && entry.plan == new_plan
                && entry.email == new_email
                && captured_at.saturating_sub(entry.captured_at) < REWRITE_SECONDS
            {
                return;
            }
            entry.windows = windows;
            entry.plan = new_plan;
            entry.email = new_email;
            entry.captured_at = captured_at;
        } else {
            let ordinal = next_ordinal(&book.entries);
            book.entries.push(Entry {
                id: id.to_string(),
                ordinal,
                custom_name: None,
                email: address,
                plan,
                windows,
                captured_at,
            });
        }
        save(&book.entries);
    });
}

/// What the book holds for an account: its windows as saved, the plan and when the reading was
/// taken. The caller decides which windows are still in force.
#[derive(Clone, Debug, PartialEq)]
pub struct Saved {
    pub plan: Option<String>,
    pub windows: Vec<Window>,
    pub captured_at: u64,
}

/// The account's last saved reading, restored when the notch starts so the ring is not empty
/// until the first fetch. `None` for an account not in the book.
pub fn last_reading(id: &str) -> Option<Saved> {
    with_book(|book| {
        let entry = book.entries.iter().find(|e| e.id == id)?;
        Some(Saved {
            plan: entry.plan.clone(),
            windows: entry.windows.clone(),
            captured_at: entry.captured_at,
        })
    })
}

/// "Forget reading": drops what was read for the account. A named account keeps its name;
/// an unnamed one leaves the book (an account folder on disk lists again as unread).
pub fn clear_reading(id: &str) {
    with_book(|book| {
        let Some(index) = book.entries.iter().position(|e| e.id == id) else {
            return;
        };
        if book.entries[index].custom_name.is_some() {
            let entry = &mut book.entries[index];
            entry.windows.clear();
            entry.plan = None;
            entry.captured_at = 0;
        } else {
            book.entries.remove(index);
        }
        save(&book.entries);
    });
}

/// An empty name returns the account to its default name. An account with no entry yet (a
/// folder on disk, never read) gets one so the name sticks; an id that is neither saved nor
/// `on_disk` is ignored.
pub fn rename(id: &str, proposed: &str, on_disk: bool) {
    let name: String = proposed.trim().chars().take(NAME_MAX_CHARS).collect();
    let value = (!name.is_empty()).then_some(name);
    with_book(|book| {
        let Some(index) = book.entries.iter().position(|e| e.id == id) else {
            let Some(value) = value else {
                return;
            };
            if !on_disk {
                return;
            }
            names::rename(id, &value);
            let ordinal = next_ordinal(&book.entries);
            let mut entry = Entry::stub(id);
            entry.ordinal = ordinal;
            entry.custom_name = Some(value);
            book.entries.push(entry);
            save(&book.entries);
            return;
        };
        let shared = names::load().get(id).map(|entry| entry.name.clone());
        if book.entries[index].custom_name == value
            && shared.as_deref().unwrap_or("") == value.as_deref().unwrap_or("")
        {
            return;
        }
        names::rename(id, value.as_deref().unwrap_or(""));
        book.entries[index].custom_name = value;
        // A named stub renamed back to the default has nothing left to keep.
        if book.entries[index].custom_name.is_none()
            && !book.entries[index].has_reading()
            && on_disk
        {
            book.entries.remove(index);
        }
        save(&book.entries);
    });
}

/// Drops an old account from the list. The signed-in account stays, and so does any account
/// whose folder is still `on_disk`: it would reappear at once, and forgetting it would only
/// lose its name.
pub fn forget(id: &str, keeping_active: Option<&str>, on_disk: bool) {
    if keeping_active == Some(id) || on_disk {
        return;
    }
    with_book(|book| {
        let before = book.entries.len();
        book.entries.retain(|e| e.id != id);
        if book.entries.len() != before {
            save(&book.entries);
        }
    });
}

/// Applies a hub command file when it is one of the account commands (`renameClaudeAccount`,
/// `forgetClaudeAccount`); false when it is some other command for the caller to handle.
/// `folders` lists the Desktop account folders on disk (id and folder time) and `active` the
/// signed-in account; both are asked only when a command needs them.
pub fn apply_command(
    bytes: &[u8],
    folders: &dyn Fn() -> Vec<(String, u64)>,
    active: &dyn Fn() -> Option<String>,
) -> bool {
    if bytes.len() > COMMAND_MAX_BYTES {
        return false;
    }
    let Ok(root) = serde_json::from_slice::<Value>(bytes) else {
        return false;
    };
    let command = root.get("command").and_then(Value::as_str);
    if command != Some("renameClaudeAccount") && command != Some("forgetClaudeAccount") {
        return false;
    }
    let Some(id) = root.get("id").and_then(Value::as_str) else {
        return true;
    };
    let id = id.trim().to_ascii_lowercase();
    if !is_account_id(&id) {
        return true;
    }
    let on_disk = folders().iter().any(|(folder, _)| *folder == id);
    if command == Some("renameClaudeAccount") {
        let proposed = root.get("name").and_then(Value::as_str).unwrap_or("");
        rename(&id, proposed, on_disk);
    } else {
        forget(&id, active().as_deref(), on_disk);
    }
    true
}

// ------------------------------------------------------------------ for the hub

/// Who is signed in, as the host's reader knows it.
#[derive(Clone, Copy, Debug, Default)]
pub struct SignedIn<'a> {
    /// The account Claude Desktop is signed in to.
    pub active: Option<&'a str>,
    /// The account Claude Code is signed in to.
    pub code: Option<&'a str>,
    /// Claude Code's sign-in address.
    pub email: Option<&'a str>,
}

/// Every account for the hub's list, as a JSON array: saved ones plus every account folder on
/// disk (`folders`: id and folder time in epoch seconds), the signed-in one first, then by last
/// reading, then by folder time. Dates are epoch seconds; the hub decides "resets in" and
/// "Reset" with its own clock. An account with no saved reading has no windows and a null
/// `capturedAt`; `onDisk` is false only for an account whose folder is gone (the only kind the
/// hub may forget). Each account is also given its first sighting in [`names`], so the computers
/// agree on `Claude A/B` once merged.
pub fn published_json(signed_in: SignedIn<'_>, folders: &[(String, u64)]) -> String {
    let SignedIn {
        active,
        code,
        email,
    } = signed_in;
    let code_email = email.filter(|e| !e.is_empty());
    // Every folder on disk (oldest first) and the signed-in account get an entry, so each has a
    // stable order and so a stable "Claude A/B" name.
    let mut missing: Vec<&(String, u64)> = with_book(|book| {
        folders
            .iter()
            .filter(|(id, _)| !book.entries.iter().any(|entry| entry.id == *id))
            .collect()
    });
    missing.sort_by(|a, b| (a.1, &a.0).cmp(&(b.1, &b.0)));
    let mut adopt: Vec<String> = missing.into_iter().map(|(id, _)| id.clone()).collect();
    if let Some(active) = active
        && !adopt.iter().any(|id| id == active)
        && !with_book(|book| book.entries.iter().any(|entry| entry.id == active))
    {
        adopt.push(active.to_string());
    }
    // Every account here also gets its place in the shared automatic order (first sighting, the
    // folder's age when there is one), so the PC and the Mac name them alike once merged.
    let sightings: Vec<(String, u64)> = with_book(|book| {
        let mut all: Vec<(String, u64)> = book
            .entries
            .iter()
            .map(|entry| {
                let folder = folders
                    .iter()
                    .find(|(id, _)| *id == entry.id)
                    .map_or(0, |(_, modified)| *modified);
                let seen = if folder > 0 {
                    folder
                } else {
                    entry.captured_at
                };
                (entry.id.clone(), seen.saturating_mul(1000))
            })
            .collect();
        all.extend(
            folders
                .iter()
                .map(|(id, modified)| (id.clone(), modified.saturating_mul(1000))),
        );
        all
    });
    names::note_seen(&sightings);
    if let Some(active) = active {
        names::note_seen(&[(active.to_string(), 0)]);
    }
    let shared = names::load();
    let mut all = with_book(|book| {
        if !adopt.is_empty() {
            for id in &adopt {
                let mut entry = Entry::stub(id);
                entry.ordinal = next_ordinal(&book.entries);
                book.entries.push(entry);
            }
            save(&book.entries);
        }
        book.entries.clone()
    });
    // An unread account shows Claude Code's address when it is that account.
    for entry in &mut all {
        if entry.email.is_none() && code == Some(entry.id.as_str()) {
            entry.email = code_email.map(str::to_string);
        }
    }
    let folder_time = |id: &str| {
        folders
            .iter()
            .find(|(folder, _)| folder == id)
            .map_or(0, |(_, modified)| *modified)
    };
    let rank = |entry: &Entry| {
        (
            active == Some(entry.id.as_str()),
            if entry.has_reading() {
                entry.captured_at
            } else {
                0
            },
            folder_time(&entry.id),
        )
    };
    all.sort_by(|a, b| {
        let (x, y) = (rank(a), rank(b));
        y.0.cmp(&x.0)
            .then(y.1.cmp(&x.1))
            .then(y.2.cmp(&x.2))
            .then(a.id.cmp(&b.id))
    });
    let rows: Vec<Value> = all
        .iter()
        .map(|entry| {
            let captured: Value = if entry.has_reading() {
                json!(entry.captured_at)
            } else {
                Value::Null
            };
            let mut row = json!({
                "id": entry.id,
                "name": entry.name(&shared),
                "active": active == Some(entry.id.as_str()),
                "onDisk": folders.iter().any(|(folder, _)| *folder == entry.id),
                "capturedAt": captured,
                "windows": windows_json(&entry.windows),
            });
            put_text(&mut row, "email", &entry.email);
            put_text(&mut row, "plan", &entry.plan);
            row
        })
        .collect();
    Value::Array(rows).to_string()
}
