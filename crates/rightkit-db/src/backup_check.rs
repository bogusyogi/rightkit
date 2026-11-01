//! Checks around a pre-migration backup: the free-space probe and the reopen-and-compare of the
//! copy. The copy itself is taken by [`crate::MigrateOptions`]; this module only judges it.
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::DbOptions;

/// Free-space probe: bytes available on the volume holding the given path.
pub type FreeBytes = fn(&str) -> Option<u64>;

/// Bytes available to this user on the volume containing `db_path`, or `None` when it cannot be
/// determined.
pub fn free_bytes(db_path: &str) -> Option<u64> {
    let dir = Path::new(db_path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    rightkit_fs::free_bytes(dir).ok()
}

/// Length of a file, 0 when it is missing.
pub fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |m| m.len())
}

/// `<db path><suffix>`, for example the `-wal` sidecar.
pub fn sidecar(db_path: &Path, suffix: &str) -> PathBuf {
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

/// Row count of `table` (a literal chosen by the caller, never user input), or `None` when the
/// table does not exist.
pub fn row_count(conn: &Connection, table: &str) -> Result<Option<i64>, String> {
    if !table_exists(conn, table) {
        return Ok(None);
    }
    conn.query_row(&format!("SELECT count(*) FROM \"{table}\""), [], |r| {
        r.get(0)
    })
    .map(Some)
    .map_err(|e| format!("count {table}: {e}"))
}

/// Refuse unless the volume of `probe_path` can hold the database at `db_path`, its WAL, and 10%
/// slack. The backup check passes the database path twice; a copy that lands on another volume
/// probes that one.
pub fn check_free_space_on(
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

/// Re-open `path` with the key and compare it against the source: same schema version and the
/// same row count in `count_table`.
pub fn verify(
    path: &Path,
    key: &str,
    from: i64,
    rows: Option<i64>,
    count_table: &str,
) -> Result<(), String> {
    let copy = keyed(path, key)?;
    let version = crate::user_version(&copy).map_err(|e| format!("verify backup: {e}"))?;
    if version != from {
        return Err(format!(
            "verify backup: schema version {version}, expected {from}"
        ));
    }
    let count = row_count(&copy, count_table).map_err(|e| format!("verify backup: {e}"))?;
    if count != rows {
        return Err(format!(
            "verify backup: {count_table} count {count:?}, expected {rows:?}"
        ));
    }
    Ok(())
}
