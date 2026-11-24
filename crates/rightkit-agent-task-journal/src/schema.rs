//! The journal's storage shape: the job store's configuration, the task status vocabulary on top
//! of it, the transactional hook that enforces the C0 transition table and writes the event
//! outbox, and the small metadata table (schema version, owner epoch, admission, publish cursor).
//!
//! Status mapping. The job store fixes its terminal names to `complete`, `failed` and
//! `cancelled`; the C0 names are `succeeded`, `failed` and `cancelled`. `complete` is stored and read
//! back as `succeeded`; every other status is stored under its C0 name (`waiting-user`,
//! `cancel-requested`, ...). The store's own model only knows "non-terminal to non-terminal pairs
//! allowed, anything to terminal allowed"; the hook below applies `rightkit_agent_task_contract::transition` to
//! every status change inside the same SQLite transaction, so a move the C0 table forbids rolls
//! back and nothing is written.

use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rightkit_agent_task_contract::{is_legal_transition, transition, TaskStatus};
use rightkit_db::{
    migrate, migration_version, open_read_only, stored_version_is_newer, DbError, MigrateOptions,
    Migration, VersionStorage,
};
use rightkit_job_queue::rusqlite::{params, Connection, OptionalExtension};
use rightkit_job_queue::{
    ExtColumn, JobError, JobRecord, JobResult, StatusModel, StoreConfig, StoreHook,
};
use serde_json::{json, Value};

use crate::host::JournalHost;
use crate::link::{self, PendingLink};

/// Version of the journal's tables and of the meaning of their columns. Recorded in
/// `pulse_journal_meta` at first open. A binary that finds a larger number refuses to open the
/// journal (`journal_schema_newer`) instead of reading state it cannot interpret.
pub const JOURNAL_SCHEMA_VERSION: u32 = 2;

pub const META_SCHEMA: &str = "schema_version";
pub const META_EPOCH: &str = "epoch";
pub const META_ADMISSION: &str = "admission";
pub const META_PUBLISHED: &str = "published_seq";

const NON_TERMINAL: [TaskStatus; 7] = [
    TaskStatus::Accepted,
    TaskStatus::Leased,
    TaskStatus::Running,
    TaskStatus::WaitingUser,
    TaskStatus::WaitingDevice,
    TaskStatus::CancelRequested,
    TaskStatus::Uncertain,
];

/// The name a status is stored under in `jobs.status`.
pub fn store_name(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Accepted => "accepted",
        TaskStatus::Leased => "leased",
        TaskStatus::Running => "running",
        TaskStatus::WaitingUser => "waiting-user",
        TaskStatus::WaitingDevice => "waiting-device",
        TaskStatus::CancelRequested => "cancel-requested",
        TaskStatus::Uncertain => "uncertain",
        TaskStatus::Succeeded => "complete",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
    }
}

/// The C0 name of a status, as the commands and events show it.
pub fn pulse_name(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Succeeded => "succeeded",
        other => store_name(other),
    }
}

/// Reads a stored `jobs.status` back as a C0 status. `None` for a name this version does not
/// know (written by a newer binary).
pub fn parse_status(stored: &str) -> Option<TaskStatus> {
    Some(match stored {
        "accepted" => TaskStatus::Accepted,
        "leased" => TaskStatus::Leased,
        "running" => TaskStatus::Running,
        "waiting-user" => TaskStatus::WaitingUser,
        "waiting-device" => TaskStatus::WaitingDevice,
        "cancel-requested" => TaskStatus::CancelRequested,
        "uncertain" => TaskStatus::Uncertain,
        "complete" => TaskStatus::Succeeded,
        "failed" => TaskStatus::Failed,
        "cancelled" => TaskStatus::Cancelled,
        _ => return None,
    })
}

/// The event topic a status change publishes: `task.<status>`.
pub fn topic_for(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Accepted => "task.accepted",
        TaskStatus::Leased => "task.leased",
        TaskStatus::Running => "task.running",
        TaskStatus::WaitingUser => "task.waiting-user",
        TaskStatus::WaitingDevice => "task.waiting-device",
        TaskStatus::CancelRequested => "task.cancel-requested",
        TaskStatus::Succeeded => "task.succeeded",
        TaskStatus::Failed => "task.failed",
        TaskStatus::Cancelled => "task.cancelled",
        TaskStatus::Uncertain => "task.uncertain",
    }
}

/// The store's vocabulary: the seven non-terminal C0 statuses and exactly the non-terminal moves
/// the C0 table allows (built from `is_legal_transition`, so the two cannot drift).
fn status_model() -> StatusModel {
    let mut model = StatusModel::new(
        store_name(TaskStatus::Accepted),
        NON_TERMINAL.iter().map(|s| store_name(*s)),
    );
    for from in NON_TERMINAL {
        for to in NON_TERMINAL {
            if from != to && is_legal_transition(from, to) {
                model = model.allow(store_name(from), store_name(to));
            }
        }
    }
    model
}

const APP_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS pulse_journal_meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS pulse_task_events (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  task_id TEXT NOT NULL,
  topic TEXT NOT NULL,
  at_ms INTEGER NOT NULL,
  status TEXT NOT NULL,
  owner_epoch INTEGER NOT NULL,
  attempt INTEGER NOT NULL,
  detail TEXT
);
CREATE INDEX IF NOT EXISTS pulse_task_events_task ON pulse_task_events(task_id, seq);
CREATE INDEX IF NOT EXISTS pulse_task_utterance ON jobs(utterance_id, utterance_generation);
";

/// The device-link request replay memory (`link.rs`), part of the one schema.
pub(crate) const LINK_REQUESTS_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS pulse_link_requests (
  request_id TEXT PRIMARY KEY,
  device_id TEXT NOT NULL,
  pairing_id TEXT NOT NULL,
  body_sha256 TEXT NOT NULL,
  issued_at_ms INTEGER NOT NULL,
  expires_at_ms INTEGER NOT NULL,
  outcome TEXT NOT NULL,
  task_id TEXT,
  reason TEXT,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS pulse_link_requests_expiry ON pulse_link_requests(expires_at_ms);
";

/// The job store's configuration for the journal. `pending` is the link request the
/// acceptance in progress belongs to (see `link`); the hook writes its row in the
/// acceptance transaction. `host` names the route of a kind for its events.
pub(crate) fn store_config(pending: PendingLink, host: Arc<dyn JournalHost>) -> StoreConfig {
    StoreConfig::new()
        .statuses(status_model())
        // Fencing epoch of the engine process that holds the task's lease.
        .column(ExtColumn::integer("owner_epoch").not_null().default(0))
        .column(ExtColumn::text("utterance_id"))
        .column(ExtColumn::integer("utterance_generation"))
        .column(ExtColumn::text("device_id"))
        .column(ExtColumn::integer("deadline_ms"))
        // Reason for the latest transition. Every transition the journal writes sets it (to
        // null when there is none), so the hook never reads a stale value.
        .column(ExtColumn::text("note"))
        // The journal's own version is `pulse_journal_meta`; the pragma is left alone so an
        // older binary cannot write a smaller number over a newer journal's.
        .user_version(None)
        .hook(Arc::new(JournalHook { pending, host }))
        .app_schema(format!("{APP_SCHEMA}{LINK_REQUESTS_SCHEMA}"))
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

// ---- metadata -----------------------------------------------------------------------------

pub(crate) fn meta_get(conn: &Connection, key: &str) -> JobResult<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT value FROM pulse_journal_meta WHERE key=?1",
            [key],
            |r| r.get::<_, String>(0),
        )
        .optional()?)
}

pub(crate) fn meta_set(conn: &Connection, key: &str, value: &str) -> JobResult<()> {
    conn.execute(
        "INSERT INTO pulse_journal_meta (key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// The journal's schema version, recorded by RightKit's migration owner (`rightkit-db`) with the
/// version kept in `pulse_journal_meta` under `schema_version`. The tables are `APP_SCHEMA` (with
/// `LINK_REQUESTS_SCHEMA`), which the store creates at every open (`IF NOT EXISTS`), so neither
/// step runs anything. The number is 2 because journals made before the 2026-10-23 port
/// (2f6ac1b0) recorded 2 for these same tables; that port wrote 1, and every engine since refused
/// those journals as newer (`journal_schema_newer` on the Mac, the Dell and the box, #500).
const MIGRATIONS: &[Migration] = &[
    Migration::code(1, "journal_v1", |_| Ok(())),
    Migration::code(2, "journal_v2_link_requests", |_| Ok(())),
];

const _: () = assert!(MIGRATIONS[MIGRATIONS.len() - 1].version == JOURNAL_SCHEMA_VERSION as i64);

fn version_storage() -> VersionStorage {
    VersionStorage::Table {
        table: "pulse_journal_meta".into(),
        key: META_SCHEMA.into(),
    }
}

fn schema_error(error: DbError) -> JobError {
    match error {
        DbError::NewerDatabase { current, max } => JobError::new(
            "journal_schema_newer",
            format!("the journal is schema {current}; this engine reads schema {max}"),
        ),
        DbError::InvalidMigrations(why) => JobError::new(
            "journal_schema_unreadable",
            format!("journal schema version is unreadable: {why}"),
        ),
        other => JobError::from(other),
    }
}

/// Refuses a journal written by a newer binary. Called BEFORE the store opens the file, because
/// the store creates and alters tables at open: a newer journal must not see any DDL from this
/// engine. Reads only. An existing file that cannot be opened here is an error (the store's open
/// is never reached with an unknown version).
pub(crate) fn refuse_newer(path: &Path) -> JobResult<()> {
    if !path.exists() {
        return Ok(());
    }
    // An existing file that cannot be opened to read its version is a failure, not a pass: the
    // store's open that follows would otherwise run DDL on a file whose version is unknown.
    let conn = open_read_only(path).map_err(schema_error)?;
    // The owner's read-only newer-version test (nothing is migrated or written); the version is
    // read again only to name it in the refusal.
    if stored_version_is_newer(&conn, &version_storage(), MIGRATIONS).map_err(schema_error)? {
        let stored = migration_version(&conn, &version_storage()).map_err(schema_error)?;
        return Err(schema_error(DbError::NewerDatabase {
            current: stored,
            max: MIGRATIONS.last().map_or(0, |m| m.version),
        }));
    }
    Ok(())
}

/// Raises the journal to the current version (the first open of a new journal runs every step),
/// one transaction per step. A newer version is refused again here, before any step.
pub(crate) fn migrate_journal(path: &Path, conn: &Connection) -> JobResult<()> {
    let options = MigrateOptions {
        version_storage: version_storage(),
        backup: false,
        optimize_after: false,
        ..MigrateOptions::default()
    };
    migrate(conn, Some(path), MIGRATIONS, &options)
        .map(|_| ())
        .map_err(schema_error)
}

/// The next owner epoch: one more than any epoch a previous engine process took. Committed
/// before the process leases anything, so a lease of an older epoch is provably from a process
/// that is no longer the owner.
pub(crate) fn bump_epoch(conn: &Connection) -> JobResult<u64> {
    let current: u64 = match meta_get(conn, META_EPOCH)? {
        Some(text) => text.parse().map_err(|_| {
            JobError::new(
                "journal_schema_unreadable",
                format!("owner epoch {text:?} is not a number"),
            )
        })?,
        None => 0,
    };
    let next = current + 1;
    meta_set(conn, META_EPOCH, &next.to_string())?;
    Ok(next)
}

// ---- events -------------------------------------------------------------------------------

/// Appends one lifecycle event row on `conn` (inside the caller's transaction). `detail` holds
/// codes and counts only: never task content, file names or messages.
pub(crate) fn insert_event(
    conn: &Connection,
    task_id: &str,
    topic: &str,
    status: &str,
    owner_epoch: i64,
    attempt: i64,
    detail: &Value,
) -> JobResult<()> {
    conn.execute(
        "INSERT INTO pulse_task_events (task_id, topic, at_ms, status, owner_epoch, attempt, detail) \
         VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            task_id,
            topic,
            now_ms(),
            status,
            owner_epoch,
            attempt,
            detail.to_string()
        ],
    )?;
    Ok(())
}

/// Adds `route` to an event's `detail` for a kind that has one (`agent.request`). Content-free.
pub(crate) fn add_route(host: &dyn JournalHost, detail: &mut Value, kind: &str) {
    if let (Some(route), Value::Object(map)) = (host.kind(kind).and_then(|k| k.route), detail) {
        map.insert("route".into(), json!(route));
    }
}

/// Adds `origin_kind` to an event's `detail`: the kind of principal that submitted the task
/// (`owner`, `local-client`, `paired-device`, ...), read from the stored envelope. A word from a
/// closed vocabulary, never a subject id or content. A notch shows an answer only for its own
/// owner's requests, so it keys on this.
pub(crate) fn add_origin_kind(detail: &mut Value, request: &Value) {
    if let (Some(kind), Value::Object(map)) = (
        request
            .pointer("/envelope/origin/kind")
            .and_then(Value::as_str),
        detail,
    ) {
        map.insert("origin_kind".into(), json!(kind));
    }
}

pub(crate) fn ext_i64(record: &JobRecord, name: &str) -> i64 {
    record.ext.get(name).and_then(Value::as_i64).unwrap_or(0)
}

/// Runs in the same SQLite transaction as every journal write.
struct JournalHook {
    pending: PendingLink,
    host: Arc<dyn JournalHost>,
}

impl StoreHook for JournalHook {
    /// ACCEPTANCE: the task row and its `task.accepted` event commit together.
    fn on_create(&self, conn: &Connection, job: &JobRecord) -> JobResult<()> {
        let mut detail = json!({
            "kind": job.job.kind,
            "target_device": job.ext.get("device_id").cloned().unwrap_or(Value::Null),
        });
        add_route(self.host.as_ref(), &mut detail, &job.job.kind);
        add_origin_kind(&mut detail, &job.job.request);
        insert_event(
            conn,
            &job.job.job_id,
            topic_for(TaskStatus::Accepted),
            store_name(TaskStatus::Accepted),
            0,
            0,
            &detail,
        )?;
        // A device-link request's replay row commits with the task, or neither does. The unique
        // key makes a second concurrent delivery fail here, which rolls its task back.
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(entry) = pending {
            link::insert_accepted(conn, &entry, &job.job.job_id)?;
        }
        Ok(())
    }

    fn on_transition(
        &self,
        conn: &Connection,
        before: &JobRecord,
        after: &JobRecord,
    ) -> JobResult<()> {
        let unknown = |name: &str| {
            JobError::new(
                "illegal_transition",
                format!("unknown task status {name:?} on {}", after.job.job_id),
            )
        };
        let from = parse_status(&before.job.status).ok_or_else(|| unknown(&before.job.status))?;
        let to = parse_status(&after.job.status).ok_or_else(|| unknown(&after.job.status))?;
        transition(from, to).map_err(|e| JobError::new("illegal_transition", e.to_string()))?;
        let mut detail = json!({
            "kind": after.job.kind,
            "from": pulse_name(from),
            "note": after.ext.get("note").cloned().unwrap_or(Value::Null),
            "error": after.job.error.as_ref().and_then(|e| e.get("code")).cloned().unwrap_or(Value::Null),
        });
        add_route(self.host.as_ref(), &mut detail, &after.job.kind);
        add_origin_kind(&mut detail, &after.job.request);
        insert_event(
            conn,
            &after.job.job_id,
            topic_for(to),
            store_name(to),
            ext_i64(after, "owner_epoch"),
            after.job.attempts,
            &detail,
        )
    }
}
