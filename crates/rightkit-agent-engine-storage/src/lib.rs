//! rightkit-agent-engine-storage — SQLite-backed persistence for the host app sessions,
//! the append-only event log, tasks, artifacts, and memory.

mod adapt;
mod adapt_observation_cursor;
mod artifacts;
mod cli_attestation;
mod cli_profile;
mod compaction_archives;
mod driver_host;
mod error;
mod events;
mod goals;
mod handoff;
mod inference;
mod memory;
pub mod migrations;
mod remote;
mod run_budget_state;
mod session;
mod soak_health;
mod tasks;
mod tool_invocations;
mod transcript_export;

pub use adapt::{
    AdaptApplicationReceiptRow, AdaptGuidanceRow, AdaptGuidanceWrite, AdaptReceiptAppend,
};
pub use adapt_observation_cursor::AdaptObservationCursor;

use rightkit_db::rusqlite;
use rightkit_db::rusqlite::{Connection, OptionalExtension};
use std::ops::Deref;
use std::path::Path;

pub use cli_attestation::{
    attestation_inputs_are_unique, CliAttestationResult, CliHarnessAttestationRow,
};
pub use cli_profile::CliAccountProfileRow;
pub use compaction_archives::{
    CompactionArchiveRow, MAX_COMPACTION_ARCHIVE_BYTES, MAX_COMPACTION_ARCHIVE_SESSION_BYTES,
};
pub use driver_host::{
    DriverBindingRow, DriverEventClaim, DriverEventInboxRow, SessionCapabilityLeaseRow,
};
pub use error::{Result, StorageError};
pub use events::{
    set_sqlite_op_observer, tool_call_origin_id, DecodedEvent, EventPage, EventPersistenceSnapshot,
    EventPersistenceState, EventRow, PersistenceGapError, PersistenceStateError, QuarantineOutcome,
    SessionMatch, SqliteOpKind, SqliteOpObserver, RESUME_PAGE_ROWS,
};
pub use goals::{GoalRow, GoalStatus};
pub use handoff::{
    HandoffAdvance, HandoffGapMessage, HandoffJournalRow, HandoffRow, HandoffTerminalState,
    QuiescenceOutcome,
};
pub use inference::{
    DirectInferenceConnectionRow, InferenceRouteAttestationRow, InferenceRuntimeBindingRow,
};
pub use memory::MemoryRow;
pub use remote::{RemoteBindingRow, RemoteChannelRow, RemoteDeliveryRow};
pub use session::SessionRow;
pub use soak_health::{
    freelist_count, integrity_ok, orphan_event_count, orphan_session_count, orphan_snapshot,
    page_count, page_size, seq_contiguous, seq_gaps, wal_size_for, wal_snapshot, OrphanSnapshot,
    WalSnapshot,
};
pub use tool_invocations::{
    ToolInvocationCancellationRow, ToolInvocationEventStoreRow, ToolInvocationStoreRow,
};
pub use transcript_export::TranscriptExportCursor;

#[derive(Debug, Clone)]
pub struct BenchmarkRow {
    pub id: String,
    pub role: String,
    pub model: String,
    pub suite: String,
    pub repeats: u32,
    pub successes: u32,
    pub task_id: Option<String>,
    pub passed: Option<bool>,
    pub agent_output: Option<String>,
    pub verify_output: Option<String>,
    pub verify_error: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Default)]
pub struct BenchmarkDetail {
    pub task_id: Option<String>,
    pub passed: Option<bool>,
    pub agent_output: Option<String>,
    pub verify_output: Option<String>,
    pub verify_error: Option<String>,
}

// Re-export the protocol types consumers will pass in/out of storage.
pub use rightkit_agent_engine_protocol::{
    ArtifactApproval, ArtifactRow, ArtifactVerdict, ProtocolEvent, TaskNode, TranscriptMessage,
};

/// A handle to the SQLite-backed store. Owns a single connection. Migrations
/// run automatically on open.
pub struct Storage {
    pub(crate) conn: Connection,
    read_pool: Option<StoragePool>,
    /// Cached highest `seq` per session, so `append_event` doesn't run a
    /// `SELECT MAX(seq)` on every append. Safe because this single connection
    /// (behind the daemon's mutex) is the only writer.
    pub(crate) seq_cache: std::collections::HashMap<String, i64>,
}

#[derive(Clone)]
pub struct StoragePool(rightkit_db::ReadPool);

pub struct PooledReadConnection(rightkit_db::PooledReadConnection);

impl Deref for PooledReadConnection {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl StoragePool {
    pub fn open<P: AsRef<Path>>(path: P, max_size: usize) -> Result<Self> {
        let options = rightkit_db::DbOptions::new(path.as_ref())
            .with_journal_env("RIGHTKIT_SQLITE_JOURNAL_MODE")
            .with_per_host_fallback(true);
        Self::from_options(options, max_size)
    }

    fn from_options(options: rightkit_db::DbOptions, max_size: usize) -> Result<Self> {
        options
            .open_read_pool(max_size.max(1))
            .map(Self)
            .map_err(StorageError::from)
    }

    pub fn get(&self) -> Result<PooledReadConnection> {
        self.0
            .get()
            .map(PooledReadConnection)
            .map_err(StorageError::from)
    }

    pub fn idle_len(&self) -> usize {
        self.0.idle_len()
    }
}

/// Best-effort `chmod`. A store on a filesystem without POSIX modes (a mounted
/// share, some containers) must still open, so failure warns rather than aborts.
/// Restrict the database AND its WAL sidecars to the owner, and the directory
/// that holds them to 0700.
///
/// Hardening only the main file is a half-fix: in WAL mode the `-wal` sidecar
/// carries committed transactions that have not been checkpointed yet, so it
/// holds the most RECENT transcript data — and SQLite creates it 0644 minus
/// umask, independently of the main file. Observed on a live install after the
/// main database had already been locked down: `host app.db` 0600,
/// `host app.db-wal` 0644.
///
/// Newly created database files and sidecars are hardened on every open path;
/// the legacy sibling-directory sweep is canonicalized and runs once per
/// process.
///
/// Reapply RightKit owner-only policy to the state directory after opening DB.
fn restrict_storage_directory(path: &Path) -> Result<()> {
    let Some(dir) = path.parent() else {
        return Ok(());
    };
    rightkit_fs::restrict_owner_only(dir).map_err(StorageError::from)?;
    for entry in std::fs::read_dir(dir)
        .map_err(|error| StorageError::Serde(format!("read storage directory: {error}")))?
    {
        let entry = entry.map_err(|error| StorageError::Serde(error.to_string()))?;
        rightkit_fs::restrict_owner_only(&entry.path()).map_err(StorageError::from)?;
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod db_permission_tests {
    use super::Storage;
    use std::os::unix::fs::PermissionsExt;

    /// The database holds transcripts, tool arguments and results, and workspace
    /// paths. SQLite creates 0644 minus umask, so it shipped readable by every
    /// local account while `daemon.port` beside it was correctly 0600.
    /// The `-wal` sidecar holds committed transactions not yet checkpointed —
    /// the most RECENT transcript data — and SQLite creates it independently of
    /// the main file. A live install had `host app.db` at 0600 and
    /// `host app.db-wal` at 0644 after the main file had been "fixed".
    #[test]
    fn wal_sidecars_and_the_data_directory_are_restricted_too() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.db");

        let storage = Storage::open(&path).expect("opens");
        // Force a WAL write so the sidecars exist.
        storage
            .conn
            .execute_batch("CREATE TABLE probe (v TEXT); INSERT INTO probe VALUES ('x');")
            .unwrap();
        super::restrict_storage_directory(&path).unwrap();

        for suffix in ["", "-wal", "-shm"] {
            let p = std::path::PathBuf::from(format!("{}{suffix}", path.display()));
            if !p.exists() {
                continue;
            }
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{} must be owner-only", p.display());
        }
        let dir_mode = std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "data directory must be owner-only");
    }

    /// The database's directory also holds the trigger store, the middleware
    /// span log and migration backups, each written by a different path. A live
    /// install had ten group/world-readable files here after the database itself
    /// had been restricted — so the sweep covers siblings, not just the db.
    #[test]
    fn sibling_state_files_are_restricted_too() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.db");
        std::fs::write(dir.path().join("triggers.json"), "{}").unwrap();
        std::fs::write(dir.path().join("runtime-middleware.json"), "{}").unwrap();
        for f in ["triggers.json", "runtime-middleware.json"] {
            std::fs::set_permissions(dir.path().join(f), std::fs::Permissions::from_mode(0o644))
                .unwrap();
        }

        let _storage = Storage::open(&path).expect("opens");

        for f in ["triggers.json", "runtime-middleware.json"] {
            let mode = std::fs::metadata(dir.path().join(f))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "{f} must be owner-only");
        }
    }

    #[test]
    fn opening_a_database_restricts_it_to_the_owner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.db");

        let _storage = Storage::open(&path).expect("opens");

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "database must not be group/world readable");
    }

    /// The journal mode the writer applies must be derived from the
    /// filesystem state of the database path, not hardcoded. The default
    /// case (local disk) is still WAL; the test asserts that the
    /// runtime report matches the requested policy.
    #[test]
    fn storage_open_uses_journal_policy_derived_from_filesystem_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policy.db");
        let storage = Storage::open(&path).unwrap();
        let report = rightkit_db::journal_mode(&storage.conn).unwrap();
        assert!(matches!(report.as_str(), "wal" | "delete" | "truncate"));
    }
}

impl Storage {
    /// Persist the daemon-wide PL-009 observation ledger as one atomically
    /// replaced snapshot. Schema creation is idempotent for older databases.
    pub fn save_observation_snapshot(&mut self, snapshot: &[u8]) -> Result<()> {
        self.conn.execute_batch("CREATE TABLE IF NOT EXISTS observation_ledger (id INTEGER PRIMARY KEY CHECK (id = 1), snapshot BLOB NOT NULL)")?;
        self.conn.execute("INSERT INTO observation_ledger (id, snapshot) VALUES (1, ?1) ON CONFLICT(id) DO UPDATE SET snapshot = excluded.snapshot", [snapshot])?;
        Ok(())
    }

    /// Commit corrective tasks with their emitted-reaction markers. A crash
    /// must not retain a dedup marker while losing the canonical task.
    pub fn save_observation_reaction(
        &mut self,
        session_id: &str,
        tasks: &[rightkit_agent_engine_protocol::TaskNode],
        snapshot: &[u8],
    ) -> Result<()> {
        self.conn.execute_batch("CREATE TABLE IF NOT EXISTS observation_ledger (id INTEGER PRIMARY KEY CHECK (id = 1), snapshot BLOB NOT NULL)")?;
        let tx = rightkit_db::write_txn(&mut self.conn)?;
        for task in tasks {
            let node_json = serde_json::to_string(task)?;
            tx.execute(
                "INSERT OR REPLACE INTO tasks (session_id, id, node_json) VALUES (?1, ?2, ?3)",
                (session_id, &task.id, &node_json),
            )?;
        }
        tx.execute("INSERT INTO observation_ledger (id, snapshot) VALUES (1, ?1) ON CONFLICT(id) DO UPDATE SET snapshot = excluded.snapshot", [snapshot])?;
        tx.commit()?;
        Ok(())
    }

    pub fn load_observation_snapshot(&self) -> Result<Option<Vec<u8>>> {
        self.conn.execute_batch("CREATE TABLE IF NOT EXISTS observation_ledger (id INTEGER PRIMARY KEY CHECK (id = 1), snapshot BLOB NOT NULL)")?;
        let mut stmt = self
            .conn
            .prepare("SELECT snapshot FROM observation_ledger WHERE id = 1")?;
        Ok(stmt.query_row([], |row| row.get(0)).optional()?)
    }
    /// Open (or create) a database at `path` and run migrations.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let mut options = rightkit_db::DbOptions::new(path)
            .with_journal_env("RIGHTKIT_SQLITE_JOURNAL_MODE")
            .with_per_host_fallback(true);
        options.foreign_keys = true;
        let conn = options.open_writer()?;
        let effective_path =
            conn.query_row("PRAGMA database_list", [], |row| row.get::<_, String>(2))?;
        let effective_path = Path::new(&effective_path);
        restrict_storage_directory(effective_path)?;
        migrations::migrate(&conn, Some(effective_path))?;
        let mut read_options = rightkit_db::DbOptions::new(effective_path)
            .with_journal_env("RIGHTKIT_SQLITE_JOURNAL_MODE")
            .with_per_host_fallback(true);
        read_options.foreign_keys = true;
        if effective_path != path {
            read_options.journal = rightkit_db::JournalSelect::Truncate;
        }
        let read_pool = StoragePool::from_options(read_options, 4)?;
        let mut storage = Storage {
            conn,
            read_pool: Some(read_pool),
            seq_cache: std::collections::HashMap::new(),
        };
        // S:E11: salvage malformed journal records before any public replay
        // or session rehydration can read this database. Retain raw bytes in
        // events_quarantine through the existing recovery authority.
        let corrupt_sessions: Vec<String> = {
            let mut stmt = storage.conn.prepare(
                "SELECT DISTINCT session_id FROM events WHERE NOT json_valid(payload_json)",
            )?;
            let rows = stmt.query_map([], |row| row.get(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for session_id in corrupt_sessions {
            let outcome = storage.quarantine_malformed_payload_events(&session_id)?;
            tracing::warn!(
                session_id = %session_id,
                quarantined_seqs = ?outcome.quarantined_seqs,
                reason = outcome.reason,
                "malformed durable events quarantined before replay"
            );
        }
        Ok(storage)
    }

    /// Borrow the underlying writer `Connection` so the workflow supervisor
    /// (WP-03) can drive lease fencing, journal appends, and projection
    /// updates inside the same single-writer SQLite transaction. The caller
    /// must respect the daemon's single-writer invariant — every write is
    /// serialized through this handle. Added in WP-03 alongside
    /// `rightkit_agent_engine_storage::workflow` (ADR-0021).
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn read_pool(&self) -> Option<StoragePool> {
        self.read_pool.clone()
    }

    /// Delete events for sessions that have been dormant for more than `older_than_days` days,
    /// and that have at least one `SessionCompacted` event (meaning their history is summarised).
    /// Returns the number of rows deleted.
    pub fn prune_old_events(&self, older_than_days: u32) -> Result<u64> {
        let cutoff_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .saturating_sub(older_than_days as u64 * 86400);
        let cutoff = format_unix_as_iso(cutoff_secs);
        let deleted = self.conn.execute(
            "DELETE FROM events WHERE session_id IN (
                 SELECT id FROM sessions
                 WHERE status = 'dormant'
                   AND updated_at < ?1
                   AND id IN (
                       SELECT DISTINCT session_id FROM events WHERE kind = 'sessionCompacted'
                   )
             )",
            [&cutoff],
        )? as u64;
        Ok(deleted)
    }

    /// Delete all events for a session with seq < the given threshold.
    /// Returns the number of rows deleted.
    pub fn delete_events_before_seq(&self, session_id: &str, seq: u32) -> Result<usize> {
        let deleted = self.conn.execute(
            "DELETE FROM events WHERE session_id = ?1 AND seq < ?2",
            rusqlite::params![session_id, seq as i64],
        )?;
        Ok(deleted)
    }

    /// Get all sessions that have been compacted, returning (session_id, compaction_seq) pairs
    /// where compaction_seq is the seq of the latest SessionCompacted event for that session.
    pub fn compacted_sessions(&self) -> Result<Vec<(String, u32)>> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, MAX(seq) as seq FROM events \
             WHERE kind = 'sessionCompacted' GROUP BY session_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u32))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Open an in-memory database and run migrations. Useful for tests.
    pub fn open_in_memory() -> Result<Self> {
        let conn = rightkit_db::open_in_memory()?;
        conn.execute_batch("PRAGMA temp_store=MEMORY;")?;
        migrations::migrate(&conn, None)?;
        Ok(Storage {
            conn,
            read_pool: None,
            seq_cache: std::collections::HashMap::new(),
        })
    }

    /// Persist one benchmark report row.
    #[allow(clippy::too_many_arguments)]
    pub fn record_benchmark(
        &self,
        id: &str,
        role: &str,
        model: &str,
        suite: &str,
        repeats: u32,
        successes: u32,
        created_at: &str,
    ) -> Result<()> {
        self.record_benchmark_with_detail(
            id, role, model, suite, repeats, successes, created_at, None,
        )
    }

    /// Persist one benchmark report row with optional task/verifier detail.
    #[allow(clippy::too_many_arguments)]
    pub fn record_benchmark_with_detail(
        &self,
        id: &str,
        role: &str,
        model: &str,
        suite: &str,
        repeats: u32,
        successes: u32,
        created_at: &str,
        detail: Option<&BenchmarkDetail>,
    ) -> Result<()> {
        let detail = detail.cloned().unwrap_or_default();
        self.conn.execute(
            "INSERT OR REPLACE INTO benchmarks \
             (id, role, model, suite, repeats, successes, task_id, passed, agent_output, verify_output, verify_error, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![
                id,
                role,
                model,
                suite,
                repeats,
                successes,
                detail.task_id,
                detail.passed.map(|passed| if passed { 1_i64 } else { 0_i64 }),
                detail.agent_output,
                detail.verify_output,
                detail.verify_error,
                created_at
            ],
        )?;
        Ok(())
    }

    /// List all benchmark reports, newest first.
    pub fn list_benchmarks(&self) -> Result<Vec<BenchmarkRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, role, model, suite, repeats, successes, task_id, passed, agent_output, verify_output, verify_error, created_at \
             FROM benchmarks ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(BenchmarkRow {
                id: r.get(0)?,
                role: r.get(1)?,
                model: r.get(2)?,
                suite: r.get(3)?,
                repeats: r.get::<_, i64>(4)? as u32,
                successes: r.get::<_, i64>(5)? as u32,
                task_id: r.get(6)?,
                passed: r.get::<_, Option<i64>>(7)?.map(|value| value != 0),
                agent_output: r.get(8)?,
                verify_output: r.get(9)?,
                verify_error: r.get(10)?,
                created_at: r.get(11)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

/// Format a Unix timestamp (seconds since epoch) as a zero-padded RFC 3339 string
/// in UTC (`YYYY-MM-DDTHH:MM:SSZ`). Used so ISO string comparisons in SQL work
/// correctly against timestamps produced by `now_iso()` in the daemon.
fn format_unix_as_iso(secs: u64) -> String {
    i64::try_from(secs)
        .ok()
        .and_then(|secs| rightkit_time::DateTime::from_unix(secs, 0))
        .map(|dt| dt.format_rfc3339())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_engine_protocol::task::TaskState;

    fn sess(id: &str) -> SessionRow {
        SessionRow {
            id: id.to_string(),
            title: format!("title-{id}"),
            workspace_root: format!("/ws/{id}"),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            parent_session_id: None,
            role: None,
            status: "idle".to_string(),
            config_json: "{}".to_string(),
            tags: String::new(),
            recovery_marker: None,
        }
    }

    fn task(id: &str, title: &str) -> TaskNode {
        TaskNode {
            id: id.to_string(),
            title: title.to_string(),
            deps: vec![],
            done_when: "true".to_string(),
            done_when_predicates: vec![],
            state: TaskState::Open,
            category: String::new(),
            estimated_duration: String::new(),
            assigned_role: String::new(),
            escalation_path: String::new(),
            open_questions: vec![],
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn artifact(id: &str, path: &str) -> ArtifactRow {
        ArtifactRow {
            id: id.to_string(),
            path: path.to_string(),
            kind: "file".to_string(),
            content_hash: "deadbeef".to_string(),
            shippable: false,
            producer: "agent".to_string(),
            provenance: String::new(),
            verdict: None,
            approvals: vec![],
            meta: String::new(),
            trust_label: "unreviewed".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn read_pool_caps_total_connections() {
        use std::sync::mpsc;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool-cap.sqlite");
        let writer = Connection::open(&path).unwrap();
        drop(writer);

        let pool = StoragePool::open(&path, 1).unwrap();
        let first = pool.get().unwrap();
        let (tx, rx) = mpsc::channel();
        let waiting_pool = pool.clone();
        let waiter = std::thread::spawn(move || {
            let second = waiting_pool.get().unwrap();
            tx.send(()).unwrap();
            drop(second);
        });

        assert!(
            rx.recv_timeout(Duration::from_millis(150)).is_err(),
            "a saturated pool must not open an uncapped reader"
        );
        drop(first);
        rx.recv_timeout(Duration::from_secs(2))
            .expect("waiter should acquire the returned reader");
        waiter.join().unwrap();
        assert_eq!(pool.idle_len(), 1);
    }

    fn mem(id: &str, tier: &str, score: f64) -> MemoryRow {
        MemoryRow {
            id: id.to_string(),
            tier: tier.to_string(),
            content: format!("content-{id}"),
            score,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            meta_json: "{}".to_string(),
        }
    }

    // ── open / migrations ────────────────────────────────────────────────

    #[test]
    fn open_in_memory_runs_migrations() {
        let s = Storage::open_in_memory().unwrap();
        // Tables exist if queries against them succeed.
        assert!(s.list_sessions().unwrap().is_empty());
        assert_eq!(s.latest_seq("nope").unwrap(), 0);
        assert!(s.get_tasks("nope").unwrap().is_empty());
        assert!(s.list_artifacts("nope").unwrap().is_empty());
        assert!(s.top_memories(10).unwrap().is_empty());
    }

    #[test]
    fn chat_history_ids_ignore_session_setup_events() {
        let mut s = Storage::open_in_memory().unwrap();
        s.append_event("empty", "sessionCreated", r#"{"$kind":"sessionCreated"}"#)
            .unwrap();
        assert!(s.session_ids_with_chat_history().unwrap().is_empty());

        s.append_event(
            "chatted",
            "userMessageSubmitted",
            r#"{"$kind":"userMessageSubmitted","text":"hello"}"#,
        )
        .unwrap();
        assert_eq!(
            s.session_ids_with_chat_history().unwrap(),
            std::collections::HashSet::from(["chatted".to_string()]),
        );

        s.append_event(
            "compacted",
            "sessionCompacted",
            r#"{"$kind":"sessionCompacted","summary":"earlier chat"}"#,
        )
        .unwrap();
        assert_eq!(
            s.session_ids_with_chat_history().unwrap(),
            std::collections::HashSet::from(["chatted".to_string(), "compacted".to_string(),]),
        );
    }

    #[test]
    fn open_on_disk_runs_migrations() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("db.sqlite");
        let s = Storage::open(&path).unwrap();
        assert!(s.list_sessions().unwrap().is_empty());
        // Reopen to confirm persistence + idempotent migration.
        drop(s);
        let s2 = Storage::open(&path).unwrap();
        assert!(s2.list_sessions().unwrap().is_empty());
    }

    #[test]
    fn open_on_disk_enables_wal_and_read_pool() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("db.sqlite");
        let s = Storage::open(&path).unwrap();

        let wal: String = s
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(wal.to_ascii_lowercase(), "wal");
        let synchronous: i64 = s
            .conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .unwrap();
        let busy_timeout: i64 = s
            .conn
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        let journal_limit: i64 = s
            .conn
            .query_row("PRAGMA journal_size_limit", [], |r| r.get(0))
            .unwrap();
        let temp_store: i64 = s
            .conn
            .query_row("PRAGMA temp_store", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            (synchronous, busy_timeout, journal_limit),
            (1, 5_000, 67_108_864)
        );
        assert_eq!(temp_store, 2);

        let pool = s.read_pool().expect("on-disk stores have a read pool");
        let reader = pool.get().unwrap();
        let query_only: i64 = reader
            .query_row("PRAGMA query_only", [], |r| r.get(0))
            .unwrap();
        assert_eq!(query_only, 1);
        let reader_sync: i64 = reader
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .unwrap();
        let reader_busy: i64 = reader
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        let reader_temp_store: i64 = reader
            .query_row("PRAGMA temp_store", [], |r| r.get(0))
            .unwrap();
        assert_eq!((reader_sync, reader_busy), (1, 5_000));
        assert_eq!(reader_temp_store, 2);
        let table_count: i64 = reader
            .query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| r.get(0))
            .unwrap();
        assert!(table_count > 0);
    }

    #[test]
    fn pooled_read_connection_returns_to_idle_pool() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("db.sqlite");
        let s = Storage::open(&path).unwrap();
        let pool = s.read_pool().expect("on-disk stores have a read pool");

        let before = pool.idle_len();
        {
            let _reader = pool.get().unwrap();
            assert_eq!(pool.idle_len(), before.saturating_sub(1));
        }
        assert!(pool.idle_len() >= before);
    }

    #[test]
    fn benchmark_detail_roundtrips() {
        let s = Storage::open_in_memory().unwrap();
        let detail = BenchmarkDetail {
            task_id: Some("tb-001".to_string()),
            passed: Some(true),
            agent_output: Some("agent fixed it".to_string()),
            verify_output: Some("ok".to_string()),
            verify_error: Some(String::new()),
        };

        s.record_benchmark_with_detail(
            "bench-1",
            "coder",
            "openrouter/test",
            "terminal-bench",
            1,
            1,
            "2026-06-06T00:00:00Z",
            Some(&detail),
        )
        .unwrap();

        let row = s.list_benchmarks().unwrap().remove(0);
        assert_eq!(row.task_id.as_deref(), Some("tb-001"));
        assert_eq!(row.passed, Some(true));
        assert_eq!(row.agent_output.as_deref(), Some("agent fixed it"));
        assert_eq!(row.verify_output.as_deref(), Some("ok"));
        assert_eq!(row.verify_error.as_deref(), Some(""));
    }

    // ── sessions ─────────────────────────────────────────────────────────

    #[test]
    fn create_and_get_session() {
        let s = Storage::open_in_memory().unwrap();
        let row = sess("a");
        s.create_session(&row).unwrap();
        let got = s.get_session("a").unwrap().unwrap();
        assert_eq!(got, row);
    }

    #[test]
    fn child_sessions_returns_the_agent_tree() {
        let s = Storage::open_in_memory().unwrap();
        s.create_session(&sess("conductor")).unwrap();
        let mut child1 = sess("worker-1");
        child1.parent_session_id = Some("conductor".to_string());
        child1.role = Some("coder".to_string());
        let mut child2 = sess("worker-2");
        child2.parent_session_id = Some("conductor".to_string());
        let unrelated = sess("other");
        s.create_session(&child1).unwrap();
        s.create_session(&child2).unwrap();
        s.create_session(&unrelated).unwrap();

        let kids = s.child_sessions("conductor").unwrap();
        let ids: Vec<&str> = kids.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["worker-1", "worker-2"]);
        assert_eq!(kids[0].role.as_deref(), Some("coder"));
    }

    #[test]
    fn update_session_status_changes_status_and_timestamp() {
        let s = Storage::open_in_memory().unwrap();
        s.create_session(&sess("a")).unwrap();
        s.update_session_status("a", "dormant", "2026-02-02T00:00:00Z")
            .unwrap();
        let got = s.get_session("a").unwrap().unwrap();
        assert_eq!(got.status, "dormant");
        assert_eq!(got.updated_at, "2026-02-02T00:00:00Z");
    }

    /// Startup interruption-marking reads a session snapshot, then writes
    /// "interrupted" only if the row is unchanged. That guard is only worth
    /// anything if the comparison and the write are one statement, so assert
    /// the CAS refuses on *either* half of the expectation drifting.
    #[test]
    fn update_session_status_if_unchanged_is_a_real_compare_and_swap() {
        let s = Storage::open_in_memory().unwrap();
        s.create_session(&sess("a")).unwrap();
        let base = s.get_session("a").unwrap().unwrap();

        // Matching snapshot -> the write lands.
        assert!(s
            .update_session_status_if_unchanged(
                "a",
                "interrupted",
                "2026-02-02T00:00:00Z",
                &base.status,
                &base.updated_at,
                None,
            )
            .unwrap());
        assert_eq!(s.get_session("a").unwrap().unwrap().status, "interrupted");

        // Simulates a concurrent turn committing "running" after the snapshot
        // was taken: the stale scan must NOT clobber it.
        s.update_session_status("a", "running", "2026-02-03T00:00:00Z")
            .unwrap();
        assert!(!s
            .update_session_status_if_unchanged(
                "a",
                "interrupted",
                "2026-02-04T00:00:00Z",
                &base.status,
                &base.updated_at,
                None,
            )
            .unwrap());
        let got = s.get_session("a").unwrap().unwrap();
        assert_eq!(
            got.status, "running",
            "live write must survive a stale scan"
        );
        assert_eq!(got.updated_at, "2026-02-03T00:00:00Z");

        // Same status but a newer timestamp still means someone touched it.
        assert!(!s
            .update_session_status_if_unchanged(
                "a",
                "interrupted",
                "2026-02-05T00:00:00Z",
                "running",
                "2026-02-03T00:00:00Z_stale",
                None,
            )
            .unwrap());
        assert_eq!(s.get_session("a").unwrap().unwrap().status, "running");

        // A missing row is not a silent success.
        assert!(!s
            .update_session_status_if_unchanged(
                "missing",
                "interrupted",
                "2026-02-06T00:00:00Z",
                "running",
                "2026-02-03T00:00:00Z",
                None,
            )
            .unwrap());
    }

    /// `now_iso()` resolves to whole seconds, so a concurrent writer that
    /// re-persists the SAME status within the same second produces a row
    /// byte-identical to the scan's snapshot — equality alone cannot tell that
    /// apart from an untouched row. The age bound is what separates them.
    #[test]
    fn update_session_status_if_unchanged_rejects_same_second_write() {
        let s = Storage::open_in_memory().unwrap();
        s.create_session(&sess("a")).unwrap();

        // A live turn re-persists "running" in the same second the scan began.
        // The snapshot the scan holds is identical to what is now on the row.
        s.update_session_status("a", "running", "2026-02-02T00:00:05Z")
            .unwrap();
        let snapshot = s.get_session("a").unwrap().unwrap();

        // Without the bound, equality matches and the live row is clobbered.
        // With the scan-start bound, it is correctly refused.
        assert!(
            !s.update_session_status_if_unchanged(
                "a",
                "interrupted",
                "2026-02-02T00:00:05Z",
                &snapshot.status,
                &snapshot.updated_at,
                Some("2026-02-02T00:00:05Z"),
            )
            .unwrap(),
            "a write in the scan's own second must not be treated as stale"
        );
        assert_eq!(s.get_session("a").unwrap().unwrap().status, "running");

        // A genuinely stale row — written strictly before the scan began — is
        // still marked, so the bound does not disable the sweep.
        s.update_session_status("a", "running", "2026-02-02T00:00:04Z")
            .unwrap();
        let stale = s.get_session("a").unwrap().unwrap();
        assert!(s
            .update_session_status_if_unchanged(
                "a",
                "interrupted",
                "2026-02-02T00:00:05Z",
                &stale.status,
                &stale.updated_at,
                Some("2026-02-02T00:00:05Z"),
            )
            .unwrap());
        assert_eq!(s.get_session("a").unwrap().unwrap().status, "interrupted");
    }

    #[test]
    fn update_session_recovery_marker_round_trips_and_clears() {
        let s = Storage::open_in_memory().unwrap();
        s.create_session(&sess("a")).unwrap();
        let marker_json = r#"{"turnId":"turn-1","startedAt":"t","promptPreview":"x"}"#;
        s.update_session_recovery_marker("a", Some(marker_json), "2026-02-02T00:00:00Z")
            .unwrap();
        let got = s.get_session("a").unwrap().unwrap();
        assert_eq!(got.recovery_marker.as_deref(), Some(marker_json));
        assert_eq!(got.updated_at, "2026-02-02T00:00:00Z");

        // Clearing back to None (clean completion) erases the marker but
        // leaves the row in place — the session is still persisted.
        s.update_session_recovery_marker("a", None, "2026-02-03T00:00:00Z")
            .unwrap();
        let got = s.get_session("a").unwrap().unwrap();
        assert!(got.recovery_marker.is_none());
        assert_eq!(got.updated_at, "2026-02-03T00:00:00Z");
    }

    #[test]
    fn update_session_config_string_sets_and_clears_spec_binding() {
        let s = Storage::open_in_memory().unwrap();
        let mut row = sess("a");
        row.config_json = serde_json::json!({"model": "openrouter/test"}).to_string();
        s.create_session(&row).unwrap();

        assert!(s
            .update_session_config_string("a", "specId", Some("spec-123"), "2026-02-03T00:00:00Z")
            .unwrap());
        let got = s.get_session("a").unwrap().unwrap();
        let config: serde_json::Value = serde_json::from_str(&got.config_json).unwrap();
        assert_eq!(config["model"], "openrouter/test");
        assert_eq!(config["specId"], "spec-123");
        assert_eq!(got.updated_at, "2026-02-03T00:00:00Z");

        assert!(s
            .update_session_config_string("a", "specId", None, "2026-02-04T00:00:00Z")
            .unwrap());
        let got = s.get_session("a").unwrap().unwrap();
        let config: serde_json::Value = serde_json::from_str(&got.config_json).unwrap();
        assert!(config.get("specId").is_none());
        assert_eq!(got.updated_at, "2026-02-04T00:00:00Z");
        assert!(!s
            .update_session_config_string("missing", "specId", Some("spec-123"), "now")
            .unwrap());
    }

    #[test]
    fn update_session_inference_selection_replaces_scalar_model_atomically() {
        let s = Storage::open_in_memory().unwrap();
        let mut row = sess("a");
        row.config_json =
            serde_json::json!({"model": "legacy/model", "safeMode": true}).to_string();
        s.create_session(&row).unwrap();
        let selection = serde_json::json!({
            "schemaVersion": 1,
            "authority": "direct",
            "connectionId": "direct-1",
            "protocol": "openai_compatible",
            "modelId": "model-a",
            "routePolicyId": null
        });

        assert!(s
            .update_session_inference_selection(
                "a",
                &selection,
                "fingerprint-a",
                "2026-08-11T00:00:00Z",
            )
            .unwrap());
        let got = s.get_session("a").unwrap().unwrap();
        let config: serde_json::Value = serde_json::from_str(&got.config_json).unwrap();
        assert_eq!(config["inferenceSelection"], selection);
        assert_eq!(config["inferenceFingerprint"], "fingerprint-a");
        assert_eq!(config["safeMode"], true);
        assert!(config.get("model").is_none());
        assert!(!s
            .update_session_inference_selection(
                "missing",
                &serde_json::Value::Null,
                "fingerprint",
                "now",
            )
            .unwrap());
    }

    #[test]
    fn list_sessions_returns_all() {
        let s = Storage::open_in_memory().unwrap();
        s.create_session(&sess("a")).unwrap();
        s.create_session(&sess("b")).unwrap();
        s.create_session(&sess("c")).unwrap();
        let all = s.list_sessions().unwrap();
        assert_eq!(all.len(), 3);
        let ids: Vec<_> = all.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"a") && ids.contains(&"b") && ids.contains(&"c"));
    }

    #[test]
    fn delete_session_removes_it() {
        let s = Storage::open_in_memory().unwrap();
        s.create_session(&sess("a")).unwrap();
        s.create_session(&sess("b")).unwrap();
        s.delete_session("a").unwrap();
        assert!(s.get_session("a").unwrap().is_none());
        assert!(s.get_session("b").unwrap().is_some());
        assert_eq!(s.list_sessions().unwrap().len(), 1);
    }

    #[test]
    fn get_missing_session_is_none() {
        let s = Storage::open_in_memory().unwrap();
        assert!(s.get_session("ghost").unwrap().is_none());
    }

    #[test]
    fn create_session_overwrites() {
        let s = Storage::open_in_memory().unwrap();
        s.create_session(&sess("a")).unwrap();
        let mut updated = sess("a");
        updated.title = "renamed".to_string();
        s.create_session(&updated).unwrap();
        assert_eq!(s.get_session("a").unwrap().unwrap().title, "renamed");
        assert_eq!(s.list_sessions().unwrap().len(), 1);
    }

    /// Lightweight throughput check (Codex review #5: "benchmark before changing
    /// databases"). Appends a realistic burst of events and a full rehydrate read.
    /// Asserts a generous floor so it doubles as a regression guard; prints the
    /// rate so we have real numbers to decide if `spawn_blocking`/a DB actor is
    /// worth it. (`:memory:` is faster than on-disk WAL but the ratio is what we
    /// care about — appends are tiny vs LLM/IO.)
    #[test]
    fn bench_append_and_rehydrate_throughput() {
        use std::time::Instant;
        let mut s = Storage::open_in_memory().unwrap();
        s.create_session(&sess("bench")).unwrap();
        let n = 5_000;
        let payload =
            serde_json::json!({"$kind":"assistantDelta","text":"x".repeat(40)}).to_string();

        let t0 = Instant::now();
        for _ in 0..n {
            s.append_event("bench", "assistantDelta", &payload).unwrap();
        }
        let append = t0.elapsed();

        let t1 = Instant::now();
        let rows = s.all_events("bench").unwrap();
        let read = t1.elapsed();

        assert_eq!(rows.len(), n);
        assert_eq!(s.latest_seq("bench").unwrap(), n as i64);
        let per_append_us = append.as_micros() as f64 / n as f64;
        eprintln!(
            "[bench] {n} appends in {append:?} ({per_append_us:.1} µs/append); rehydrate {n} in {read:?}"
        );
        // Generous floor: even on a slow CI box this should be well under here.
        assert!(
            append.as_secs_f64() < 10.0,
            "append throughput regressed: {append:?} for {n}"
        );
    }

    // ── events ───────────────────────────────────────────────────────────

    #[test]
    fn append_event_assigns_seq_1_first() {
        let mut s = Storage::open_in_memory().unwrap();
        let seq = s.append_event("s1", "kind", "{}").unwrap();
        assert_eq!(seq, 1);
    }

    #[test]
    fn append_event_increments_seq() {
        let mut s = Storage::open_in_memory().unwrap();
        assert_eq!(s.append_event("s1", "a", "{}").unwrap(), 1);
        assert_eq!(s.append_event("s1", "b", "{}").unwrap(), 2);
        assert_eq!(s.append_event("s1", "c", "{}").unwrap(), 3);
    }

    #[test]
    fn append_events_separate_sessions_independent_seqs() {
        let mut s = Storage::open_in_memory().unwrap();
        assert_eq!(s.append_event("A", "x", "{}").unwrap(), 1);
        assert_eq!(s.append_event("B", "x", "{}").unwrap(), 1);
        assert_eq!(s.append_event("A", "y", "{}").unwrap(), 2);
        assert_eq!(s.append_event("B", "y", "{}").unwrap(), 2);
        assert_eq!(s.append_event("A", "z", "{}").unwrap(), 3);
    }

    #[test]
    fn events_after_returns_only_newer() {
        let mut s = Storage::open_in_memory().unwrap();
        for i in 0..5 {
            s.append_event("s1", "k", &format!("{{\"i\":{i}}}"))
                .unwrap();
        }
        let got = s.events_after("s1", 2).unwrap();
        let seqs: Vec<_> = got.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![3, 4, 5]);
    }

    #[test]
    fn events_after_seq_zero_returns_all() {
        let mut s = Storage::open_in_memory().unwrap();
        for _ in 0..4 {
            s.append_event("s1", "k", "{}").unwrap();
        }
        let got = s.events_after("s1", 0).unwrap();
        assert_eq!(got.len(), 4);
        assert_eq!(got.first().unwrap().seq, 1);
        assert_eq!(got.last().unwrap().seq, 4);
    }

    #[test]
    fn all_events_ordered_by_seq() {
        let mut s = Storage::open_in_memory().unwrap();
        for _ in 0..6 {
            s.append_event("s1", "k", "{}").unwrap();
        }
        let got = s.all_events("s1").unwrap();
        let seqs: Vec<_> = got.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn all_events_scoped_to_session() {
        let mut s = Storage::open_in_memory().unwrap();
        s.append_event("A", "k", "{}").unwrap();
        s.append_event("B", "k", "{}").unwrap();
        s.append_event("A", "k", "{}").unwrap();
        assert_eq!(s.all_events("A").unwrap().len(), 2);
        assert_eq!(s.all_events("B").unwrap().len(), 1);
    }

    #[test]
    fn event_payload_roundtrips() {
        let mut s = Storage::open_in_memory().unwrap();
        s.append_event("s1", "tool", "{\"name\":\"grep\"}").unwrap();
        let got = s.all_events("s1").unwrap();
        assert_eq!(got[0].kind, "tool");
        assert_eq!(got[0].payload_json, "{\"name\":\"grep\"}");
        assert_eq!(got[0].session_id, "s1");
    }

    #[test]
    fn latest_seq_empty_is_zero() {
        let s = Storage::open_in_memory().unwrap();
        assert_eq!(s.latest_seq("s1").unwrap(), 0);
    }

    #[test]
    fn latest_seq_after_writes() {
        let mut s = Storage::open_in_memory().unwrap();
        for _ in 0..7 {
            s.append_event("s1", "k", "{}").unwrap();
        }
        assert_eq!(s.latest_seq("s1").unwrap(), 7);
    }

    #[test]
    fn event_log_replay_600() {
        let mut s = Storage::open_in_memory().unwrap();
        for i in 1..=600 {
            let seq = s
                .append_event("s1", "k", &format!("{{\"n\":{i}}}"))
                .unwrap();
            assert_eq!(seq, i, "seq must be contiguous");
        }
        let replayed = s.events_after("s1", 100).unwrap();
        assert_eq!(replayed.len(), 500);
        assert_eq!(replayed.first().unwrap().seq, 101);
        assert_eq!(replayed.last().unwrap().seq, 600);
        // Confirm strictly contiguous and ascending.
        for (idx, ev) in replayed.iter().enumerate() {
            assert_eq!(ev.seq, 101 + idx as i64);
        }
    }

    // ── tasks ────────────────────────────────────────────────────────────

    #[test]
    fn upsert_task_and_get() {
        let s = Storage::open_in_memory().unwrap();
        s.upsert_task("s1", &task("t1", "first")).unwrap();
        s.upsert_task("s1", &task("t2", "second")).unwrap();
        let tasks = s.get_tasks("s1").unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].id, "t1");
        assert_eq!(tasks[0].title, "first");
        assert_eq!(tasks[1].id, "t2");
    }

    #[test]
    fn upsert_task_overwrites() {
        let s = Storage::open_in_memory().unwrap();
        s.upsert_task("s1", &task("t1", "old")).unwrap();
        let mut updated = task("t1", "new");
        updated.state = TaskState::Done;
        s.upsert_task("s1", &updated).unwrap();
        let tasks = s.get_tasks("s1").unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "new");
        assert_eq!(tasks[0].state, TaskState::Done);
    }

    #[test]
    fn tasks_scoped_to_session() {
        let s = Storage::open_in_memory().unwrap();
        s.upsert_task("A", &task("t1", "a")).unwrap();
        s.upsert_task("B", &task("t1", "b")).unwrap();
        assert_eq!(s.get_tasks("A").unwrap().len(), 1);
        assert_eq!(s.get_tasks("A").unwrap()[0].title, "a");
        assert_eq!(s.get_tasks("B").unwrap()[0].title, "b");
    }

    // ── artifacts ────────────────────────────────────────────────────────

    #[test]
    fn upsert_and_get_artifact() {
        let s = Storage::open_in_memory().unwrap();
        s.upsert_artifact("s1", &artifact("a1", "src/main.rs"))
            .unwrap();
        let got = s.get_artifact("a1").unwrap().unwrap();
        assert_eq!(got.id, "a1");
        assert_eq!(got.path, "src/main.rs");
    }

    #[test]
    fn get_missing_artifact_is_none() {
        let s = Storage::open_in_memory().unwrap();
        assert!(s.get_artifact("ghost").unwrap().is_none());
    }

    #[test]
    fn upsert_artifact_overwrites() {
        let s = Storage::open_in_memory().unwrap();
        s.upsert_artifact("s1", &artifact("a1", "p")).unwrap();
        let mut updated = artifact("a1", "p");
        updated.shippable = true;
        s.upsert_artifact("s1", &updated).unwrap();
        assert!(s.get_artifact("a1").unwrap().unwrap().shippable);
        assert_eq!(s.list_artifacts("s1").unwrap().len(), 1);
    }

    #[test]
    fn get_artifact_by_path() {
        let s = Storage::open_in_memory().unwrap();
        s.upsert_artifact("s1", &artifact("a1", "src/lib.rs"))
            .unwrap();
        s.upsert_artifact("s1", &artifact("a2", "src/main.rs"))
            .unwrap();
        let got = s
            .get_artifact_by_path("s1", "src/main.rs")
            .unwrap()
            .unwrap();
        assert_eq!(got.id, "a2");
        assert!(s.get_artifact_by_path("s1", "nope").unwrap().is_none());
    }

    #[test]
    fn list_artifacts_for_session() {
        let s = Storage::open_in_memory().unwrap();
        s.upsert_artifact("A", &artifact("a1", "p1")).unwrap();
        s.upsert_artifact("A", &artifact("a2", "p2")).unwrap();
        s.upsert_artifact("B", &artifact("b1", "p3")).unwrap();
        let a = s.list_artifacts("A").unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(s.list_artifacts("B").unwrap().len(), 1);
    }

    #[test]
    fn artifact_trust_label_round_trips() {
        let s = Storage::open_in_memory().unwrap();
        let mut a = artifact("a1", "p1");
        a.trust_label = "approved".to_string();
        s.upsert_artifact("s1", &a).unwrap();
        let got = s.get_artifact("a1").unwrap().unwrap();
        assert_eq!(got.trust_label, "approved");
        // Overwrite with a different label.
        let mut updated = artifact("a1", "p1");
        updated.trust_label = "rejected".to_string();
        s.upsert_artifact("s1", &updated).unwrap();
        let got = s.get_artifact("a1").unwrap().unwrap();
        assert_eq!(got.trust_label, "rejected");
    }

    // ── memory ───────────────────────────────────────────────────────────

    #[test]
    fn write_and_get_memory() {
        let s = Storage::open_in_memory().unwrap();
        let row = mem("m1", "hot", 0.5);
        s.write_memory(&row).unwrap();
        assert_eq!(s.get_memory("m1").unwrap().unwrap(), row);
        assert!(s.get_memory("ghost").unwrap().is_none());
    }

    #[test]
    fn query_memory_by_tier_ordered_by_score() {
        let s = Storage::open_in_memory().unwrap();
        s.write_memory(&mem("m1", "hot", 0.2)).unwrap();
        s.write_memory(&mem("m2", "hot", 0.9)).unwrap();
        s.write_memory(&mem("m3", "hot", 0.5)).unwrap();
        s.write_memory(&mem("c1", "cold", 0.99)).unwrap();
        let hot = s.query_memory_by_tier("hot").unwrap();
        let ids: Vec<_> = hot.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["m2", "m3", "m1"]);
        // cold not included
        assert!(hot.iter().all(|m| m.tier == "hot"));
    }

    #[test]
    fn top_memories_limit() {
        let s = Storage::open_in_memory().unwrap();
        s.write_memory(&mem("m1", "a", 0.1)).unwrap();
        s.write_memory(&mem("m2", "b", 0.8)).unwrap();
        s.write_memory(&mem("m3", "c", 0.5)).unwrap();
        s.write_memory(&mem("m4", "d", 0.95)).unwrap();
        let top = s.top_memories(2).unwrap();
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].id, "m4");
        assert_eq!(top[1].id, "m2");
    }

    #[test]
    fn write_memory_overwrites() {
        let s = Storage::open_in_memory().unwrap();
        s.write_memory(&mem("m1", "a", 0.1)).unwrap();
        s.write_memory(&mem("m1", "a", 0.7)).unwrap();
        let got = s.get_memory("m1").unwrap().unwrap();
        assert_eq!(got.score, 0.7);
        assert_eq!(s.top_memories(10).unwrap().len(), 1);
    }
}
