//! The document workspace store: recent files, pinned files and folders, per-document view
//! state and the open-tab session, in one SQLite file the host app places.

use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rightkit_db::rusqlite::{self, Connection};
use rightkit_db::{
    DbError, DbOptions, Degraded, IntegrityCheck, MigrateOptions, Migration, OpenOrDegrade,
    QuarantineStamp, RecoveryPolicy, SidecarPolicy, VersionStorage,
};
use serde::Serialize;

#[derive(Debug, Serialize, Clone)]
pub struct FileEntry {
    pub path: String,
    pub display_name: String,
    pub format: String,
    /// Unix epoch seconds of the last open (recent_files.opened_at). 0 for rows written by a
    /// schema that predates the column — the renderer treats missing values as "no time shown".
    pub opened_at: i64,
}

/// One row of `pinned_files` for `list_pins`. `kind` is "file" (openable documents) or
/// "dir" (explorer sidebar pinned folders); `format` is the document format for files
/// and the literal "dir" for directories, which have no document format.
#[derive(Debug, Serialize, Clone)]
pub struct PinEntry {
    pub path: String,
    pub display_name: String,
    pub format: String,
    pub kind: String,
}

#[derive(Debug, Serialize, Clone, Default)]
pub struct ViewState {
    pub last_view: Option<String>,
    /// Per-format view metadata as a JSON string (text scroll, PDF page/zoom/fit). Opaque to the
    /// backend — the renderer owns the shape.
    pub view_meta: Option<String>,
}

pub struct Db(pub Mutex<Connection>);

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl Db {
    /// Pragmas, WAL fallback, the `quick_check` and quarantine all come from `rightkit-db`; this keeps
    /// the store-specific part: the migration list.
    fn options(path: &Path) -> DbOptions {
        let mut options = DbOptions::new(path);
        // `quick_check` rather than `integrity_check`: this runs on the startup path before the window
        // is shown, and `integrity_check` also cross-references every index (O(database size)).
        // `quick_check` still catches the structural page corruption that makes a file unsafe to serve.
        options.integrity = IntegrityCheck::Quick;
        options
    }

    /// The migration list, in order. Versions are contiguous from 1; the app has no shipped data, so
    /// version 1 is the full current schema.
    fn migrations() -> [Migration; 1] {
        [Migration::sql(
            1,
            "init",
            include_str!("../data/migrations/0001_init.sql"),
        )]
    }

    /// Migration settings. The shared migrator records versions in the store's
    /// `_migrations(version, applied_at)` table. Each migration and its version row commit together or
    /// not at all.
    fn migrate_options() -> MigrateOptions {
        MigrateOptions {
            version_storage: VersionStorage::version_table("_migrations"),
            // A copy `<db>.pre-v<target>.bak` is written beside the database before a migration
            // changes it, so a failed or unwanted upgrade never costs the previous data.
            backup: true,
            optimize_after: false,
            // A database written by a newer build keeps opening, as before.
            forward_compatible: Some(|_| true),
            ..Default::default()
        }
    }

    /// An unreadable file (failed open-time `quick_check`, SQLITE_CORRUPT, SQLITE_NOTADB, including
    /// when it surfaces during migration) is quarantined as `workspace.db.corrupt-<unix-seconds>`
    /// and its `-wal`, `-shm` and `-journal` sidecars are renamed beside it under the same suffix,
    /// so every original byte is kept and no stale WAL is replayed into the replacement. Transient
    /// conditions (busy, locked, permission) are never quarantined.
    ///
    /// There is deliberately no in-memory fallback: a store that does not persist would let the
    /// user keep working and lose everything at quit. When the database cannot be opened the open
    /// fails and the app reports it and exits.
    fn recovery_policy() -> RecoveryPolicy {
        RecoveryPolicy {
            quarantine_corrupt: true,
            quarantine_stamp: QuarantineStamp::Seconds,
            sidecars: SidecarPolicy::RenameSidecars,
            allow_in_memory_fallback: false,
            ..Default::default()
        }
    }

    /// `open()` reports `rusqlite::Error`; keep the underlying SQLite failure where there is one.
    fn to_sqlite(err: DbError) -> rusqlite::Error {
        match err {
            DbError::Sqlite(err) | DbError::MigrationFailed { source: err, .. } => err,
            other => rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
                Some(other.to_string()),
            ),
        }
    }

    /// `PRAGMA journal_mode=WAL` reports the mode it actually adopted rather than failing. `rightkit-db`
    /// falls back to a rollback journal on a filesystem that can't do WAL (network shares, some FUSE
    /// mounts); read the value back so that degradation stays visible in logs. Deliberately non-fatal:
    /// the database is still correct without WAL, just less concurrent.
    fn verify_wal(conn: &Connection) {
        match rightkit_db::journal_mode(conn) {
            Ok(mode) if mode.eq_ignore_ascii_case("wal") => {}
            Ok(mode) => eprintln!(
                "rightkit-document-workspace: database is running in {mode} journal mode, not WAL; concurrent reads during writes may block"
            ),
            Err(err) => eprintln!("rightkit-document-workspace: could not read back journal_mode: {err}"),
        }
    }

    /// Open the on-disk database at `path`, recovering from corruption rather than degrading permanently.
    ///
    /// A single corrupt file would otherwise be terminal: opening fails on every launch. An
    /// unreadable file is quarantined (original bytes and sidecars kept, see `recovery_policy`) and
    /// a fresh persistent database is created at the same path; the returned `Some(Degraded)` has
    /// `quarantined` set so the UI can tell the user where the old file went (`persistence_status`).
    ///
    /// `Err` when the database cannot be opened at all (locked or unwritable folder, a database
    /// written by a newer incompatible build, an unfinished earlier quarantine). The caller reports
    /// it and exits; the session never continues on a store that does not persist.
    pub fn open_path(path: &Path) -> rusqlite::Result<(Self, Option<Degraded>)> {
        let OpenOrDegrade { conn, degraded, .. } = Self::options(path)
            .open_writer_migrated_or_degrade(
                &Self::migrations(),
                &Self::migrate_options(),
                Self::recovery_policy(),
            )
            .map_err(Self::to_sqlite)?;
        if let Some(degraded) = &degraded {
            // The policy refuses an in-memory store, so this cannot happen; if it ever did, a
            // volatile store must still never be served.
            if degraded.in_memory {
                return Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
                    Some(format!(
                        "refusing an in-memory database: {}",
                        degraded.reason
                    )),
                ));
            }
            if let Some(aside) = &degraded.quarantined {
                eprintln!(
                    "rightkit-document-workspace: database was unreadable ({}); kept the original at {} and started a fresh one",
                    degraded.reason,
                    aside.display()
                );
            }
        }
        Self::verify_wal(&conn);
        Ok((Db(Mutex::new(conn)), degraded))
    }

    /// Lock the connection, recovering the guard if a prior holder panicked (mutex poisoning). Release
    /// builds use `panic = "abort"` so poisoning can't occur there, but debug builds must not turn one
    /// panicked query into an app-wide cascade of `.unwrap()` crashes on every later DB call.
    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn list_recent(&self, limit: i64) -> rusqlite::Result<Vec<FileEntry>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT path, display_name, format, opened_at FROM recent_files ORDER BY opened_at DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map([limit], |r| {
                Ok(FileEntry {
                    path: r.get(0)?,
                    display_name: r.get(1)?,
                    format: r.get(2)?,
                    opened_at: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn clear_recent(&self) -> rusqlite::Result<()> {
        self.conn().execute("DELETE FROM recent_files", [])?;
        Ok(())
    }

    pub fn is_pinned(&self, path: &str) -> rusqlite::Result<bool> {
        let conn = self.conn();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pinned_files WHERE path = ?1",
            [path],
            |r| r.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn toggle_pin(
        &self,
        path: &str,
        display_name: &str,
        format: &str,
    ) -> rusqlite::Result<bool> {
        self.toggle_pin_kind(path, display_name, format, "file")
    }

    /// Pin/unpin a directory. Directories have no document format, so `format` stores the
    /// literal "dir" — keeping the NOT NULL column honest and mirroring what `list_pins`
    /// reports for these rows (`kind` distinguishes them either way).
    pub fn toggle_pin_dir(&self, path: &str, display_name: &str) -> rusqlite::Result<bool> {
        self.toggle_pin_kind(path, display_name, "dir", "dir")
    }

    fn toggle_pin_kind(
        &self,
        path: &str,
        display_name: &str,
        format: &str,
        kind: &str,
    ) -> rusqlite::Result<bool> {
        // Hold ONE lock across the check + write so two concurrent toggles can't both INSERT (a
        // PRIMARY KEY violation) or both DELETE. Inlined (not self.is_pinned) because std::Mutex is
        // not reentrant — calling is_pinned() here would re-lock and deadlock.
        let conn = self.conn();
        let pinned: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pinned_files WHERE path = ?1",
            [path],
            |r| r.get(0),
        )?;
        if pinned > 0 {
            conn.execute("DELETE FROM pinned_files WHERE path = ?1", [path])?;
            Ok(false)
        } else {
            let next: i64 = conn.query_row(
                "SELECT COALESCE(MAX(sort_order), 0) + 1 FROM pinned_files",
                [],
                |r| r.get(0),
            )?;
            conn.execute(
                "INSERT INTO pinned_files (path, display_name, format, sort_order, pinned_at, kind) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![path, display_name, format, next, now(), kind],
            )?;
            Ok(true)
        }
    }

    /// All pins in stored pin order (the `sort_order` sequence `toggle_pin*` assigns).
    /// The renderer groups rows into pinned folders then files — the DB does not
    /// re-sort, so pins stay in the order the user created them.
    pub fn list_pins(&self) -> rusqlite::Result<Vec<PinEntry>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT path, display_name, format, kind FROM pinned_files ORDER BY sort_order ASC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(PinEntry {
                    path: r.get(0)?,
                    display_name: r.get(1)?,
                    format: r.get(2)?,
                    kind: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn record_recent(
        &self,
        path: &str,
        display_name: &str,
        format: &str,
    ) -> rusqlite::Result<()> {
        let conn = self.conn();
        // Record-then-prune is one unit: a reader must never observe the table above its cap, and a
        // failed prune must not leave the new row committed on its own.
        let tx = rightkit_db::write_txn_unchecked(&conn).map_err(Self::to_sqlite)?;
        tx.execute(
            "INSERT INTO recent_files (path, display_name, format, opened_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(path) DO UPDATE SET opened_at = excluded.opened_at, display_name = excluded.display_name",
            rusqlite::params![path, display_name, format, now()],
        )?;
        // Prune to the 200 most-recent rows so the table never grows without bound.
        tx.execute(
            "DELETE FROM recent_files WHERE path NOT IN (
                SELECT path FROM recent_files ORDER BY opened_at DESC LIMIT 200)",
            [],
        )?;
        tx.commit()
    }

    pub fn save_view_state(
        &self,
        path: &str,
        last_view: &str,
        view_meta: Option<&str>,
        display_name: &str,
        format: &str,
    ) -> rusqlite::Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO recent_files (path, display_name, format, opened_at, last_view, view_meta)
             VALUES (?1, ?4, ?5, ?6, ?2, ?3)
             ON CONFLICT(path) DO UPDATE SET last_view = excluded.last_view, view_meta = excluded.view_meta",
            rusqlite::params![path, last_view, view_meta, display_name, format, now()],
        )?;
        Ok(())
    }

    /// Restored view state is advisory: a document must still open when it can't be read, so this
    /// falls back to the default rather than propagating. The caller (`read_document`) uses `?`, so
    /// returning an error here would turn a transient database lock into a failure to open a file the
    /// database has no say over. A missing row is the ordinary case for a first-time open and stays
    /// silent; anything else is a real fault and is logged instead of vanishing.
    pub fn view_state(&self, path: &str) -> rusqlite::Result<ViewState> {
        let conn = self.conn();
        let st = conn
            .query_row(
                "SELECT last_view, view_meta FROM recent_files WHERE path = ?1",
                [path],
                |r| {
                    Ok(ViewState {
                        last_view: r.get(0)?,
                        view_meta: r.get::<_, Option<String>>(1)?,
                    })
                },
            )
            .unwrap_or_else(|err| {
                if !matches!(err, rusqlite::Error::QueryReturnedNoRows) {
                    eprintln!(
                        "rightkit-document-workspace: could not read view state for {path}: {err}"
                    );
                }
                ViewState::default()
            });
        Ok(st)
    }

    /// Replace the saved session atomically. The clear-then-insert pair must not run in autocommit:
    /// a failure partway through the loop would otherwise leave the previous session already deleted
    /// and the new one half-written, so the next launch would restore a truncated set of tabs (or
    /// none) with nothing reporting that anything was lost.
    pub fn save_session(&self, tabs: &[(&str, bool)]) -> rusqlite::Result<()> {
        let conn = self.conn();
        let tx = rightkit_db::write_txn_unchecked(&conn).map_err(Self::to_sqlite)?;
        tx.execute("DELETE FROM session_tabs", [])?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO session_tabs (sort_order, path, active) VALUES (?1, ?2, ?3)",
            )?;
            for (i, (path, active)) in tabs.iter().enumerate() {
                stmt.execute(rusqlite::params![i as i64, path, *active as i64])?;
            }
        }
        tx.commit()
    }

    pub fn load_session(&self) -> rusqlite::Result<Vec<(String, bool)>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT path, active FROM session_tabs ORDER BY sort_order ASC")?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? != 0))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn clear_session(&self) -> rusqlite::Result<()> {
        let conn = self.conn();
        conn.execute("DELETE FROM session_tabs", [])?;
        Ok(())
    }

    /// Privacy purge: delete every stored document path (recents with their view state, the saved
    /// session tabs, and pins) in one transaction, then overwrite freed pages (`secure_delete`) and
    /// truncate the WAL so no path string survives in the database file or its sidecar.
    pub fn purge_user_paths(&self) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        conn.execute_batch("PRAGMA secure_delete = ON;")?;
        let tx = rightkit_db::write_txn(&mut conn).map_err(Self::to_sqlite)?;
        tx.execute("DELETE FROM recent_files", [])?;
        tx.execute("DELETE FROM session_tabs", [])?;
        tx.execute("DELETE FROM pinned_files", [])?;
        tx.commit()?;
        // `secure_delete` only blanks cells deleted now; pages freed earlier (migrations, older
        // deletes made before the pragma) still hold path bytes on the freelist. VACUUM rewrites
        // the whole file, dropping every freed page, so nothing stale survives.
        // `sqlite_stat4` keeps sample index keys (here: stored paths) from an earlier ANALYZE; VACUUM
        // copies them over. Re-analysing the purged tables replaces those samples first.
        conn.execute_batch("ANALYZE recent_files; ANALYZE session_tabs; ANALYZE pinned_files;")?;
        conn.execute_batch("VACUUM;")?;
        // A reader that still holds the log keeps it from being truncated, and the path strings the
        // purge promises to remove would stay in the `-wal` file. `busy != 0` means the checkpoint
        // did not complete, so the purge did not either.
        let status = rightkit_db::checkpoint_truncate(&conn).map_err(Self::to_sqlite)?;
        if status.busy != 0 {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                Some("purge incomplete: the write-ahead log could not be truncated".to_string()),
            ));
        }
        Ok(())
    }
}
