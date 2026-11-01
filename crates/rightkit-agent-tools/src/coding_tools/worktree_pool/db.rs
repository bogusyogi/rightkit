//! SQLite-backed metadata database for tracking pool worktrees.
//!
//! DIRECT_PORT_RUST (atom VG-004, lane A17-vg-rust-ports): ported from
//! grok-build `crates/codegen/xai-fast-worktree/src/db/mod.rs` and
//! `db/queries.rs`/`db/schema.rs`. Donor revision and NOTICE entry are
//! recorded in
//! `docs/canon/daemon-master-canon-atoms.md`.
//!
//! Seam adaptations (all at the edges, the record shape and every query stay
//! verbatim):
//!   - Donor resolves a "grok home" directory (`$GROK_HOME` / `~/.grok`) via
//!     the `xai_grok_home` crate and picks a WAL-vs-DELETE journal mode with
//!     network-mount detection via the `xai_sqlite_journal` crate. Neither
//!     crate is a the host app dependency, so `open`/`open_default` here take an
//!     explicit directory from the caller (the daemon's own state dir). Journal
//!     mode, busy timeout and file modes come from `rightkit-db`
//!     (`DbOptions::open_writer`, verified WAL with rollback fallback).
//!     `open_read_only`'s busy-retry loop is kept (it is donor-native SQLite
//!     behavior, not network-journal logic).
//!   - `id_from_path` delegated to `crate::coding_tools::worktree::plan::worktree_id_from_path`
//!     in the donor; that module is not part of this port's allowlist, so
//!     `id_from_path` here hashes the canonicalized path directly (same
//!     `<basename>-<hash>` shape, same collision-avoidance rationale).
//!   - `sweep_dead`'s NFS/"grove" liveness branches (`crate::coding_tools::nfs::*`,
//!     `crate::coding_tools::worktree::is_grove_strategy`) are dropped — the host app's pool
//!     only ever creates local linked worktrees, so a plain
//!     `symlink_metadata` existence check (the donor's non-NFS branch) is the
//!     complete and correct check here. This is the one place behavior was
//!     intentionally narrowed rather than purely relocated; flagged for
//!     Oracle/Sage review if NFS-backed pool worktrees are ever in scope.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use rightkit_db::rusqlite;
use rightkit_db::rusqlite::{params, Connection, ErrorCode};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorktreeKind {
    Session,
    Ab,
    Pool,
    Fork,
    Manual,
    Subagent,
}

impl WorktreeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Ab => "ab",
            Self::Pool => "pool",
            Self::Fork => "fork",
            Self::Manual => "manual",
            Self::Subagent => "subagent",
        }
    }

    pub fn from_str_lossy(s: &str) -> Self {
        Self::from_str_exact(s).unwrap_or(Self::Manual)
    }

    pub fn from_str_exact(s: &str) -> Option<Self> {
        match s {
            "session" => Some(Self::Session),
            "ab" => Some(Self::Ab),
            "pool" => Some(Self::Pool),
            "fork" => Some(Self::Fork),
            "manual" => Some(Self::Manual),
            "subagent" => Some(Self::Subagent),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorktreeStatus {
    Alive,
    Dead,
}

impl WorktreeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Alive => "alive",
            Self::Dead => "dead",
        }
    }

    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "alive" => Self::Alive,
            _ => Self::Dead,
        }
    }
}

pub const META_KEY_LABEL: &str = "label";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorktreeRecord {
    pub id: String,
    pub path: PathBuf,
    pub source_repo: PathBuf,
    pub repo_name: String,
    pub kind: WorktreeKind,
    pub creation_mode: String,
    pub git_ref: Option<String>,
    pub head_commit: Option<String>,
    pub session_id: Option<String>,
    pub creator_pid: Option<u32>,
    pub created_at: i64,
    pub last_accessed_at: Option<i64>,
    pub status: WorktreeStatus,
    pub metadata: Option<serde_json::Value>,
}

impl WorktreeRecord {
    pub fn label(&self) -> Option<&str> {
        self.metadata
            .as_ref()
            .and_then(|m| m.get(META_KEY_LABEL))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    }
}

#[derive(Default)]
pub struct ListFilter {
    pub repo_name: Option<String>,
    pub source_repo: Option<PathBuf>,
    pub kind: Option<WorktreeKind>,
    pub status: Option<WorktreeStatus>,
    pub include_dead: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DbStats {
    pub total_records: u64,
    pub alive_count: u64,
    pub dead_count: u64,
    pub db_file_bytes: u64,
}

pub struct WorktreeDb {
    conn: Connection,
}

const WORKTREES_DB_FILE: &str = "worktrees.db";
const BUSY_RETRY_BUDGET: Duration = Duration::from_secs(5);

fn is_sqlite_busy(e: &rightkit_db::DbError) -> bool {
    matches!(
        e,
        rightkit_db::DbError::Sqlite(rusqlite::Error::SqliteFailure(f, _))
            if matches!(f.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SqliteFailureKind {
    Busy,
    Corrupt,
    Other,
}

/// What a SQLite failure says about the file. Only the damage codes justify
/// telling a user to delete their registry.
pub fn classify_sqlite_error(error: &anyhow::Error) -> SqliteFailureKind {
    let sqlite =
        error
            .chain()
            .find_map(|cause| match cause.downcast_ref::<rightkit_db::DbError>() {
                Some(rightkit_db::DbError::Sqlite(inner)) => Some(inner),
                _ => cause.downcast_ref::<rusqlite::Error>(),
            });
    let Some(rusqlite::Error::SqliteFailure(f, _)) = sqlite else {
        return SqliteFailureKind::Other;
    };
    if matches!(f.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) {
        return SqliteFailureKind::Busy;
    }
    match f.code {
        ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt => SqliteFailureKind::Corrupt,
        _ => SqliteFailureKind::Other,
    }
}

impl WorktreeDb {
    /// Open (or create) the DB at `<state_dir>/worktrees.db`.
    pub fn open(state_dir: &Path) -> Result<Self> {
        Self::open_at(&state_dir.join(WORKTREES_DB_FILE))
    }

    pub fn open_at(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create dir for DB: {}", parent.display()))?;
        }
        // rightkit-db owns connection setup: busy timeout, verified WAL with
        // rollback-journal fallback, owner-only file modes.
        let conn = rightkit_db::DbOptions::new(path)
            .open_writer()
            .with_context(|| format!("failed to open worktree DB: {}", path.display()))?;
        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    /// Read-only open: creates no directory, database file, or schema.
    pub fn open_read_only(state_dir: &Path) -> RegistryOpen {
        let path = state_dir.join(WORKTREES_DB_FILE);
        match Self::open_read_only_at(&path) {
            Ok(Some(db)) => RegistryOpen::Opened { path, db },
            Ok(None) => RegistryOpen::Absent { path },
            Err(OpenFailure::Busy(error)) => RegistryOpen::Busy { path, error },
            Err(OpenFailure::Other(error)) => RegistryOpen::Failed { path, error },
        }
    }

    fn open_read_only_at(path: &Path) -> Result<Option<Self>, OpenFailure> {
        let present = path.try_exists().map_err(|e| {
            OpenFailure::Other(
                anyhow::Error::new(e)
                    .context(format!("cannot stat worktree DB: {}", path.display())),
            )
        })?;
        if !present {
            return Ok(None);
        }
        let deadline = std::time::Instant::now() + BUSY_RETRY_BUDGET;
        let conn = Self::open_readonly_with_busy_retry(path, deadline)?;
        Ok(Some(Self { conn }))
    }

    fn open_readonly_with_busy_retry(
        path: &Path,
        deadline: std::time::Instant,
    ) -> Result<Connection, OpenFailure> {
        let context = || format!("failed to open worktree DB read-only: {}", path.display());
        let start = std::time::Instant::now();
        loop {
            match rightkit_db::DbOptions::new(path).open_read_only() {
                Ok(conn) => return Ok(conn),
                Err(e) if !is_sqlite_busy(&e) => {
                    return Err(OpenFailure::Other(anyhow::Error::new(e).context(context())));
                }
                Err(e) => {
                    if std::time::Instant::now() >= deadline {
                        let elapsed = start.elapsed();
                        return Err(OpenFailure::Busy(anyhow::Error::new(e).context(format!(
                            "{} (database busy after {elapsed:?})",
                            context()
                        ))));
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = rightkit_db::open_in_memory().context("failed to open in-memory DB")?;
        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    fn init_schema(&self) -> Result<()> {
        self.conn
            .execute_batch(schema::INIT_SQL)
            .context("failed to init worktree DB schema")?;
        let stored: Option<String> = self
            .conn
            .query_row(schema::GET_META, ["schema_version"], |row| row.get(0))
            .ok();
        let needs_update = match stored {
            None => true,
            Some(v) => v.parse::<u32>().unwrap_or(0) < schema::SCHEMA_VERSION,
        };
        if needs_update {
            self.conn.execute(
                schema::UPSERT_META,
                params!["schema_version", schema::SCHEMA_VERSION.to_string()],
            )?;
        }
        Ok(())
    }

    pub fn register(&self, record: &WorktreeRecord) -> Result<()> {
        queries::register(&self.conn, record)
    }

    pub fn unregister(&self, id: &str) -> Result<bool> {
        queries::unregister(&self.conn, id)
    }

    pub fn unregister_by_path(&self, path: &Path) -> Result<bool> {
        queries::unregister_by_path(&self.conn, path)
    }

    pub fn mark_dead(&self, id: &str) -> Result<bool> {
        queries::mark_dead(&self.conn, id)
    }

    pub fn touch(&self, id: &str) -> Result<bool> {
        queries::touch(&self.conn, id)
    }

    pub fn get_by_id(&self, id: &str) -> Result<Option<WorktreeRecord>> {
        queries::get_by_id(&self.conn, id)
    }

    pub fn get(&self, id_or_path: &str) -> Result<Option<WorktreeRecord>> {
        if id_or_path.contains('/') || id_or_path.contains('\\') {
            let canon = PathBuf::from(id_or_path);
            let canon = std::fs::canonicalize(&canon).unwrap_or(canon);
            queries::get_by_path(&self.conn, &canon)
        } else {
            let by_id = queries::get_by_id(&self.conn, id_or_path)?;
            if by_id.is_some() {
                return Ok(by_id);
            }
            queries::get_by_label(&self.conn, id_or_path)
        }
    }

    pub fn get_by_label(&self, label: &str) -> Result<Option<WorktreeRecord>> {
        queries::get_by_label(&self.conn, label)
    }

    pub fn list(&self, filter: &ListFilter) -> Result<Vec<WorktreeRecord>> {
        queries::list(&self.conn, filter)
    }

    pub fn stats(&self) -> Result<DbStats> {
        queries::stats(&self.conn)
    }

    /// Mark all `alive` records whose path no longer exists on disk as dead.
    /// (Donor's NFS/"grove" liveness branches dropped — see module doc.)
    pub fn sweep_dead(&self) -> Result<u64> {
        queries::sweep_dead(&self.conn)
    }

    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        match self
            .conn
            .query_row(schema::GET_META, [key], |row| row.get(0))
        {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).with_context(|| format!("failed to read meta key {key}")),
        }
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn
            .execute(schema::UPSERT_META, params![key, value])
            .with_context(|| format!("failed to write meta key {key}"))?;
        Ok(())
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn execute_batch_for_test(&self, sql: &str) -> Result<()> {
        self.conn
            .execute_batch(sql)
            .context("execute_batch_for_test failed")?;
        Ok(())
    }
}

/// Outcome of a read-only open. A corrupt file still reaches `Opened`, since
/// SQLite reads lazily and fails at the first query.
pub enum RegistryOpen {
    Opened { path: PathBuf, db: WorktreeDb },
    Absent { path: PathBuf },
    Busy { path: PathBuf, error: anyhow::Error },
    Failed { path: PathBuf, error: anyhow::Error },
}

#[derive(Debug)]
enum OpenFailure {
    Busy(anyhow::Error),
    Other(anyhow::Error),
}

/// Derive a worktree ID from its destination path: `<basename>-<hash of full
/// path>`. Hashing the full path (not just the basename) keeps distinct
/// worktrees distinct when basenames collide across source repos, matching
/// the donor's `INSERT OR REPLACE` collision-avoidance rationale.
///
/// Seam note: the donor delegates to `crate::coding_tools::worktree::plan::worktree_id_from_path`
/// (not on this port's allowlist); this hashes the canonicalized path with
/// `DefaultHasher` directly to keep the same collision-avoidance property
/// without pulling in that module.
pub fn id_from_path(path: &Path) -> String {
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let basename = canon
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "worktree".to_string());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    canon.hash(&mut hasher);
    format!("{basename}-{:016x}", hasher.finish())
}

pub(crate) fn repo_name_from_path(source: &Path) -> String {
    source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_string())
}

pub fn now_epoch_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

mod schema {
    pub const SCHEMA_VERSION: u32 = 1;

    pub const INIT_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS worktrees (
    id TEXT PRIMARY KEY,
    path TEXT UNIQUE NOT NULL,
    source_repo TEXT NOT NULL,
    repo_name TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'session',
    creation_mode TEXT NOT NULL DEFAULT 'linked',
    git_ref TEXT,
    head_commit TEXT,
    session_id TEXT,
    creator_pid INTEGER,
    created_at INTEGER NOT NULL,
    last_accessed_at INTEGER,
    status TEXT NOT NULL DEFAULT 'alive',
    metadata TEXT
);

CREATE INDEX IF NOT EXISTS idx_worktrees_repo ON worktrees(repo_name);
CREATE INDEX IF NOT EXISTS idx_worktrees_status_kind ON worktrees(status, kind);
CREATE INDEX IF NOT EXISTS idx_worktrees_session ON worktrees(session_id);
CREATE INDEX IF NOT EXISTS idx_worktrees_created ON worktrees(created_at);
"#;

    pub const UPSERT_META: &str = "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)";
    pub const GET_META: &str = "SELECT value FROM meta WHERE key = ?1";
}

mod queries {
    use std::path::Path;

    use anyhow::{Context, Result};
    use rightkit_db::rusqlite;
    use rightkit_db::rusqlite::{params, Connection};

    use super::{
        now_epoch_secs, DbStats, ListFilter, WorktreeKind, WorktreeRecord, WorktreeStatus,
    };

    fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorktreeRecord> {
        let kind_str: String = row.get("kind")?;
        let status_str: String = row.get("status")?;
        let path_str: String = row.get("path")?;
        let source_str: String = row.get("source_repo")?;
        let metadata_str: Option<String> = row.get("metadata")?;

        Ok(WorktreeRecord {
            id: row.get("id")?,
            path: path_str.into(),
            source_repo: source_str.into(),
            repo_name: row.get("repo_name")?,
            kind: WorktreeKind::from_str_lossy(&kind_str),
            creation_mode: row.get("creation_mode")?,
            git_ref: row.get("git_ref")?,
            head_commit: row.get("head_commit")?,
            session_id: row.get("session_id")?,
            creator_pid: row.get::<_, Option<i64>>("creator_pid")?.map(|v| v as u32),
            created_at: row.get("created_at")?,
            last_accessed_at: row.get("last_accessed_at")?,
            status: WorktreeStatus::from_str_lossy(&status_str),
            metadata: metadata_str.and_then(|s| serde_json::from_str(&s).ok()),
        })
    }

    pub fn register(conn: &Connection, record: &WorktreeRecord) -> Result<()> {
        let path_str = record.path.to_string_lossy();
        let source_str = record.source_repo.to_string_lossy();
        let metadata_str = record
            .metadata
            .as_ref()
            .and_then(|v| serde_json::to_string(v).ok());

        conn.execute(
            "INSERT OR REPLACE INTO worktrees \
             (id, path, source_repo, repo_name, kind, creation_mode, git_ref, \
              head_commit, session_id, creator_pid, created_at, last_accessed_at, \
              status, metadata) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                record.id,
                path_str.as_ref(),
                source_str.as_ref(),
                record.repo_name,
                record.kind.as_str(),
                record.creation_mode,
                record.git_ref,
                record.head_commit,
                record.session_id,
                record.creator_pid.map(|p| p as i64),
                record.created_at,
                record.last_accessed_at,
                record.status.as_str(),
                metadata_str,
            ],
        )
        .context("failed to register worktree")?;
        Ok(())
    }

    pub fn unregister(conn: &Connection, id: &str) -> Result<bool> {
        let affected = conn
            .execute("DELETE FROM worktrees WHERE id = ?1", params![id])
            .context("failed to unregister worktree")?;
        Ok(affected > 0)
    }

    pub fn unregister_by_path(conn: &Connection, path: &Path) -> Result<bool> {
        let path_str = path.to_string_lossy();
        let affected = conn
            .execute(
                "DELETE FROM worktrees WHERE path = ?1",
                params![path_str.as_ref()],
            )
            .context("failed to unregister worktree by path")?;
        Ok(affected > 0)
    }

    pub fn mark_dead(conn: &Connection, id: &str) -> Result<bool> {
        let affected = conn
            .execute(
                "UPDATE worktrees SET status = 'dead' WHERE id = ?1",
                params![id],
            )
            .context("failed to mark worktree dead")?;
        Ok(affected > 0)
    }

    pub fn touch(conn: &Connection, id: &str) -> Result<bool> {
        let now = now_epoch_secs();
        let affected = conn
            .execute(
                "UPDATE worktrees SET last_accessed_at = ?1 WHERE id = ?2",
                params![now, id],
            )
            .context("failed to touch worktree")?;
        Ok(affected > 0)
    }

    fn get_one(conn: &Connection, sql: &str, param: &str) -> Result<Option<WorktreeRecord>> {
        let mut stmt = conn.prepare(sql)?;
        let mut rows = stmt.query_map(params![param], row_to_record)?;
        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn get_by_id(conn: &Connection, id: &str) -> Result<Option<WorktreeRecord>> {
        get_one(conn, "SELECT * FROM worktrees WHERE id = ?1", id)
    }

    pub fn get_by_label(conn: &Connection, label: &str) -> Result<Option<WorktreeRecord>> {
        get_one(
            conn,
            "SELECT * FROM worktrees WHERE json_valid(metadata) AND json_extract(metadata, '$.label') = ?1 ORDER BY created_at DESC",
            label,
        )
    }

    pub fn get_by_path(conn: &Connection, path: &Path) -> Result<Option<WorktreeRecord>> {
        get_one(
            conn,
            "SELECT * FROM worktrees WHERE path = ?1",
            &path.to_string_lossy(),
        )
    }

    pub fn list(conn: &Connection, filter: &ListFilter) -> Result<Vec<WorktreeRecord>> {
        let mut sql = String::from("SELECT * FROM worktrees WHERE 1=1");
        let mut idx = 0usize;

        let status_str = filter.status.map(|s| s.as_str());
        let kind_str = filter.kind.map(|k| k.as_str());
        let source_repo_str = filter
            .source_repo
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned());

        if !filter.include_dead {
            sql.push_str(" AND status = 'alive'");
        }
        if status_str.is_some() {
            idx += 1;
            sql.push_str(&format!(" AND status = ?{idx}"));
        }
        if kind_str.is_some() {
            idx += 1;
            sql.push_str(&format!(" AND kind = ?{idx}"));
        }
        if filter.repo_name.is_some() {
            idx += 1;
            sql.push_str(&format!(" AND repo_name = ?{idx}"));
        }
        if source_repo_str.is_some() {
            idx += 1;
            sql.push_str(&format!(" AND source_repo = ?{idx}"));
        }
        sql.push_str(" ORDER BY created_at DESC");

        let mut params: Vec<&dyn rusqlite::types::ToSql> = Vec::with_capacity(idx);
        if let Some(ref s) = status_str {
            params.push(s);
        }
        if let Some(ref k) = kind_str {
            params.push(k);
        }
        if let Some(ref r) = filter.repo_name {
            params.push(r);
        }
        if let Some(ref s) = source_repo_str {
            params.push(s);
        }

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params.as_slice(), row_to_record)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn stats(conn: &Connection) -> Result<DbStats> {
        // SQLite integers are i64 and rusqlite implements no `FromSql` for
        // u64, so read signed and clamp; counts and page sizes are never
        // negative in practice.
        let total = conn
            .query_row("SELECT COUNT(*) FROM worktrees", [], |row| {
                row.get::<_, i64>(0)
            })?
            .max(0) as u64;
        let alive = conn
            .query_row(
                "SELECT COUNT(*) FROM worktrees WHERE status = 'alive'",
                [],
                |row| row.get::<_, i64>(0),
            )?
            .max(0) as u64;
        let page_count = conn
            .query_row("PRAGMA page_count", [], |row| row.get::<_, i64>(0))
            .unwrap_or(0)
            .max(0) as u64;
        let page_size = conn
            .query_row("PRAGMA page_size", [], |row| row.get::<_, i64>(0))
            .unwrap_or(0)
            .max(0) as u64;

        Ok(DbStats {
            total_records: total,
            alive_count: alive,
            dead_count: total.saturating_sub(alive),
            db_file_bytes: page_count * page_size,
        })
    }

    /// Adapted: donor's NFS/"grove" liveness branches dropped (see module
    /// doc); this is the donor's local-disk branch only.
    pub fn sweep_dead(conn: &Connection) -> Result<u64> {
        let alive_paths: Vec<(String, String)> = {
            let mut stmt = conn.prepare("SELECT id, path FROM worktrees WHERE status = 'alive'")?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.filter_map(|r| r.ok()).collect()
        };

        let mut marked = 0u64;
        for (id, path_str) in alive_paths {
            let dest = Path::new(&path_str);
            if std::fs::symlink_metadata(dest).is_err() {
                conn.execute(
                    "UPDATE worktrees SET status = 'dead' WHERE id = ?1",
                    params![id],
                )?;
                marked += 1;
            }
        }
        Ok(marked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_get_roundtrip() {
        let db = WorktreeDb::open_in_memory().expect("open");
        let record = WorktreeRecord {
            id: "wt-1".to_string(),
            path: PathBuf::from("/tmp/wt-1"),
            source_repo: PathBuf::from("/tmp/source"),
            repo_name: "source".to_string(),
            kind: WorktreeKind::Pool,
            creation_mode: "linked".to_string(),
            git_ref: None,
            head_commit: None,
            session_id: None,
            creator_pid: None,
            created_at: now_epoch_secs(),
            last_accessed_at: None,
            status: WorktreeStatus::Alive,
            metadata: None,
        };
        db.register(&record).expect("register");

        let fetched = db.get_by_id("wt-1").expect("get").expect("present");
        assert_eq!(fetched.path, PathBuf::from("/tmp/wt-1"));
        assert_eq!(fetched.kind, WorktreeKind::Pool);
    }

    #[test]
    fn mark_dead_and_list_respects_include_dead() {
        let db = WorktreeDb::open_in_memory().expect("open");
        let mk = |id: &str| WorktreeRecord {
            id: id.to_string(),
            path: PathBuf::from(format!("/tmp/{id}")),
            source_repo: PathBuf::from("/tmp/source"),
            repo_name: "source".to_string(),
            kind: WorktreeKind::Pool,
            creation_mode: "linked".to_string(),
            git_ref: None,
            head_commit: None,
            session_id: None,
            creator_pid: None,
            created_at: now_epoch_secs(),
            last_accessed_at: None,
            status: WorktreeStatus::Alive,
            metadata: None,
        };
        db.register(&mk("wt-a")).unwrap();
        db.register(&mk("wt-b")).unwrap();
        db.mark_dead("wt-a").unwrap();

        let alive_only = db.list(&ListFilter::default()).unwrap();
        assert_eq!(alive_only.len(), 1);
        assert_eq!(alive_only[0].id, "wt-b");

        let with_dead = db
            .list(&ListFilter {
                include_dead: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(with_dead.len(), 2);
    }

    #[test]
    fn sweep_dead_marks_missing_paths() {
        let temp = tempfile::tempdir().expect("temp dir");
        let present = temp.path().join("present");
        std::fs::create_dir(&present).expect("mkdir");

        let db = WorktreeDb::open_in_memory().expect("open");
        let mk = |id: &str, path: PathBuf| WorktreeRecord {
            id: id.to_string(),
            path,
            source_repo: PathBuf::from("/tmp/source"),
            repo_name: "source".to_string(),
            kind: WorktreeKind::Pool,
            creation_mode: "linked".to_string(),
            git_ref: None,
            head_commit: None,
            session_id: None,
            creator_pid: None,
            created_at: now_epoch_secs(),
            last_accessed_at: None,
            status: WorktreeStatus::Alive,
            metadata: None,
        };
        db.register(&mk("wt-present", present)).unwrap();
        db.register(&mk("wt-missing", temp.path().join("missing")))
            .unwrap();

        let marked = db.sweep_dead().expect("sweep");
        assert_eq!(marked, 1);
        assert_eq!(
            db.get_by_id("wt-missing").unwrap().unwrap().status,
            WorktreeStatus::Dead
        );
        assert_eq!(
            db.get_by_id("wt-present").unwrap().unwrap().status,
            WorktreeStatus::Alive
        );
    }

    #[test]
    fn id_from_path_is_stable_and_distinguishes_same_basename() {
        let temp = tempfile::tempdir().expect("temp dir");
        let a = temp.path().join("repo-a").join("worktree");
        let b = temp.path().join("repo-b").join("worktree");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();

        let id_a = id_from_path(&a);
        let id_b = id_from_path(&b);
        assert_ne!(
            id_a, id_b,
            "same basename under different parents must differ"
        );
        assert_eq!(
            id_a,
            id_from_path(&a),
            "id must be stable for the same path"
        );
    }
}
