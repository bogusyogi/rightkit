//! Verified pre-migration backup of the database file, beside it in the data
//! folder: `<db file name>.pre-v<from>-<UTC stamp>.bak`.
//!
//! RightKit's `migrate` takes the copy (keyed online backup, named and pruned
//! through `MigrateOptions`, see `migrations::migrate_with`). This file keeps
//! the free-space probe and the check that reopens the copy with the key
//! (same schema version, same message count).
use rightkit_db::rusqlite::Connection;
use std::path::{Path, PathBuf};

/// Backups kept per database file, newest first (the one just made counts).
pub(crate) const KEEP_BACKUPS: usize = 2;

/// Free-space probe: bytes available on the volume holding the given path.
pub(crate) type FreeBytes = fn(&str) -> Option<u64>;

/// The platform probe shared with compaction.
pub(crate) fn platform_free_bytes(db_path: &str) -> Option<u64> {
    free_space::free_bytes(db_path)
}

pub(crate) fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |m| m.len())
}

pub(crate) fn sidecar(db_path: &Path, suffix: &str) -> PathBuf {
    let mut name = db_path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn keyed(path: &Path, key: &str) -> Result<Connection, String> {
    DbOptions::new(path)
        .with_passphrase(key)
        .open_read_only()
        .map_err(|e| format!("open backup file: {e}"))
}

fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |r| r.get::<_, i64>(0),
    )
    .is_ok_and(|n| n > 0)
}

pub(crate) fn message_count(conn: &Connection) -> Result<Option<i64>, String> {
    if !table_exists(conn, "messages") {
        return Ok(None);
    }
    conn.query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
        .map(Some)
        .map_err(|e| format!("count messages: {e}"))
}

pub(crate) fn user_version(conn: &Connection) -> Result<i64, String> {
    rightkit_db::user_version(conn).map_err(|e| format!("read user_version: {e}"))
}

/// Refuse unless the volume of `probe_path` can hold the database at `db_path`,
/// its WAL, and 10% slack. The backup check passes the database path twice; a
/// copy that lands on another volume probes that one.
pub(crate) fn check_free_space_on(
    db_path: &Path,
    probe_path: &Path,
    free_bytes: FreeBytes,
) -> Result<(), String> {
    let used = file_len(db_path) + file_len(&sidecar(db_path, "-wal"));
    let need = used + used / 10;
    let mib = |b: u64| b / (1024 * 1024);
    match free_bytes(&probe_path.to_string_lossy()) {
        Some(free) if free >= need => Ok(()),
        Some(free) => Err(format!(
            "not enough free disk space for a backup (need {} MB, {} MB free)",
            mib(need) + 1,
            mib(free)
        )),
        None => Err("cannot determine free disk space for a backup".into()),
    }
}

/// Re-open `path` with the key and compare it against the source.
pub(crate) fn verify(
    path: &Path,
    key: &str,
    from: i64,
    messages: Option<i64>,
) -> Result<(), String> {
    let copy = keyed(path, key)?;
    let version = user_version(&copy).map_err(|e| format!("verify backup: {e}"))?;
    if version != from {
        return Err(format!(
            "verify backup: schema version {version}, expected {from}"
        ));
    }
    let count = message_count(&copy).map_err(|e| format!("verify backup: {e}"))?;
    if count != messages {
        return Err(format!(
            "verify backup: message count {count:?}, expected {messages:?}"
        ));
    }
    Ok(())
}
