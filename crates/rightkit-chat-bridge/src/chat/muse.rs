//! Muse Code chats: where Muse keeps them, and which are recent.
//!
//! Muse keeps its data under `$XDG_DATA_HOME/muse`, else `~/.local/share/muse` (the account's own
//! home, not a profile's): one folder per session at
//! `sessions/YYYY/MM/DD/<session-id>/session.jsonl` (JSON lines; a session's subagents live in a
//! `subagent/` folder inside it and are never listed) and `session-index.db`, a SQLite index of
//! them. The index gives the cheap listing (title, folder, last activity), read-only and only the
//! columns named below, as observed in Muse Code 1.4.11 on 2026-10-21; when it cannot be read the
//! sessions folder is walked instead, newest day first, for the ids and last-write times alone.
//! Windows: Muse is `%LOCALAPPDATA%\Programs\muse\muse.cmd`; its data folder is not verified there,
//! and `data_dir` is the one place to change when it differs.
//!
//! Reaching a Muse chat is `deliver_hook`; the chat registers itself and picks messages up through
//! The host app plugin's hook (`codex_hook`, `<cli> chat hook --harness muse`).

use rightkit_db::{DbError, DbOptions, rusqlite};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The kind of a Muse chat in rosters and receipts.
pub const KIND: &str = "muse";

/// How many Muse sessions (newest first) are listed, like `roster::CODEX_THREAD_LIMIT`.
pub const SESSION_LIMIT: usize = 100;

/// Most day folders the fallback walk opens.
const WALK_DAYS: usize = 120;

/// One Muse session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MuseSession {
    pub id: String,
    /// The session's name, else the start of its first prompt, as plain words; empty when neither is known.
    pub title: String,
    /// The folder the session works in; empty when unknown.
    pub cwd: String,
    /// Milliseconds since the epoch of its last activity (0 when unknown).
    pub updated_ms: u64,
}

/// The command that starts a headless turn resuming one Muse session, when this Muse has one
/// (`muse wake`, #410). Muse Code 1.4.11 offers no supported entry that resumes a session with a
/// prompt, and no live probe was possible, so none is invented: this returns `None` and an idle
/// Muse chat is `held` with its message in the hook inbox. When a probe finds the entry (Muse 1.4.11 or
/// newer), this is the one place that returns it; `deliver_hook::deliver` then runs it for an idle
/// chat when `Store::muse_wake_idle` allows, exactly as the Codex wake does (one wake per message id).
pub fn muse_wake_command() -> Option<std::process::Command> {
    None
}

/// Muse's data folder.
pub fn data_dir() -> PathBuf {
    match std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("muse"),
        None => rightkit_app_host::account_home()
            .join(".local")
            .join("share")
            .join("muse"),
    }
}

fn sessions_root() -> PathBuf {
    data_dir().join("sessions")
}

fn index_path() -> PathBuf {
    data_dir().join("session-index.db")
}

/// A title as plain words: whitespace squashed, at most 60 characters.
fn clean_title(text: &str) -> String {
    super::envelope::plain_label(text, 60)
}

fn open_index(path: &Path) -> Result<rusqlite::Connection, rusqlite::Error> {
    let mut options = DbOptions::new(path);
    options.busy_timeout = Duration::from_millis(250);
    options.open_read_only().map_err(|error| match error {
        DbError::Sqlite(error) => error,
        other => rusqlite::Error::ToSqlConversionFailure(Box::new(other)),
    })
}

/// The columns read from the index: id, name, first prompt, folder, last activity (microseconds).
const COLUMNS: &str = "session_id, session_name, title, workspace_root, updated_at_us";

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MuseSession> {
    let text = |i: usize| {
        row.get::<_, Option<String>>(i)
            .map(Option::unwrap_or_default)
    };
    let name = text(1)?;
    let title = if name.trim().is_empty() {
        text(2)?
    } else {
        name
    };
    let micros: i64 = row.get::<_, Option<i64>>(4)?.unwrap_or(0);
    Ok(MuseSession {
        id: text(0)?,
        title: clean_title(&title),
        cwd: text(3)?,
        updated_ms: u64::try_from(micros / 1000).unwrap_or(0),
    })
}

/// A session id is a single folder name: letters, digits, `-` and `_`.
fn valid_id(id: &str) -> bool {
    super::codex_hook::safe_component(id)
}

/// What identifies one state of the index on disk: the size and modification time of the
/// database and of its write-ahead log (`-wal`). Muse writes the log on every change, so an
/// unchanged pair means an unchanged index.
type IndexStamp = [(u64, u64); 2];

fn index_stamp(db: &Path) -> Option<IndexStamp> {
    let of = |path: &Path| {
        std::fs::metadata(path)
            .ok()
            .map(|m| (m.len(), mtime_ms(path)))
            .unwrap_or((0, 0))
    };
    let mut wal = db.as_os_str().to_owned();
    wal.push("-wal");
    let stamp = [of(db), of(Path::new(&wal))];
    (stamp[0] != (0, 0)).then_some(stamp)
}

/// The last listing, reused while the index is unchanged. The full read is a table scan over
/// rows with long titles (about 14 MB on the Dell for 113 sessions) and the chat hub lists every
/// 5 s, so reading it each time cost about 10 GB of disk reads an hour.
static LISTED: std::sync::Mutex<Option<(IndexStamp, usize, Vec<MuseSession>)>> =
    std::sync::Mutex::new(None);

fn list_from_index(limit: usize) -> Result<Vec<MuseSession>, rusqlite::Error> {
    let path = index_path();
    let stamp = index_stamp(&path);
    if let Some(stamp) = stamp {
        let listed = LISTED.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((seen, seen_limit, sessions)) = listed.as_ref()
            && *seen == stamp
            && *seen_limit == limit
        {
            return Ok(sessions.clone());
        }
    }
    let sessions = read_index(&path, limit)?;
    if let Some(stamp) = stamp {
        *LISTED.lock().unwrap_or_else(|e| e.into_inner()) = Some((stamp, limit, sessions.clone()));
    }
    Ok(sessions)
}

fn read_index(path: &Path, limit: usize) -> Result<Vec<MuseSession>, rusqlite::Error> {
    let conn = open_index(path)?;
    // Root sessions only: a subagent's session is not a chat anyone is in.
    let sql = format!(
        "SELECT {COLUMNS} FROM sessions WHERE msp_kind IS NULL OR msp_kind = 'root' \
         ORDER BY updated_at_us DESC LIMIT ?1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([i64::try_from(limit).unwrap_or(i64::MAX)], from_row)?;
    Ok(rows
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|s| valid_id(&s.id))
        .collect())
}

fn find_in_index(id: &str) -> Result<Option<MuseSession>, rusqlite::Error> {
    use rusqlite::OptionalExtension;
    let conn = open_index(&index_path())?;
    let sql = format!("SELECT {COLUMNS} FROM sessions WHERE session_id = ?1");
    conn.query_row(&sql, [id], from_row).optional()
}

/// The numbered sub-folders of `dir`, largest first.
fn numbered(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<(u32, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_str()?.parse::<u32>().ok()?;
            e.path().is_dir().then(|| (n, e.path()))
        })
        .collect();
    found.sort_by_key(|(n, _)| std::cmp::Reverse(*n));
    found.into_iter().map(|(_, path)| path).collect()
}

/// Every `YYYY/MM/DD` folder, newest first.
fn day_folders() -> Vec<PathBuf> {
    numbered(&sessions_root())
        .iter()
        .flat_map(|year| numbered(year))
        .flat_map(|month| numbered(&month))
        .collect()
}

fn mtime_ms(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0))
}

fn list_from_folders(limit: usize) -> Vec<MuseSession> {
    let mut found: Vec<MuseSession> = Vec::new();
    for day in day_folders().into_iter().take(WALK_DAYS) {
        for entry in std::fs::read_dir(&day).into_iter().flatten().flatten() {
            let log = entry.path().join("session.jsonl");
            let Some(id) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if valid_id(&id) && log.is_file() {
                found.push(MuseSession {
                    id,
                    title: String::new(),
                    cwd: String::new(),
                    updated_ms: mtime_ms(&log),
                });
            }
        }
        if found.len() >= limit {
            break;
        }
    }
    found.sort_by_key(|s| std::cmp::Reverse(s.updated_ms));
    found.truncate(limit);
    found
}

/// The newest `limit` Muse sessions, newest first.
pub fn list_sessions(limit: usize) -> Vec<MuseSession> {
    if !sessions_root().is_dir() {
        return Vec::new();
    }
    list_from_index(limit).unwrap_or_else(|_| list_from_folders(limit))
}

/// The session with exactly this id, whether or not it is among the newest.
pub fn find_session(id: &str) -> Option<MuseSession> {
    if !valid_id(id) {
        return None;
    }
    if let Ok(found) = find_in_index(id) {
        return found;
    }
    let log = transcript_path(id)?;
    Some(MuseSession {
        id: id.to_string(),
        title: String::new(),
        cwd: String::new(),
        updated_ms: mtime_ms(&log),
    })
}

/// A session's transcript, `session.jsonl`, found by walking the day folders (never from a path
/// the index records), newest day first.
pub fn transcript_path(id: &str) -> Option<PathBuf> {
    if !valid_id(id) {
        return None;
    }
    day_folders()
        .into_iter()
        .map(|day| day.join(id).join("session.jsonl"))
        .find(|log| log.is_file())
}
