//! rightkit-agent-engine-storage — SQLite-backed persistence for the host app sessions,
//! the append-only event log, tasks and artifacts.

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
pub mod migrations;
mod remote;
mod run_budget_state;
mod session;
mod soak_health;
mod tasks;
mod tool_invocations;
mod transcript_export;
#[cfg(feature = "workflow")]
mod workflow;

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
#[cfg(feature = "workflow")]
pub use workflow::{
    parse_payload_json, HmacAdmittedTriggerEnvelope, JournalAppend, JournalAppendOutcome,
    LeaseRelease, LeaseUpdate, OccurrenceAdmission, ProjectionUpdate, TriggerAdmissionOutcome,
    TriggerEventInsert, TriggerRunAdmission, WorkflowApprovalDecision, WorkflowApprovalRow,
    WorkflowAttemptRow, WorkflowAttemptUpdate, WorkflowAttentionRow, WorkflowClaimEvidenceRow,
    WorkflowClaimRow, WorkflowCommandApplyOutcome, WorkflowCommandBoundary, WorkflowCommandRow,
    WorkflowDefinitionRow, WorkflowDefinitionWriter, WorkflowDeliveryRow, WorkflowEffectClaim,
    WorkflowEffectOutcome, WorkflowEffectRow, WorkflowEvidenceRow, WorkflowImportQuarantineRow,
    WorkflowJournalRow, WorkflowJournalWriter, WorkflowMonitoringReceiptRow, WorkflowNodeStateRow,
    WorkflowOccurrenceRow, WorkflowOutboxRow, WorkflowPromotionReceiptRow, WorkflowQueueRow,
    WorkflowRepository, WorkflowRunLeaseRow, WorkflowRunRow, WorkflowTriggerArtifactPut,
    WorkflowTriggerArtifactStore, WorkflowTriggerEventRow, WorkflowTriggerRawArtifactRow,
    WorkflowTriggerRawArtifactWrite, WorkflowTriggerRow, WorkflowTriggerWriter, WorkflowVersionRow,
    WorkflowWaitRow,
};

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

/// Whether two database paths name the same location once symlinked directories are resolved (the file itself
/// may not exist yet). SQLite reports the resolved path, so a plain comparison misreads macOS's /var -> /private/var.
fn same_location(a: &Path, b: &Path) -> bool {
    let resolve = |p: &Path| -> std::path::PathBuf {
        std::fs::canonicalize(p).unwrap_or_else(|_| match (p.parent(), p.file_name()) {
            (Some(dir), Some(name)) => std::fs::canonicalize(dir)
                .map(|dir| dir.join(name))
                .unwrap_or_else(|_| p.to_path_buf()),
            _ => p.to_path_buf(),
        })
    };
    a == b || resolve(a) == resolve(b)
}

impl Storage {
    /// Persist the daemon-wide PL-009 observation ledger as one atomically
    /// replaced snapshot (the table is created by migration 33).
    pub fn save_observation_snapshot(&mut self, snapshot: &[u8]) -> Result<()> {
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
        // Deleting a session removes its bytes, not just its rows (storage rule 35).
        options.secure_delete = Some(true);
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
        // SQLite reports the resolved path (macOS: /var is /private/var). Only a real per-host fallback moves the readers.
        if !same_location(effective_path, path) {
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
