//! Dedicated SQLite store for the evolution/governance subsystems.
//!
//! The session store (`{app}-storage`) owns one database for sessions,
//! events, tasks, and artifacts. The evolution/governance subsystems —
//! learning proposals, role stats, the observation stream, the hash-chained
//! audit log, and durable memory — get their **own** database
//! (`~/.{app}/evolution.db`) with clear table separation, rather than a
//! scatter of JSON files. These subsystems are synchronous and low-volume
//! (post-run bookkeeping, not the hot session path), so a single connection
//! behind an `Arc<Mutex<…>>` is the right fit: no async seam, transactional
//! writes (no torn JSON on a crash), and the data is genuinely queryable.
//!
//! Rollouts are intentionally *not* persisted here — a rollout is short-lived
//! and the chosen canary model is already written to config (see
//! `rollout_store.rs`).

use crate::api::lock_recover;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use rightkit_db::rusqlite::Connection;
use rightkit_db::{add_column_if_missing, DbOptions};

/// A cloneable handle to the evolution database. All clones share one
/// connection, serialized through the inner mutex.
#[derive(Clone)]
pub struct EvolutionDb {
    conn: Arc<Mutex<Connection>>,
}

impl EvolutionDb {
    /// Open (or create) the database at `path` and run migrations. `rightkit-db` selects WAL
    /// (verified, with a rollback-journal fallback on network mounts), a 5 s busy timeout and
    /// `synchronous=NORMAL`, so a reader and the writer proceed concurrently and a contending
    /// writer waits instead of failing fast.
    pub fn open<P: AsRef<Path>>(path: P) -> rightkit_db::Result<Self> {
        let conn = DbOptions::new(path.as_ref()).open_writer()?;
        let wal = rightkit_db::journal_mode(&conn)?;
        tracing::info!(%wal, "evolution SQLite journal mode");
        migrate(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// An ephemeral in-memory database (the non-persistent path and tests).
    pub fn open_in_memory() -> Self {
        let conn = DbOptions::new(":memory:")
            .open_in_memory()
            .expect("open in-memory sqlite");
        migrate(&conn).expect("migrate in-memory evolution db");
        Self {
            conn: Arc::new(Mutex::new(conn)),
        }
    }

    /// Lock the connection for a unit of work.
    pub fn lock(&self) -> MutexGuard<'_, Connection> {
        lock_recover(&self.conn, "self.conn")
    }
}

fn migrate(conn: &Connection) -> rightkit_db::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS learning_proposals (
             id   TEXT PRIMARY KEY,
             json TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS role_stats (
             role      TEXT PRIMARY KEY,
             runs      INTEGER NOT NULL,
             successes INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS observations (
             id                            INTEGER PRIMARY KEY AUTOINCREMENT,
             role                          TEXT    NOT NULL,
             model                         TEXT    NOT NULL,
             bucket                        INTEGER NOT NULL,
             gds                           REAL    NOT NULL,
             input_tokens                  INTEGER NOT NULL,
             mop_fired                     INTEGER NOT NULL,
             completed_without_escalation  INTEGER NOT NULL,
             quality                       REAL    NOT NULL,
             created_at                    TEXT    NOT NULL
         );
         CREATE INDEX IF NOT EXISTS observations_role ON observations(role);
         CREATE TABLE IF NOT EXISTS audit_log (
             seq        INTEGER PRIMARY KEY,
             created_at TEXT NOT NULL,
             kind       TEXT NOT NULL,
             subject    TEXT NOT NULL,
             summary    TEXT NOT NULL,
             prev_hash  TEXT NOT NULL,
             hash       TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS memories (
             id           TEXT PRIMARY KEY,
             tier         TEXT NOT NULL,
             content      TEXT NOT NULL,
             keywords     TEXT NOT NULL,
             score        REAL NOT NULL,
             created_at   TEXT NOT NULL,
             access_count INTEGER NOT NULL,
             embedding    BLOB,
             embedding_q  BLOB,
             scope_id     TEXT NOT NULL DEFAULT 'global'
         );
         CREATE TABLE IF NOT EXISTS trajectories (
             id         INTEGER PRIMARY KEY AUTOINCREMENT,
             role       TEXT NOT NULL,
             model      TEXT NOT NULL,
             prompt     TEXT NOT NULL,
             output     TEXT NOT NULL,
             gds        REAL NOT NULL,
             created_at TEXT NOT NULL
         );",
    )?;
    // Columns added after the first schema: databases created before them gain the column.
    // `embedding`: semantic memory. `embedding_q`: quantized (i8 TurboQuant) storage, ~4x smaller
    // than the f32 blob; legacy rows keep `embedding`. `scope_id`: per-project scope so ONE store
    // holds many projects with isolation (defaults to 'global'). `agent_version`: provenance.
    add_column_if_missing(conn, "memories", "embedding", "BLOB")?;
    add_column_if_missing(conn, "memories", "embedding_q", "BLOB")?;
    add_column_if_missing(
        conn,
        "memories",
        "scope_id",
        "TEXT NOT NULL DEFAULT 'global'",
    )?;
    add_column_if_missing(conn, "audit_log", "agent_version", "TEXT")?;
    add_column_if_missing(conn, "trajectories", "agent_version", "TEXT")?;
    Ok(())
}
