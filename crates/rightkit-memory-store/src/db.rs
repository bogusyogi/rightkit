//! MBR-501: durable recording of normalized cross-agent observable events.
//!
//! Every client emits the same [`rightkit_context_protocol::ObservableEventV1`]
//! envelope; this module is the single writer that persists one row per
//! event into the `observable_events` table inside the existing
//! membrane-events SQLite database. The table is created on first use so
//! a clean install does not need an explicit migration; no existing table
//! schema is changed.
//!
//! The connection target mirrors the existing `RIGHTKIT_CONTEXT_EVENT_DB` env-var
//! convention (`memdb::resolve_event_db_path`). When that var is unset the
//! function defaults to `<memory_db_basename>.membrane-events.sqlite3` next
//! to the memory DB, matching what `MemDb::open` derives at runtime. The
//! function is intentionally fire-and-log: a missing DB is not fatal, so a
//! client that emits an event before its durable store is initialized
//! cannot deadlock the runtime.

use rightkit_context_protocol::ObservableEventV1;
use rightkit_db::rusqlite::{self, params, Connection};
use rightkit_db::{DbOptions, DurabilityProfile};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Open a durable store for read-write use through rightkit-db. Bootstrap (WAL with verified
/// fallback, busy wait, temp storage, owner-only permissions) is owned by rightkit-db. The
/// profile is `Full` because every store opened here holds non-reconstructable records.
pub(crate) fn open_store(path: &Path) -> rusqlite::Result<Connection> {
    DbOptions {
        durability: DurabilityProfile::Full,
        ..DbOptions::new(path)
    }
    .open_writer()
    .map_err(sqlite_error)
}

/// Open an existing store read-only through rightkit-db with the caller's busy wait. Never
/// creates the file. Callers that need query-only enforcement set it themselves.
pub(crate) fn open_store_read_only(
    path: &Path,
    busy_timeout: Duration,
) -> rusqlite::Result<Connection> {
    DbOptions {
        busy_timeout,
        ..DbOptions::new(path)
    }
    .open_read_only()
    .map_err(sqlite_error)
}

/// Map a rightkit-db error onto the rusqlite error type used by these crates' public signatures.
pub(crate) fn sqlite_error(error: rightkit_db::DbError) -> rusqlite::Error {
    match error {
        rightkit_db::DbError::Sqlite(error) => error,
        other => rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
            Some(other.to_string()),
        ),
    }
}

/// Errors `record_observable_event` can surface to callers. SQLite I/O
/// errors are wrapped verbatim; the typed variants stay narrow on purpose
/// so the runtime can map them to retry decisions.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The on-disk database could not be opened, the schema could not be
    /// applied, or the row insert failed.
    #[error("observable_events insert: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// The caller asked us to persist an envelope whose self-integrity
    /// check failed. This is the gate that prevents a tampered payload
    /// from sneaking into the durable ledger.
    #[error("observable_events integrity check failed: {0}")]
    Integrity(String),
}

/// Persist one observable event into the `observable_events` table. The
/// table is created on first write so no migration is required.
///
/// `now_unix_ms` is the durable-store wall clock (separate from the
/// envelope's `occurredAtUnixMs`, which is the producing client's clock).
/// `event` must verify its own digest; tampering is refused before any
/// row is written.
pub fn record_observable_event(
    event: &ObservableEventV1,
    now_unix_ms: u64,
) -> Result<(), StoreError> {
    let path = resolve_default_path();
    record_observable_event_at_path(event, now_unix_ms, &path)
}

fn record_observable_event_at_path(
    event: &ObservableEventV1,
    now_unix_ms: u64,
    path: &Path,
) -> Result<(), StoreError> {
    if let Err(message) = event.verify_self_integrity() {
        return Err(StoreError::Integrity(message));
    }

    let payload_json = serde_json::to_string(event)
        .map_err(|error| StoreError::Integrity(format!("serialize event: {error}")))?;

    let mut conn = open_store(path)?;
    ensure_observable_events_schema(&mut conn)?;

    let now_iso = crate::time::now_iso();
    conn.execute(
        "INSERT OR IGNORE INTO observable_events (
            event_id, schema_version, kind, client_id, installation_id,
            occurred_at_unix_ms, observed_at_unix_ms, observed_at_iso,
            scope_grant, mechanism, payload_json, digest
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            event.event_id,
            event.schema_version as i64,
            event.kind.as_str(),
            event.client_id,
            event.installation_id,
            event.occurred_at_unix_ms as i64,
            now_unix_ms as i64,
            now_iso,
            event.scope_grant,
            event.mechanism,
            payload_json,
            event.digest,
        ],
    )?;
    Ok(())
}

/// Create the `observable_events` table if it does not exist. The schema
/// mirrors the canonical envelope 1:1 so a round-trip read is a single
/// `SELECT`; the `payload_json` column keeps the original envelope verbatim
/// for forensic replay.
fn ensure_observable_events_schema(conn: &mut Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS observable_events (
            event_id            TEXT PRIMARY KEY,
            schema_version      INTEGER NOT NULL,
            kind                TEXT NOT NULL
                CHECK (kind IN (
                    'context_delivered',
                    'scope_grant_issued',
                    'mechanism_receipt',
                    'adapter_heartbeat',
                    'provider_error',
                    'client_conflation'
                )),
            client_id           TEXT NOT NULL,
            installation_id     TEXT NOT NULL,
            occurred_at_unix_ms INTEGER NOT NULL,
            observed_at_unix_ms INTEGER NOT NULL,
            observed_at_iso     TEXT NOT NULL,
            scope_grant         TEXT,
            mechanism           TEXT,
            payload_json        TEXT NOT NULL,
            digest              TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_observable_events_installation_ts
            ON observable_events(installation_id, occurred_at_unix_ms);
        CREATE INDEX IF NOT EXISTS idx_observable_events_client_kind_ts
            ON observable_events(client_id, kind, occurred_at_unix_ms);",
    )
}

/// Default SQLite path the function opens when `RIGHTKIT_CONTEXT_EVENT_DB` is
/// unset. Mirrors the convention `memdb::resolve_event_db_path` uses so
/// the runtime does not need to plumb a connection into this module.
fn resolve_default_path() -> PathBuf {
    if let Some(path) = crate::qa_env::event_db() {
        return PathBuf::from(path);
    }
    if let Some(path) = crate::qa_env::memory_db() {
        let memory_path = PathBuf::from(path);
        return derive_event_path(&memory_path);
    }
    // Final fallback: an in-memory database is unusable across processes,
    // but it keeps the function total when no env var is configured (the
    // caller can still observe the result through `StoreError::Sqlite`).
    PathBuf::from(":memory:")
}

fn derive_event_path(memory_path: &Path) -> PathBuf {
    let stem = memory_path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("cortex");
    memory_path.with_file_name(format!("{stem}.membrane-events.sqlite3"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_event_path_uses_memory_db_stem() {
        let derived = derive_event_path(Path::new("some/dir/memory.sqlite3"));
        assert_eq!(
            derived.file_name().and_then(|name| name.to_str()),
            Some("memory.membrane-events.sqlite3")
        );
        assert_eq!(
            derived.parent(),
            Path::new("some/dir/memory.sqlite3").parent()
        );
    }

    #[test]
    fn derive_event_path_falls_back_to_cortex_without_stem() {
        let derived = derive_event_path(Path::new(""));
        assert_eq!(derived, PathBuf::from("cortex.membrane-events.sqlite3"));
    }

    #[test]
    fn sqlite_error_passes_sqlite_errors_through_unchanged() {
        let mapped = sqlite_error(rightkit_db::DbError::Sqlite(
            rusqlite::Error::QueryReturnedNoRows,
        ));
        assert!(matches!(mapped, rusqlite::Error::QueryReturnedNoRows));
    }

    #[test]
    fn observable_events_schema_is_idempotent_and_enforces_kind_check() {
        let mut conn = rightkit_db::open_in_memory().unwrap();
        ensure_observable_events_schema(&mut conn).unwrap();
        ensure_observable_events_schema(&mut conn).unwrap();

        let insert = |kind: &str| {
            conn.execute(
                "INSERT INTO observable_events (event_id, schema_version, kind, client_id,
                    installation_id, occurred_at_unix_ms, observed_at_unix_ms, observed_at_iso,
                    payload_json, digest)
                 VALUES (?1, 1, ?2, 'c', 'i', 1, 2, '2026-01-01T00:00:00Z', '{}', 'd')",
                params![format!("evt-{kind}"), kind],
            )
        };
        assert_eq!(insert("adapter_heartbeat").unwrap(), 1);
        assert!(insert("not_a_kind").is_err());
    }

    #[test]
    fn open_store_creates_a_writable_database_in_a_temp_dir() {
        let dir = std::env::temp_dir().join(format!(
            "rightkit-memory-store-db-test-{}-open",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("store.sqlite3");
        {
            let conn = open_store(&path).unwrap();
            conn.execute_batch("CREATE TABLE t (v INTEGER); INSERT INTO t VALUES (7);")
                .unwrap();
        }
        {
            let conn = open_store_read_only(&path, Duration::from_millis(100)).unwrap();
            let value: i64 = conn
                .query_row("SELECT v FROM t", [], |row| row.get(0))
                .unwrap();
            assert_eq!(value, 7);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
