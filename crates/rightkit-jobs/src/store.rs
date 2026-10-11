use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rightkit_db::rusqlite::{self, params, params_from_iter, Connection, OptionalExtension, Row};
use rightkit_db::{column_exists, write_txn, DbOptions, DurabilityProfile, RecoveryPolicy};
use serde_json::{json, Value};

use crate::config::{Ext, StatusModel, StoreConfig};
use crate::error::{JobError, JobResult};

pub const QUEUED: &str = "queued";
pub const RUNNING: &str = "running";
pub const COMPLETE: &str = "complete";
pub const FAILED: &str = "failed";
pub const CANCELLED: &str = "cancelled";

/// Progress events kept per job (oldest are dropped).
pub const MAX_EVENTS: i64 = 200;

/// Crate-owned side table: one row per counted external submission attempt.
pub const SUBMISSIONS_TABLE: &str = "rk_job_submissions";

mod delete;
pub use delete::{DeleteOptions, DeleteReport, DELETING_TABLE, PATH_OUTSIDE_ROOT};

const CORE_SCHEMA: &str = "
CREATE INDEX IF NOT EXISTS jobs_status ON jobs(status);
CREATE TABLE IF NOT EXISTS job_events (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  job_id TEXT NOT NULL,
  at TEXT NOT NULL,
  status TEXT NOT NULL,
  stage TEXT NOT NULL,
  attempt INTEGER NOT NULL,
  progress REAL,
  message TEXT
);
CREATE INDEX IF NOT EXISTS job_events_job ON job_events(job_id, seq);
";

const SUBMISSIONS_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS rk_job_submissions (
  job_id TEXT NOT NULL,
  attempt INTEGER NOT NULL,
  idempotency_key TEXT NOT NULL,
  state TEXT NOT NULL,
  external_id TEXT,
  error_json TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (job_id, attempt)
);
";

/// Crate-owned side table: per-job recovery class and single-flight key. A job without a row is
/// `Recovery::Resume` with no key, which is every job written before this table existed.
pub const META_TABLE: &str = "rk_job_meta";

const META_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS rk_job_meta (
  job_id TEXT PRIMARY KEY,
  recovery TEXT NOT NULL DEFAULT 'resume',
  flight_key TEXT
);
CREATE INDEX IF NOT EXISTS rk_job_meta_flight ON rk_job_meta(flight_key) WHERE flight_key IS NOT NULL;
";

/// What may happen to a job after its runner is lost (crash, or a lock that is free again while
/// the job is still non-terminal). Chosen at submit and stored with the job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Recovery {
    /// Run the job again from `Runner::resume_recoverable` / `Runner::run` (the behaviour of
    /// every earlier version).
    #[default]
    Resume,
    /// Never hand the job to a handler a second time. Once an attempt has begun, a lost runner
    /// (or a retryable handler error) fails the job with the `interrupted` error; for effects
    /// that must not repeat: uninstall, cleanup apply, package upgrades. A job that never began
    /// an attempt still runs normally.
    Never,
}

impl Recovery {
    fn as_str(self) -> &'static str {
        match self {
            Recovery::Resume => "resume",
            Recovery::Never => "never",
        }
    }
    /// An unknown stored value (written by a newer version) reads as the safe class.
    fn parse(s: &str) -> Self {
        match s {
            "resume" => Recovery::Resume,
            _ => Recovery::Never,
        }
    }
}

/// Terminal jobs to drop. At least one limit is required; a job matching either limit goes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrunePolicy {
    older_than: Option<Duration>,
    keep_latest: Option<usize>,
    keep_total: Option<usize>,
}

impl PrunePolicy {
    pub fn new() -> Self {
        Self::default()
    }
    /// Drop terminal jobs whose `updated_at` is older than `d`.
    pub fn older_than(mut self, d: Duration) -> Self {
        self.older_than = Some(d);
        self
    }
    /// Keep only the `n` newest terminal jobs (newest by `created_at`, as in `list`).
    pub fn keep_latest(mut self, n: usize) -> Self {
        self.keep_latest = Some(n);
        self
    }
    /// Bound the whole table to `n` rows: keep `n` minus the number of non-terminal jobs of the
    /// newest terminal rows, so active jobs are never dropped and never crowd the total past
    /// `n`. Inside [`SubmitOptions::prune`] the incoming row counts as a non-terminal job. A
    /// table already holding `n` or more non-terminal jobs keeps no terminal row.
    pub fn keep_total(mut self, n: usize) -> Self {
        self.keep_total = Some(n);
        self
    }
}

/// How an operator or an outside check settles a held job (see [`JobStore::resolve`]).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Resolution {
    /// The outcome is known to be "not submitted": mark the submission failed, clear the
    /// external id and move the job to the given status so a new attempt can be counted.
    Resubmit,
    /// Give up: the job ends `failed` with this error.
    Abandon(JobError),
    /// The outside system did the work: the job ends `complete` with this result.
    Adopt(Value),
}

/// What a prune removed. Artifact files are never touched by `prune`: the crate indexes files the
/// handler wrote and does not own them, so the caller deletes `artifacts[..].path` if it wants
/// them gone. (`delete` removes them when the store has roots configured.)
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PruneReport {
    /// Ids of the removed jobs.
    pub jobs: Vec<String>,
    /// Progress events removed.
    pub events: usize,
    /// Artifact rows removed, with the paths of their files.
    pub artifacts: Vec<Artifact>,
    /// Rows removed from the submissions side table.
    pub submissions: usize,
}

/// Options for [`JobStore::submit_with`].
#[derive(Debug, Clone, Default)]
pub struct SubmitOptions {
    pub recovery: Recovery,
    /// Latest-wins key: this submit replaces every non-terminal job submitted with the same key.
    pub single_flight: Option<String>,
    /// Prune terminal jobs in the submit transaction, before the new job is inserted.
    pub prune: Option<PrunePolicy>,
}

impl SubmitOptions {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn recovery(mut self, r: Recovery) -> Self {
        self.recovery = r;
        self
    }
    /// Single-flight, latest-wins: see [`JobStore::submit_with`].
    pub fn single_flight(mut self, key: impl Into<String>) -> Self {
        self.single_flight = Some(key.into());
        self
    }
    pub fn prune(mut self, p: PrunePolicy) -> Self {
        self.prune = Some(p);
        self
    }
}

/// Result of [`JobStore::submit_with`].
#[derive(Debug, Clone)]
pub struct Submitted {
    pub record: JobRecord,
    /// The `request_id` already existed; nothing else happened.
    pub deduplicated: bool,
    /// Jobs the single-flight key replaced (now `cancelled` with error code `superseded`).
    pub superseded: Vec<String>,
    pub pruned: PruneReport,
}

/// In-memory cancel flag of one job: the fast path. `true` is certain; `false` only says no
/// cancel was seen in this process. The database status stays the source of truth (another
/// process cancels through it), so a handler that must notice a cross-process cancel also polls
/// [`crate::JobControl::is_cancelled`] or [`crate::JobControl::step_boundary`], which read the
/// database and set this flag when they find the job cancelled.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// One atomic load.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    /// The shared flag, for code that takes an `Arc<AtomicBool>` (e.g. a media cancellation).
    pub fn flag(&self) -> Arc<AtomicBool> {
        self.0.clone()
    }
    pub(crate) fn set(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Counters of [`JobStore::progress_fenced`] since the store opened.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProgressStats {
    /// Progress events written (flushed ticks included).
    pub written: u64,
    /// Ticks replaced by a newer one before they were written.
    pub coalesced: u64,
    /// Events written with relaxed durability.
    pub relaxed_writes: u64,
}

#[derive(Debug)]
struct Pending {
    generation: i64,
    stage: String,
    progress: Option<f64>,
    message: Option<String>,
}

#[derive(Debug, Default)]
struct ProgState {
    last_write: Option<Instant>,
    pending: Option<Pending>,
}

/// Current UTC time as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    format_utc(secs)
}

/// Format Unix seconds as `YYYY-MM-DDTHH:MM:SSZ` (proleptic Gregorian, UTC).
fn format_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil-from-days (Howard Hinnant): days since 1970-01-01 -> y/m/d.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

#[derive(Debug, Clone)]
pub struct Job {
    pub job_id: String,
    pub request_id: Option<String>,
    pub kind: String,
    pub request: Value,
    pub status: String,
    pub attempts: i64,
    pub error: Option<Value>,
    pub result: Option<Value>,
    pub acked_by: Option<String>,
    pub acked_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl Job {
    pub fn is_terminal(&self) -> bool {
        StatusModel::is_terminal(&self.status)
    }
    /// Wire record for status/list. Requests must not carry secrets.
    pub fn to_value(&self) -> Value {
        json!({
            "job_id": self.job_id, "request_id": self.request_id, "kind": self.kind,
            "request": self.request, "status": self.status, "attempts": self.attempts,
            "error": self.error, "acked_by": self.acked_by, "acked_at": self.acked_at,
            "created_at": self.created_at, "updated_at": self.updated_at,
        })
    }
}

/// A job plus its declared extension columns.
#[derive(Debug, Clone)]
pub struct JobRecord {
    pub job: Job,
    pub ext: Ext,
}

impl JobRecord {
    /// [`Job::to_value`] with every extension column as a top-level field.
    pub fn to_value(&self) -> Value {
        let mut v = self.job.to_value();
        for (k, x) in &self.ext {
            v[k] = x.clone();
        }
        v
    }
    pub fn ext_str(&self, name: &str) -> Option<&str> {
        self.ext.get(name).and_then(Value::as_str)
    }
    pub fn ext_f64(&self, name: &str) -> Option<f64> {
        self.ext.get(name).and_then(Value::as_f64)
    }
}

const COLS: &str = "job_id, request_id, kind, request_json, status, attempts, error_json, result_json, acked_by, acked_at, created_at, updated_at";
const NCORE: usize = 12;

fn parse(s: Option<String>) -> Option<Value> {
    s.and_then(|x| serde_json::from_str(&x).ok())
}

fn row(r: &Row) -> rusqlite::Result<Job> {
    Ok(Job {
        job_id: r.get(0)?,
        request_id: r.get(1)?,
        kind: r.get(2)?,
        request: parse(r.get(3)?).unwrap_or(Value::Null),
        status: r.get(4)?,
        attempts: r.get(5)?,
        error: parse(r.get(6)?),
        result: parse(r.get(7)?),
        acked_by: r.get(8)?,
        acked_at: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
    })
}

#[derive(Debug, Clone)]
pub struct NewJob {
    pub request_id: Option<String>,
    pub kind: String,
    pub request: Value,
}

#[derive(Debug, Clone, Default)]
pub struct ListFilter {
    pub limit: Option<i64>,
    pub status: Option<String>,
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub seq: i64,
    pub job_id: String,
    pub at: String,
    pub status: String,
    pub stage: String,
    pub attempt: i64,
    pub progress: Option<f64>,
    pub message: Option<String>,
}

impl Event {
    pub fn to_value(&self) -> Value {
        let mut v = json!({
            "event": "job.progress", "seq": self.seq, "job_id": self.job_id,
            "status": self.status, "stage": self.stage, "attempt": self.attempt, "at": self.at,
        });
        if let Some(p) = self.progress {
            v["progress"] = json!(p);
        }
        if let Some(m) = &self.message {
            v["message"] = json!(m);
        }
        v
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub artifact_id: String,
    pub job_id: String,
    pub path: String,
    pub sha256: String,
    pub bytes: i64,
    pub mime: String,
}

impl Artifact {
    /// Default wire shape (`artifact_id`). See [`JobStore::artifact_value`]
    /// for the configured wire id.
    pub fn to_value(&self) -> Value {
        json!({"artifact_id": self.artifact_id, "job_id": self.job_id, "path": self.path,
               "sha256": self.sha256, "bytes": self.bytes, "mime": self.mime})
    }
}

/// A requested store change, validated against the [`StatusModel`] and run
/// with the [`crate::StoreHook`] in one write transaction.
#[derive(Debug, Clone, Default)]
pub struct Transition {
    /// New status (`None`: keep the current one).
    pub to: Option<String>,
    /// Count one attempt (fires `StoreHook::on_attempt`).
    pub count_attempt: bool,
    /// Extension columns to set.
    pub ext: Ext,
    /// `Some(None)` clears the stored error, `Some(Some(v))` sets it.
    pub error: Option<Option<Value>>,
    pub result: Option<Value>,
}

impl Transition {
    pub fn to(status: impl Into<String>) -> Self {
        Self {
            to: Some(status.into()),
            ..Self::default()
        }
    }
    pub fn keep() -> Self {
        Self::default()
    }
    pub fn count_attempt(mut self) -> Self {
        self.count_attempt = true;
        self
    }
    pub fn set(mut self, column: impl Into<String>, v: impl Into<Value>) -> Self {
        self.ext.insert(column.into(), v.into());
        self
    }
    pub fn error(mut self, e: Option<Value>) -> Self {
        self.error = Some(e);
        self
    }
    pub fn result(mut self, v: Value) -> Self {
        self.result = Some(v);
        self
    }
}

/// What a caller must do for the next external submission of a job (see
/// [`JobStore::begin_submission`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Submission {
    /// The external system already holds this job (stored external id):
    /// poll it; never submit again.
    Resume { external_id: String, attempt: i64 },
    /// Submit `attempt` with `idempotency_key`. `replay` is true when a
    /// previous process counted this attempt and died before recording the
    /// external id: the attempt, its key and its hook effects (e.g. spend
    /// reservation) are reused rather than repeated.
    Submit {
        attempt: i64,
        idempotency_key: String,
        replay: bool,
    },
    /// Every allowed attempt was used.
    Exhausted { attempts: i64 },
}

/// One row of the crate-owned submissions side table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionRecord {
    pub job_id: String,
    pub attempt: i64,
    pub idempotency_key: String,
    /// `pending`, `accepted`, or `failed`.
    pub state: String,
    pub external_id: Option<String>,
}

/// What opening did to an existing database (all additive).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Adoption {
    /// The `jobs` table existed before this open.
    pub existing_jobs_table: bool,
    /// Columns added, as `table.column`.
    pub added_columns: Vec<String>,
}

/// SQLite job store. One database may be shared by several processes (GUI
/// sidecar plus CLI invocations): WAL plus a busy timeout via `rightkit-db`.
pub struct JobStore {
    conn: Mutex<Connection>,
    cfg: StoreConfig,
    select: String,
    adoption: Adoption,
    /// The connection runs in WAL mode (relaxed progress durability is only safe there).
    wal: bool,
    base_sync: DurabilityProfile,
    prog: Mutex<HashMap<String, ProgState>>,
    cancels: Mutex<HashMap<String, CancelToken>>,
    st_written: AtomicU64,
    st_coalesced: AtomicU64,
    st_relaxed: AtomicU64,
}

impl JobStore {
    pub fn open(path: impl AsRef<Path>) -> JobResult<Self> {
        Self::open_configured(path, StoreConfig::default())
    }

    pub fn open_with(options: &DbOptions) -> JobResult<Self> {
        Self::open_with_config(options, StoreConfig::default())
    }

    /// [`JobStore::open`] with extension points (full durability, parent dir created).
    pub fn open_configured(path: impl AsRef<Path>, cfg: StoreConfig) -> JobResult<Self> {
        if let Some(dir) = path.as_ref().parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut options = DbOptions::new(path.as_ref());
        options.durability = DurabilityProfile::Full;
        Self::open_with_config(&options, cfg)
    }

    /// Open, creating or additively migrating the schema to `cfg`. An existing
    /// database (e.g. GenRight's) is adopted in place: missing columns are
    /// added, nothing is dropped, rewritten, or renamed.
    pub fn open_with_config(options: &DbOptions, cfg: StoreConfig) -> JobResult<Self> {
        cfg.validate()?;
        let conn = retry_busy_db(|| options.open_writer())?;
        Self::finish_open(options, cfg, conn)
    }

    /// [`JobStore::open_with_config`] that survives an unreadable database file: with a
    /// `policy` that allows it, the file is moved aside (see `DbOptions::open_writer_recovering`)
    /// and a fresh store is created at the same path. Returns the store and where the old file
    /// went (`None` when the database opened normally).
    pub fn open_recovering(
        options: &DbOptions,
        cfg: StoreConfig,
        policy: RecoveryPolicy,
    ) -> JobResult<(Self, Option<PathBuf>)> {
        cfg.validate()?;
        let outcome = retry_busy_db(|| options.open_writer_recovering(policy))?;
        let quarantined = outcome.quarantined;
        let store = Self::finish_open(options, cfg, outcome.conn)?;
        Ok((store, quarantined))
    }

    fn finish_open(options: &DbOptions, cfg: StoreConfig, mut conn: Connection) -> JobResult<Self> {
        let adoption = retry_busy(|| init_schema(&conn, &cfg))??;
        for sql in &cfg.app_schema {
            retry_busy(|| conn.execute_batch(sql))?;
        }
        if let Some(v) = cfg.user_version {
            retry_busy(|| {
                let tx = write_txn(&mut conn).map_err(db_to_sqlite)?;
                tx.pragma_update(None, "user_version", v)?;
                tx.commit()
            })?;
        }
        let wal = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
            .map(|m| m.eq_ignore_ascii_case("wal"))
            .unwrap_or(false);
        let mut select = COLS.to_string();
        for c in &cfg.columns {
            select.push_str(", ");
            select.push_str(&c.name);
        }
        Ok(Self {
            conn: Mutex::new(conn),
            cfg,
            select,
            adoption,
            wal,
            base_sync: options.durability,
            prog: Mutex::new(HashMap::new()),
            cancels: Mutex::new(HashMap::new()),
            st_written: AtomicU64::new(0),
            st_coalesced: AtomicU64::new(0),
            st_relaxed: AtomicU64::new(0),
        })
    }

    pub fn config(&self) -> &StoreConfig {
        &self.cfg
    }
    pub fn adoption(&self) -> &Adoption {
        &self.adoption
    }

    fn c(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn record_row(&self, r: &Row) -> rusqlite::Result<JobRecord> {
        let mut job = row(r)?;
        if !self.cfg.includes_retryable() {
            if let Some(Value::Object(o)) = &mut job.error {
                o.remove("retryable");
            }
        }
        let mut ext = Ext::new();
        for (i, c) in self.cfg.columns.iter().enumerate() {
            let v: rusqlite::types::Value = r.get(NCORE + i)?;
            ext.insert(c.name.clone(), c.decode(v));
        }
        Ok(JobRecord { job, ext })
    }

    fn load(&self, conn: &Connection, job_id: &str) -> JobResult<JobRecord> {
        conn.query_row(
            &format!("SELECT {} FROM jobs WHERE job_id=?1", self.select),
            [job_id],
            |r| self.record_row(r),
        )
        .optional()?
        .ok_or_else(|| JobError::new("job_not_found", format!("no such job: {job_id}")))
    }

    /// Run `f` inside one write transaction on the store's connection (app-owned
    /// tables such as a spend ledger or settings). Commits on `Ok`, rolls back on `Err`.
    pub fn write<T>(&self, f: impl FnOnce(&Connection) -> JobResult<T>) -> JobResult<T> {
        let mut conn = self.c();
        let tx = write_txn(&mut conn)?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// Read through the store's connection.
    pub fn read<T>(&self, f: impl FnOnce(&Connection) -> JobResult<T>) -> JobResult<T> {
        f(&self.c())
    }

    /// Insert a job in the initial status, or return the existing one for the
    /// same `request_id` (the mapping lives in the same transaction as the row).
    /// Returns `(job, deduplicated)`.
    pub fn submit(&self, n: &NewJob) -> JobResult<(Job, bool)> {
        self.submit_ext(n, &Ext::new()).map(|(r, d)| (r.job, d))
    }

    /// [`JobStore::submit`] with extension column values. `StoreHook::on_create`
    /// runs in the insert transaction; its error rolls the insert back.
    pub fn submit_ext(&self, n: &NewJob, ext: &Ext) -> JobResult<(JobRecord, bool)> {
        self.submit_with(n, ext, &SubmitOptions::default())
            .map(|s| (s.record, s.deduplicated))
    }

    /// [`JobStore::submit_ext`] with a recovery class, a single-flight key and/or a prune bound,
    /// all applied in the one submit transaction.
    ///
    /// - A repeated `request_id` returns the existing job (`deduplicated`) and does nothing else:
    ///   no prune, no supersede, and the stored class of the original stands.
    /// - `prune` runs before the insert (so it never sees the new job or the jobs this submit
    ///   supersedes) and reports what it removed in [`Submitted::pruned`].
    /// - `single_flight(key)`: every non-terminal job stored with the same key is cancelled in
    ///   this transaction, with error code `superseded` naming the new job, a `superseded` event,
    ///   and `StoreHook::on_transition`. A job that has not started will never reach a handler. A
    ///   running one is cancelled in the database at once and its in-process [`CancelToken`] is set
    ///   after the commit; its handler keeps running until it next checks, and whatever it
    ///   returns is discarded (the job is already terminal). Nothing waits for it to stop: take
    ///   the job's [`crate::JobLocks`] lock to know its runner has gone. The new job is returned
    ///   at once and may be run immediately.
    pub fn submit_with(&self, n: &NewJob, ext: &Ext, opts: &SubmitOptions) -> JobResult<Submitted> {
        let (names, vals) = self.encode_new_ext(ext)?;
        if opts.single_flight.as_deref() == Some("") {
            return Err(JobError::bad_input("single-flight key is empty"));
        }
        let mut conn = self.c();
        let tx = write_txn(&mut conn)?;
        if let Some(rid) = &n.request_id {
            let found = tx
                .query_row(
                    &format!("SELECT {} FROM jobs WHERE request_id=?1", self.select),
                    [rid],
                    |r| self.record_row(r),
                )
                .optional()?;
            if let Some(j) = found {
                return Ok(Submitted {
                    record: j,
                    deduplicated: true,
                    superseded: vec![],
                    pruned: PruneReport::default(),
                });
            }
        }
        let pruned = match &opts.prune {
            Some(p) => self.prune_in(&tx, p, 1)?,
            None => PruneReport::default(),
        };
        let (id, rec) = self.insert_new(&tx, n, &names, vals)?;
        if opts.recovery != Recovery::default() || opts.single_flight.is_some() {
            tx.execute(
                "INSERT INTO rk_job_meta (job_id, recovery, flight_key) VALUES (?1,?2,?3)",
                params![id, opts.recovery.as_str(), opts.single_flight],
            )?;
        }
        let mut superseded = Vec::new();
        if let Some(key) = &opts.single_flight {
            let old: Vec<String> = {
                let mut st = tx.prepare(&format!(
                    "SELECT j.job_id FROM rk_job_meta m JOIN jobs j ON j.job_id=m.job_id \
                     WHERE m.flight_key=?1 AND m.job_id<>?2 AND j.status IN {} \
                     ORDER BY j.created_at, j.rowid",
                    // A held job waits for a person: a newer submit never replaces it.
                    self.cfg.statuses.sql_set_unheld()
                ))?;
                let rows = st
                    .query_map(params![key, id], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                rows
            };
            for old_id in old {
                let before = self.load(&tx, &old_id)?;
                let err = JobError::superseded(&id);
                let t = Transition::to(CANCELLED)
                    .error(Some(err.to_json_with(self.cfg.includes_retryable())));
                self.exec(&tx, &before, &t)?;
                add_event_on(
                    &tx,
                    &old_id,
                    None,
                    "superseded",
                    None,
                    None,
                    Some(&err.message),
                )?;
                superseded.push(old_id);
            }
        }
        tx.commit()?;
        drop(conn);
        for old_id in &superseded {
            self.settle(old_id, true);
        }
        for gone in &pruned.jobs {
            self.settle(gone, false);
        }
        Ok(Submitted {
            record: rec,
            deduplicated: false,
            superseded,
            pruned,
        })
    }

    /// Validate and encode the ext values of a new job: `(column names, encoded values)`.
    fn encode_new_ext(&self, ext: &Ext) -> JobResult<(Vec<String>, Vec<rusqlite::types::Value>)> {
        let mut names = Vec::new();
        let mut vals: Vec<rusqlite::types::Value> = Vec::new();
        for (k, v) in ext {
            let c = self
                .cfg
                .column_named(k)
                .ok_or_else(|| JobError::bad_input(format!("unknown ext column: {k}")))?;
            names.push(c.name.clone());
            vals.push(c.encode(v)?);
        }
        if let Some(c) = self
            .cfg
            .columns
            .iter()
            .find(|c| c.required() && !ext.contains_key(&c.name))
        {
            return Err(JobError::bad_input(format!("ext.{} required", c.name)));
        }
        Ok((names, vals))
    }

    /// Insert a job in the initial status on `tx` and run `StoreHook::on_create`.
    fn insert_new(
        &self,
        tx: &Connection,
        n: &NewJob,
        names: &[String],
        vals: Vec<rusqlite::types::Value>,
    ) -> JobResult<(String, JobRecord)> {
        let id = uuid::Uuid::new_v4().to_string();
        let t = now();
        let mut cols =
            "job_id, request_id, kind, request_json, status, attempts, created_at, updated_at"
                .to_string();
        let mut ph = "?1,?2,?3,?4,?5,0,?6,?6".to_string();
        for (i, name) in names.iter().enumerate() {
            cols.push_str(&format!(", {name}"));
            ph.push_str(&format!(",?{}", 7 + i));
        }
        let mut all: Vec<rusqlite::types::Value> = vec![
            id.clone().into(),
            n.request_id.clone().into(),
            n.kind.clone().into(),
            n.request.to_string().into(),
            self.cfg.statuses.initial().to_string().into(),
            t.into(),
        ];
        all.extend(vals);
        tx.execute(
            &format!("INSERT INTO jobs ({cols}) VALUES ({ph})"),
            params_from_iter(all),
        )?;
        let rec = self.load(tx, &id)?;
        if let Some(h) = &self.cfg.hook {
            h.on_create(tx, &rec)?;
        }
        Ok((id, rec))
    }

    /// Record a submission that was refused before it could run (for example an entitlement
    /// check the app made first) as a `failed` job carrying `error`, so it shows in the job list
    /// with its reason. The insert, `StoreHook::on_create`, the move to `failed`
    /// (`StoreHook::on_transition`) and a `rejected` event commit in one transaction: a crash
    /// never leaves a runnable job behind. A repeated `request_id` returns the existing job
    /// unchanged. Nothing is ever handed to a handler.
    pub fn submit_rejected(&self, n: &NewJob, ext: &Ext, error: &JobError) -> JobResult<JobRecord> {
        let (names, vals) = self.encode_new_ext(ext)?;
        let mut conn = self.c();
        let tx = write_txn(&mut conn)?;
        if let Some(rid) = &n.request_id {
            let found = tx
                .query_row(
                    &format!("SELECT {} FROM jobs WHERE request_id=?1", self.select),
                    [rid],
                    |r| self.record_row(r),
                )
                .optional()?;
            if let Some(j) = found {
                return Ok(j);
            }
        }
        let (id, rec) = self.insert_new(&tx, n, &names, vals)?;
        let t =
            Transition::to(FAILED).error(Some(error.to_json_with(self.cfg.includes_retryable())));
        let after = self.exec(&tx, &rec, &t)?;
        add_event_on(&tx, &id, None, "rejected", None, None, Some(&error.message))?;
        tx.commit()?;
        drop(conn);
        self.settle(&id, false);
        Ok(after)
    }

    /// The recovery class stored with the job (`Resume` for any job without one).
    pub fn recovery(&self, job_id: &str) -> JobResult<Recovery> {
        Ok(self.run_class(job_id)?.0)
    }

    /// `(recovery class, attempt generation)`; generation 0 means no attempt ever began.
    pub(crate) fn run_class(&self, job_id: &str) -> JobResult<(Recovery, i64)> {
        self.c()
            .query_row(
                "SELECT j.attempt_gen, m.recovery FROM jobs j \
                 LEFT JOIN rk_job_meta m ON m.job_id=j.job_id WHERE j.job_id=?1",
                [job_id],
                |r| {
                    let gen: i64 = r.get(0)?;
                    let rec: Option<String> = r.get(1)?;
                    Ok((
                        rec.as_deref().map_or(Recovery::Resume, Recovery::parse),
                        gen,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| JobError::new("job_not_found", format!("no such job: {job_id}")))
    }

    /// Fail a started `Recovery::Never` job whose runner is gone: terminal `failed` with the
    /// `interrupted` error and an `interrupted` event, in one transaction with the hook.
    /// `Ok(None)` when the job is already terminal.
    pub(crate) fn interrupt(&self, job_id: &str) -> JobResult<Option<(Job, Event)>> {
        let mut conn = self.c();
        let tx = write_txn(&mut conn)?;
        let before = self.load(&tx, job_id)?;
        if before.job.is_terminal() || self.cfg.statuses.is_held(&before.job.status) {
            return Ok(None);
        }
        let err = JobError::interrupted(job_id);
        let t = Transition::to(FAILED).error(Some(err.to_json_with(self.cfg.includes_retryable())));
        let after = self.exec(&tx, &before, &t)?;
        let ev = add_event_on(
            &tx,
            job_id,
            None,
            "interrupted",
            None,
            None,
            Some(&err.message),
        )?;
        tx.commit()?;
        drop(conn);
        self.settle(job_id, false);
        Ok(Some((after.job, ev)))
    }

    /// The in-memory cancel flag of `job_id` (created on first use). [`JobStore::cancel`], a
    /// single-flight supersede and [`crate::JobControl::is_cancelled`] set it; it is dropped from
    /// the registry when the job reaches a terminal state (clones stay valid).
    pub fn cancel_token(&self, job_id: &str) -> CancelToken {
        self.cancels
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(job_id.to_string())
            .or_default()
            .clone()
    }

    pub(crate) fn release_cancel_token(&self, job_id: &str) {
        self.cancels
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(job_id);
    }

    /// A job reached a terminal state (or was removed): set its cancel flag when it was
    /// cancelled, forget its in-memory bookkeeping.
    fn settle(&self, job_id: &str, cancelled: bool) {
        let flag = self
            .cancels
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(job_id);
        if let (true, Some(t)) = (cancelled, flag) {
            t.set();
        }
        self.prog
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(job_id);
    }

    // ------------------------------------------------------------- prune ----

    /// Remove terminal jobs matching `policy` (older than `older_than`, or beyond the
    /// `keep_latest` newest) with their events, artifact rows, submission rows and recovery
    /// metadata, in one transaction; the job row and its result go last. Non-terminal jobs are
    /// never touched. Artifact files are not deleted: see [`PruneReport`]. A policy with no limit
    /// is refused (`bad_input`) rather than meaning "everything".
    pub fn prune(&self, policy: &PrunePolicy) -> JobResult<PruneReport> {
        let mut conn = self.c();
        let tx = write_txn(&mut conn)?;
        let report = self.prune_in(&tx, policy, 0)?;
        tx.commit()?;
        drop(conn);
        for id in &report.jobs {
            self.settle(id, false);
        }
        Ok(report)
    }

    /// `incoming` is the number of rows the caller is about to insert as non-terminal jobs
    /// (they count toward `keep_total`).
    fn prune_in(
        &self,
        conn: &Connection,
        policy: &PrunePolicy,
        incoming: usize,
    ) -> JobResult<PruneReport> {
        if policy.older_than.is_none()
            && policy.keep_latest.is_none()
            && policy.keep_total.is_none()
        {
            return Err(JobError::bad_input(
                "prune policy needs older_than, keep_latest and/or keep_total",
            ));
        }
        let total_room = match policy.keep_total {
            Some(n) => {
                let active: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM jobs WHERE status NOT IN ('complete','failed','cancelled')",
                    [],
                    |r| r.get(0),
                )?;
                Some(n.saturating_sub((active.max(0) as usize).saturating_add(incoming)))
            }
            None => None,
        };
        let cutoff = policy.older_than.map(|d| {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|n| n.as_secs() as i64)
                .unwrap_or(0);
            format_utc(secs.saturating_sub(d.as_secs().min(i64::MAX as u64) as i64))
        });
        let doomed: Vec<String> = {
            let mut st = conn.prepare(
                "SELECT job_id, updated_at FROM jobs WHERE status IN ('complete','failed','cancelled') \
                 ORDER BY created_at DESC, rowid DESC",
            )?;
            let rows = st
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            rows.into_iter()
                .enumerate()
                .filter(|(rank, (_, updated))| {
                    policy.keep_latest.is_some_and(|k| *rank >= k)
                        || total_room.is_some_and(|k| *rank >= k)
                        || cutoff.as_deref().is_some_and(|c| updated.as_str() < c)
                })
                .map(|(_, (id, _))| id)
                .collect()
        };
        self.remove_jobs(conn, doomed)
    }

    /// Delete `doomed` jobs with their events, artifact rows, submission rows and recovery
    /// metadata on `conn`, reporting what went; the job rows go last.
    fn remove_jobs(&self, conn: &Connection, doomed: Vec<String>) -> JobResult<PruneReport> {
        let mut report = PruneReport::default();
        for id in &doomed {
            let mut st =
                conn.prepare(&self.art_sql("WHERE job_id=?1 ORDER BY created_at, rowid"))?;
            let rows = st
                .query_map([id], artifact_row)?
                .collect::<Result<Vec<_>, _>>()?;
            report.artifacts.extend(rows);
        }
        for id in &doomed {
            report.events += conn.execute("DELETE FROM job_events WHERE job_id=?1", [id])?;
            if self.cfg.external_id.is_some() {
                report.submissions +=
                    conn.execute("DELETE FROM rk_job_submissions WHERE job_id=?1", [id])?;
            }
            conn.execute(
                &format!("DELETE FROM {} WHERE job_id=?1", self.cfg.artifacts.table),
                [id],
            )?;
            conn.execute("DELETE FROM rk_job_meta WHERE job_id=?1", [id])?;
            conn.execute("DELETE FROM rk_job_deleting WHERE job_id=?1", [id])?;
        }
        for id in &doomed {
            conn.execute("DELETE FROM jobs WHERE job_id=?1", [id])?;
        }
        report.jobs = doomed;
        Ok(report)
    }

    /// Remove one job with its events, artifact rows, submission rows and recovery metadata. When
    /// the store has artifact or scratch roots configured ([`StoreConfig::artifact_root`],
    /// [`StoreConfig::scratch_root`]) the job's artifact files and `<scratch_root>/<job_id>` go
    /// too, inside those roots only; without roots, artifact files are left for the caller (see
    /// [`PruneReport`]). Use [`JobStore::delete_with`] to keep the files or to see what was
    /// removed. A non-terminal job is first moved to `cancelled` (`StoreHook::on_transition`
    /// fires) and its in-process [`CancelToken`] is set after the commit; a running handler is not
    /// waited for, and its later fenced writes fail with `job_not_found`. A held job is refused
    /// (`illegal_transition`): resolve it first. `job_not_found` when there is no such job.
    pub fn delete(&self, job_id: &str) -> JobResult<PruneReport> {
        self.delete_with(job_id, &DeleteOptions::default())
            .map(|r| r.prune)
    }

    /// Paths of the artifacts of every non-terminal job (held ones included), oldest first and
    /// without repeats: files that must not be swept as orphans.
    pub fn claimed_paths(&self) -> JobResult<Vec<PathBuf>> {
        let conn = self.c();
        let mut st = conn.prepare(&format!(
            "SELECT a.path FROM {} a JOIN jobs j ON j.job_id=a.job_id WHERE j.status IN {} \
             ORDER BY j.created_at, j.rowid, a.created_at, a.rowid",
            self.cfg.artifacts.table,
            self.cfg.statuses.sql_set()
        ))?;
        let all = st
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut seen = std::collections::HashSet::new();
        Ok(all
            .into_iter()
            .filter(|p| seen.insert(p.clone()))
            .map(PathBuf::from)
            .collect())
    }

    pub fn get(&self, job_id: &str) -> JobResult<Job> {
        self.record(job_id).map(|r| r.job)
    }

    pub fn record(&self, job_id: &str) -> JobResult<JobRecord> {
        self.load(&self.c(), job_id)
    }

    pub fn ext(&self, job_id: &str) -> JobResult<Ext> {
        self.record(job_id).map(|r| r.ext)
    }

    pub fn get_by_request(&self, request_id: &str) -> JobResult<Job> {
        self.record_by_request(request_id).map(|r| r.job)
    }

    pub fn record_by_request(&self, request_id: &str) -> JobResult<JobRecord> {
        self.c()
            .query_row(
                &format!("SELECT {} FROM jobs WHERE request_id=?1", self.select),
                [request_id],
                |r| self.record_row(r),
            )
            .optional()?
            .ok_or_else(|| {
                JobError::new(
                    "job_not_found",
                    format!("no job for request_id: {request_id}"),
                )
            })
    }

    /// Newest-first; returns up to `limit` (default 50) rows and whether more matched.
    pub fn list(&self, f: &ListFilter) -> JobResult<(Vec<Job>, bool)> {
        let (r, t) = self.list_records(f)?;
        Ok((r.into_iter().map(|r| r.job).collect(), t))
    }

    pub fn list_records(&self, f: &ListFilter) -> JobResult<(Vec<JobRecord>, bool)> {
        let limit = f.limit.unwrap_or(50).clamp(1, 1000);
        let conn = self.c();
        let mut st = conn.prepare(&format!(
            "SELECT {} FROM jobs WHERE (?1 IS NULL OR request_id=?1) AND (?2 IS NULL OR status=?2) ORDER BY created_at DESC, rowid DESC LIMIT ?3",
            self.select
        ))?;
        let mut jobs = st
            .query_map(params![f.request_id, f.status, limit + 1], |r| {
                self.record_row(r)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let truncated = jobs.len() as i64 > limit;
        jobs.truncate(limit as usize);
        Ok((jobs, truncated))
    }

    /// Ids of non-terminal jobs, oldest first. Passive: changes nothing.
    pub fn non_terminal_ids(&self) -> JobResult<Vec<String>> {
        let conn = self.c();
        let mut st = conn.prepare(&format!(
            "SELECT job_id FROM jobs WHERE status IN {} ORDER BY created_at, rowid",
            self.cfg.statuses.sql_set()
        ))?;
        let ids = st
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ids)
    }

    /// Apply `t` to `before` on `conn` (inside the caller's transaction) and
    /// fire hooks. `before` must be non-terminal.
    fn exec(&self, conn: &Connection, before: &JobRecord, t: &Transition) -> JobResult<JobRecord> {
        self.exec_in(conn, before, t, false)
    }

    /// [`JobStore::exec`]; `force` skips the status model's move check (hold and resolve, which
    /// check their own preconditions).
    fn exec_in(
        &self,
        conn: &Connection,
        before: &JobRecord,
        t: &Transition,
        force: bool,
    ) -> JobResult<JobRecord> {
        let model = &self.cfg.statuses;
        if let Some(to) = &t.to {
            if !force && !model.can_move(&before.job.status, to) {
                return Err(JobError::new(
                    "illegal_transition",
                    format!(
                        "job {} cannot move from {} to {to}",
                        before.job.job_id, before.job.status
                    ),
                ));
            }
        }
        let mut sets = vec!["updated_at=?2".to_string()];
        let mut vals: Vec<rusqlite::types::Value> =
            vec![before.job.job_id.clone().into(), now().into()];
        let mut bind = |sets: &mut Vec<String>, col: &str, v: rusqlite::types::Value| {
            vals.push(v);
            sets.push(format!("{col}=?{}", vals.len()));
        };
        if let Some(to) = &t.to {
            bind(&mut sets, "status", to.clone().into());
        }
        if t.count_attempt {
            sets.push("attempts=attempts+1".into());
        }
        for (k, v) in &t.ext {
            let c = self
                .cfg
                .column_named(k)
                .ok_or_else(|| JobError::bad_input(format!("unknown ext column: {k}")))?;
            bind(&mut sets, &c.name, c.encode(v)?);
        }
        if let Some(e) = &t.error {
            bind(
                &mut sets,
                "error_json",
                e.as_ref().map(|v| v.to_string()).into(),
            );
        }
        if let Some(r) = &t.result {
            bind(&mut sets, "result_json", r.to_string().into());
        }
        conn.execute(
            &format!("UPDATE jobs SET {} WHERE job_id=?1", sets.join(", ")),
            params_from_iter(vals),
        )?;
        let after = self.load(conn, &before.job.job_id)?;
        if let Some(h) = &self.cfg.hook {
            if t.count_attempt {
                h.on_attempt(conn, &after, after.job.attempts)?;
            }
            if after.job.status != before.job.status {
                h.on_transition(conn, before, &after)?;
            }
        }
        Ok(after)
    }

    /// Validate and apply a transition in one write transaction with the
    /// hook. `Ok(None)` when the job is already terminal (nothing changes);
    /// `Err(illegal_transition)` for a move the status model forbids; a hook
    /// error rolls everything back.
    pub fn transition(&self, job_id: &str, t: &Transition) -> JobResult<Option<JobRecord>> {
        self.transition_in(job_id, None, t)
    }

    /// [`JobStore::transition`] for the attempt holding `generation`: rejected
    /// with `stale_attempt` (nothing written) once a newer generation exists.
    pub fn transition_fenced(
        &self,
        job_id: &str,
        generation: i64,
        t: &Transition,
    ) -> JobResult<Option<JobRecord>> {
        self.transition_in(job_id, Some(generation), t)
    }

    fn transition_in(
        &self,
        job_id: &str,
        fence: Option<i64>,
        t: &Transition,
    ) -> JobResult<Option<JobRecord>> {
        let mut conn = self.c();
        let tx = write_txn(&mut conn)?;
        check_fence(&tx, job_id, fence)?;
        let before = self.load(&tx, job_id)?;
        if before.job.is_terminal() {
            return Ok(None);
        }
        // Latest coalesced progress lands before the state change, in the same durable commit.
        self.flush_pending_in(&tx, job_id)?;
        let after = self.exec(&tx, &before, t)?;
        tx.commit()?;
        drop(conn);
        if after.job.is_terminal() {
            self.settle(job_id, after.job.status == CANCELLED);
        }
        Ok(Some(after))
    }

    /// Current attempt generation of `job_id` (0 for a job never run). Bumped by
    /// every run start (see [`JobStore::begin_run`]); survives restarts.
    pub fn generation(&self, job_id: &str) -> JobResult<i64> {
        self.c()
            .query_row(
                "SELECT attempt_gen FROM jobs WHERE job_id=?1",
                [job_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| JobError::new("job_not_found", format!("no such job: {job_id}")))
    }

    /// Supersede the attempt holding `generation` without starting a run: the
    /// generation advances, so every write still carrying the old one is
    /// rejected. Returns the new generation, which the fencing owner uses for
    /// its own writes. `stale_attempt` if `generation` is no longer current.
    pub fn fence(&self, job_id: &str, generation: i64) -> JobResult<i64> {
        let mut conn = self.c();
        let tx = write_txn(&mut conn)?;
        check_fence(&tx, job_id, Some(generation))?;
        tx.execute(
            "UPDATE jobs SET attempt_gen=attempt_gen+1, updated_at=?2 WHERE job_id=?1",
            params![job_id, now()],
        )?;
        let next = generation + 1;
        tx.commit()?;
        Ok(next)
    }

    /// Set extension columns without changing status.
    pub fn set_ext(&self, job_id: &str, ext: &Ext) -> JobResult<JobRecord> {
        self.set_ext_in(job_id, None, ext)
    }

    /// [`JobStore::set_ext`] for the attempt holding `generation` (fenced).
    pub fn set_ext_fenced(&self, job_id: &str, generation: i64, ext: &Ext) -> JobResult<JobRecord> {
        self.set_ext_in(job_id, Some(generation), ext)
    }

    fn set_ext_in(&self, job_id: &str, fence: Option<i64>, ext: &Ext) -> JobResult<JobRecord> {
        let t = Transition {
            ext: ext.clone(),
            ..Transition::default()
        };
        match self.transition_in(job_id, fence, &t)? {
            Some(r) => Ok(r),
            // Terminal jobs still accept extension data (e.g. a final cost).
            None => self.write(|conn| {
                check_fence(conn, job_id, fence)?;
                let before = self.load(conn, job_id)?;
                self.exec(conn, &before, &t)
            }),
        }
    }

    /// Mark a non-terminal job running and count the attempt. `Ok(None)` when
    /// the job is already terminal (e.g. cancelled meanwhile).
    pub fn begin_attempt(&self, job_id: &str) -> JobResult<Option<Job>> {
        Ok(self.begin_run(job_id, true)?.map(|r| r.job))
    }

    /// Start a run: move to the model's run status (if any) and optionally
    /// count an attempt, in one transaction with the hook.
    pub fn begin_run(&self, job_id: &str, count_attempt: bool) -> JobResult<Option<JobRecord>> {
        Ok(self
            .begin_run_fenced(job_id, count_attempt)?
            .map(|(r, _)| r))
    }

    /// [`JobStore::begin_run`] that also returns the new attempt generation. The
    /// run start advances the generation in the same transaction, so every
    /// earlier attempt's writes are fenced from this point on.
    pub fn begin_run_fenced(
        &self,
        job_id: &str,
        count_attempt: bool,
    ) -> JobResult<Option<(JobRecord, i64)>> {
        let t = Transition {
            to: self.cfg.statuses.run_status_name().map(Into::into),
            count_attempt,
            ..Transition::default()
        };
        let mut conn = self.c();
        let tx = write_txn(&mut conn)?;
        let before = self.load(&tx, job_id)?;
        if before.job.is_terminal() {
            return Ok(None);
        }
        self.refuse_held(&before)?;
        let after = self.exec(&tx, &before, &t)?;
        tx.execute(
            "UPDATE jobs SET attempt_gen=attempt_gen+1 WHERE job_id=?1",
            [job_id],
        )?;
        let generation: i64 = tx.query_row(
            "SELECT attempt_gen FROM jobs WHERE job_id=?1",
            [job_id],
            |r| r.get(0),
        )?;
        tx.commit()?;
        drop(conn);
        // A new attempt starts a fresh progress interval; older pending ticks are stale.
        self.prog
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(job_id);
        Ok(Some((after, generation)))
    }

    /// Give back the attempt a paused-by-handoff run consumed (a handoff is
    /// not the job's fault, so it must not count toward `max_attempts`).
    pub fn refund_attempt(&self, job_id: &str) -> JobResult<()> {
        self.refund_attempt_in(job_id, None)
    }

    /// [`JobStore::refund_attempt`] for the attempt holding `generation` (fenced).
    pub fn refund_attempt_fenced(&self, job_id: &str, generation: i64) -> JobResult<()> {
        self.refund_attempt_in(job_id, Some(generation))
    }

    fn refund_attempt_in(&self, job_id: &str, fence: Option<i64>) -> JobResult<()> {
        let mut conn = self.c();
        let tx = write_txn(&mut conn)?;
        check_fence(&tx, job_id, fence)?;
        tx.execute(
            "UPDATE jobs SET attempts=MAX(attempts-1,0), updated_at=?2 WHERE job_id=?1",
            params![job_id, now()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Record a non-fatal error without changing status (paused, recoverable).
    pub fn record_error(&self, job_id: &str, error: &JobError) -> JobResult<()> {
        self.record_error_in(job_id, None, error)
    }

    /// [`JobStore::record_error`] for the attempt holding `generation` (fenced).
    pub fn record_error_fenced(
        &self,
        job_id: &str,
        generation: i64,
        error: &JobError,
    ) -> JobResult<()> {
        self.record_error_in(job_id, Some(generation), error)
    }

    fn record_error_in(&self, job_id: &str, fence: Option<i64>, error: &JobError) -> JobResult<()> {
        let mut conn = self.c();
        let tx = write_txn(&mut conn)?;
        check_fence(&tx, job_id, fence)?;
        self.flush_pending_in(&tx, job_id)?;
        tx.execute(
            "UPDATE jobs SET error_json=?2, updated_at=?3 WHERE job_id=?1",
            params![
                job_id,
                error
                    .to_json_with(self.cfg.includes_retryable())
                    .to_string(),
                now()
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn fail(&self, job_id: &str, error: &JobError) -> JobResult<bool> {
        self.fail_in(job_id, None, error)
    }

    /// [`JobStore::fail`] for the attempt holding `generation` (fenced).
    pub fn fail_fenced(&self, job_id: &str, generation: i64, error: &JobError) -> JobResult<bool> {
        self.fail_in(job_id, Some(generation), error)
    }

    fn fail_in(&self, job_id: &str, fence: Option<i64>, error: &JobError) -> JobResult<bool> {
        Ok(self
            .transition_in(
                job_id,
                fence,
                &Transition::to(FAILED)
                    .error(Some(error.to_json_with(self.cfg.includes_retryable()))),
            )?
            .is_some())
    }

    /// Complete with a result. False if the job went terminal (cancelled) first.
    pub fn complete(&self, job_id: &str, result: &Value) -> JobResult<bool> {
        self.complete_in(job_id, None, result)
    }

    /// [`JobStore::complete`] for the attempt holding `generation` (fenced).
    pub fn complete_fenced(
        &self,
        job_id: &str,
        generation: i64,
        result: &Value,
    ) -> JobResult<bool> {
        self.complete_in(job_id, Some(generation), result)
    }

    fn complete_in(&self, job_id: &str, fence: Option<i64>, result: &Value) -> JobResult<bool> {
        Ok(self
            .transition_in(
                job_id,
                fence,
                &Transition::to(COMPLETE).result(result.clone()).error(None),
            )?
            .is_some())
    }

    /// Cancel a job. A terminal job and a held job (see [`StatusModel::hold`]) are returned
    /// unchanged: a held job waits for [`JobStore::resolve`].
    pub fn cancel(&self, job_id: &str) -> JobResult<Job> {
        let mut conn = self.c();
        let tx = write_txn(&mut conn)?;
        let before = self.load(&tx, job_id)?;
        if before.job.is_terminal() || self.cfg.statuses.is_held(&before.job.status) {
            return Ok(before.job);
        }
        self.flush_pending_in(&tx, job_id)?;
        let after = self.exec(&tx, &before, &Transition::to(CANCELLED))?;
        tx.commit()?;
        drop(conn);
        self.settle(job_id, true);
        Ok(after.job)
    }

    pub fn is_cancelled(&self, job_id: &str) -> JobResult<bool> {
        Ok(self.get(job_id)?.status == CANCELLED)
    }

    // ------------------------------------------------ external submissions ----

    fn external_col(&self) -> JobResult<&str> {
        self.cfg.external_id.as_deref().ok_or_else(|| {
            JobError::new(
                "not_configured",
                "store has no external id column (StoreConfig::external_id)",
            )
        })
    }

    /// Decide, in one write transaction, how to drive the job's external
    /// submission so a crash or restart never double-submits:
    ///
    /// - stored external id: [`Submission::Resume`] (poll; never resubmit);
    /// - the current attempt was counted but never accepted or failed (the
    ///   process died mid-submit): [`Submission::Submit`] with `replay: true`
    ///   and the SAME idempotency key, no new attempt, no hook;
    /// - attempts used up: [`Submission::Exhausted`];
    /// - otherwise count a new attempt (`StoreHook::on_attempt`, e.g. a spend
    ///   reservation, in the same transaction) and record it `pending`.
    ///
    /// `Ok(None)` when the job is terminal.
    pub fn begin_submission(
        &self,
        job_id: &str,
        max_attempts: i64,
    ) -> JobResult<Option<Submission>> {
        self.begin_submission_in(job_id, None, max_attempts)
    }

    /// [`JobStore::begin_submission`] for the attempt holding `generation` (fenced).
    pub fn begin_submission_fenced(
        &self,
        job_id: &str,
        generation: i64,
        max_attempts: i64,
    ) -> JobResult<Option<Submission>> {
        self.begin_submission_in(job_id, Some(generation), max_attempts)
    }

    fn begin_submission_in(
        &self,
        job_id: &str,
        fence: Option<i64>,
        max_attempts: i64,
    ) -> JobResult<Option<Submission>> {
        let col = self.external_col()?.to_string();
        let mut conn = self.c();
        let tx = write_txn(&mut conn)?;
        check_fence(&tx, job_id, fence)?;
        let before = self.load(&tx, job_id)?;
        if before.job.is_terminal() {
            return Ok(None);
        }
        // Counting an attempt on a held job would reserve spend for a submission nobody may make.
        self.refuse_held(&before)?;
        if let Some(x) = before.ext_str(&col) {
            return Ok(Some(Submission::Resume {
                external_id: x.to_string(),
                attempt: before.job.attempts,
            }));
        }
        let pending: Option<String> = tx
            .query_row(
                "SELECT idempotency_key FROM rk_job_submissions WHERE job_id=?1 AND attempt=?2 AND state='pending'",
                params![job_id, before.job.attempts],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(key) = pending {
            return Ok(Some(Submission::Submit {
                attempt: before.job.attempts,
                idempotency_key: key,
                replay: true,
            }));
        }
        if before.job.attempts >= max_attempts.max(1) {
            return Ok(Some(Submission::Exhausted {
                attempts: before.job.attempts,
            }));
        }
        let after = self.exec(&tx, &before, &Transition::keep().count_attempt())?;
        let attempt = after.job.attempts;
        let key = format!("{job_id}:{attempt}");
        let t = now();
        tx.execute(
            "INSERT OR REPLACE INTO rk_job_submissions (job_id, attempt, idempotency_key, state, created_at, updated_at) VALUES (?1,?2,?3,'pending',?4,?4)",
            params![job_id, attempt, key, t],
        )?;
        tx.commit()?;
        Ok(Some(Submission::Submit {
            attempt,
            idempotency_key: key,
            replay: false,
        }))
    }

    /// The external system accepted the current attempt: store its id (and
    /// optionally move, e.g. `submitted -> running`) in one transaction.
    pub fn accept_submission(
        &self,
        job_id: &str,
        external_id: &str,
        to: Option<&str>,
    ) -> JobResult<Option<JobRecord>> {
        self.accept_submission_in(job_id, None, external_id, to)
    }

    /// [`JobStore::accept_submission`] for the attempt holding `generation` (fenced).
    pub fn accept_submission_fenced(
        &self,
        job_id: &str,
        generation: i64,
        external_id: &str,
        to: Option<&str>,
    ) -> JobResult<Option<JobRecord>> {
        self.accept_submission_in(job_id, Some(generation), external_id, to)
    }

    fn accept_submission_in(
        &self,
        job_id: &str,
        fence: Option<i64>,
        external_id: &str,
        to: Option<&str>,
    ) -> JobResult<Option<JobRecord>> {
        let col = self.external_col()?.to_string();
        let mut conn = self.c();
        let tx = write_txn(&mut conn)?;
        check_fence(&tx, job_id, fence)?;
        let before = self.load(&tx, job_id)?;
        if before.job.is_terminal() {
            return Ok(None);
        }
        let t = Transition {
            to: to.map(Into::into),
            ..Transition::default()
        }
        .set(col, external_id);
        let after = self.exec(&tx, &before, &t)?;
        tx.execute(
            "UPDATE rk_job_submissions SET state='accepted', external_id=?3, updated_at=?4 WHERE job_id=?1 AND attempt=?2",
            params![job_id, after.job.attempts, external_id, now()],
        )?;
        tx.commit()?;
        Ok(Some(after))
    }

    /// The current attempt is confirmed failed (or was rejected): forget the
    /// external id so the next [`JobStore::begin_submission`] counts a new
    /// attempt, record the error, and optionally move (e.g. `running -> submitted`).
    pub fn fail_submission(
        &self,
        job_id: &str,
        error: &JobError,
        to: Option<&str>,
    ) -> JobResult<Option<JobRecord>> {
        self.fail_submission_in(job_id, None, error, to)
    }

    /// [`JobStore::fail_submission`] for the attempt holding `generation` (fenced).
    pub fn fail_submission_fenced(
        &self,
        job_id: &str,
        generation: i64,
        error: &JobError,
        to: Option<&str>,
    ) -> JobResult<Option<JobRecord>> {
        self.fail_submission_in(job_id, Some(generation), error, to)
    }

    fn fail_submission_in(
        &self,
        job_id: &str,
        fence: Option<i64>,
        error: &JobError,
        to: Option<&str>,
    ) -> JobResult<Option<JobRecord>> {
        let col = self.external_col()?.to_string();
        let mut conn = self.c();
        let tx = write_txn(&mut conn)?;
        check_fence(&tx, job_id, fence)?;
        let before = self.load(&tx, job_id)?;
        if before.job.is_terminal() {
            return Ok(None);
        }
        let t = Transition {
            to: to.map(Into::into),
            ..Transition::default()
        }
        .set(col, Value::Null)
        .error(Some(error.to_json_with(self.cfg.includes_retryable())));
        let after = self.exec(&tx, &before, &t)?;
        tx.execute(
            "UPDATE rk_job_submissions SET state='failed', error_json=?3, updated_at=?4 WHERE job_id=?1 AND attempt=?2",
            params![job_id, before.job.attempts, error.to_json_with(self.cfg.includes_retryable()).to_string(), now()],
        )?;
        tx.commit()?;
        Ok(Some(after))
    }

    fn refuse_held(&self, rec: &JobRecord) -> JobResult<()> {
        if self.cfg.statuses.is_held(&rec.job.status) {
            return Err(JobError::new(
                "illegal_transition",
                format!(
                    "job {} is held in {}; only resolve can move it",
                    rec.job.job_id, rec.job.status
                ),
            ));
        }
        Ok(())
    }

    /// The outcome of the current submission attempt is unknown (the request may or may not
    /// have reached the outside system): record `error` and park the job in the held status
    /// `to` (declared with [`StatusModel::hold`]), in one transaction with
    /// `StoreHook::on_transition` and a `held` event. The submission row stays `pending` (with
    /// the error stored on it), so the attempt and anything the hook reserved for it stay
    /// counted. The status model's `allow` table is not consulted: any non-held, non-terminal
    /// job may be held. From here only [`JobStore::resolve`] moves the job. `Ok(None)` when the
    /// job is terminal; `illegal_transition` when it is already held.
    pub fn hold_submission(
        &self,
        job_id: &str,
        error: &JobError,
        to: &str,
    ) -> JobResult<Option<JobRecord>> {
        self.hold_submission_in(job_id, None, error, to)
    }

    /// [`JobStore::hold_submission`] for the attempt holding `generation` (fenced).
    pub fn hold_submission_fenced(
        &self,
        job_id: &str,
        generation: i64,
        error: &JobError,
        to: &str,
    ) -> JobResult<Option<JobRecord>> {
        self.hold_submission_in(job_id, Some(generation), error, to)
    }

    fn hold_submission_in(
        &self,
        job_id: &str,
        fence: Option<i64>,
        error: &JobError,
        to: &str,
    ) -> JobResult<Option<JobRecord>> {
        self.external_col()?;
        if !self.cfg.statuses.is_held(to) {
            return Err(JobError::bad_input(format!("{to} is not a held status")));
        }
        let mut conn = self.c();
        let tx = write_txn(&mut conn)?;
        check_fence(&tx, job_id, fence)?;
        let before = self.load(&tx, job_id)?;
        if before.job.is_terminal() {
            return Ok(None);
        }
        self.refuse_held(&before)?;
        self.flush_pending_in(&tx, job_id)?;
        let ej = error.to_json_with(self.cfg.includes_retryable());
        let t = Transition::to(to).error(Some(ej.clone()));
        let after = self.exec_in(&tx, &before, &t, true)?;
        tx.execute(
            "UPDATE rk_job_submissions SET error_json=?3, updated_at=?4 WHERE job_id=?1 AND attempt=?2",
            params![job_id, before.job.attempts, ej.to_string(), now()],
        )?;
        add_event_on(&tx, job_id, None, "held", None, None, Some(&error.message))?;
        tx.commit()?;
        Ok(Some(after))
    }

    /// Settle a held job, in one transaction with `StoreHook::on_transition` and
    /// `StoreHook::on_resolve` (so an app ledger keeps or releases its reservation atomically)
    /// and a `resolved` event. `illegal_transition` unless the job is in a held status.
    ///
    /// - [`Resolution::Resubmit`]: the current submission row becomes `failed`, the external id
    ///   is cleared and the job moves to `to`, which is required (`bad_input` without it) and
    ///   must be a known, non-terminal, non-held status; the next
    ///   [`JobStore::begin_submission`] counts one new attempt.
    /// - [`Resolution::Abandon`]: the job ends `failed` with that error; the submission row
    ///   becomes `failed`.
    /// - [`Resolution::Adopt`]: the job ends `complete` with that result and no error; the
    ///   submission row becomes `accepted`.
    ///
    /// `to` is ignored by the last two. A hook error rolls everything back and the job stays held.
    pub fn resolve(&self, job_id: &str, r: &Resolution, to: Option<&str>) -> JobResult<JobRecord> {
        let model = &self.cfg.statuses;
        if matches!(r, Resolution::Resubmit) {
            match to {
                Some(t)
                    if model.is_known(t) && !StatusModel::is_terminal(t) && !model.is_held(t) => {}
                _ => {
                    return Err(JobError::bad_input(
                        "resubmit needs a known, non-terminal, non-held target status",
                    ))
                }
            }
        }
        let mut conn = self.c();
        let tx = write_txn(&mut conn)?;
        let before = self.load(&tx, job_id)?;
        if !model.is_held(&before.job.status) {
            return Err(JobError::new(
                "illegal_transition",
                format!(
                    "job {job_id} is {}, not held; only a held job can be resolved",
                    before.job.status
                ),
            ));
        }
        self.flush_pending_in(&tx, job_id)?;
        let includes = self.cfg.includes_retryable();
        let (t, sub_state, label) = match r {
            Resolution::Resubmit => {
                let mut t = Transition::to(to.unwrap_or_default());
                if let Some(col) = self.cfg.external_id.as_deref() {
                    t = t.set(col, Value::Null);
                }
                (t, "failed", "resubmit")
            }
            Resolution::Abandon(e) => (
                Transition::to(FAILED).error(Some(e.to_json_with(includes))),
                "failed",
                "abandon",
            ),
            Resolution::Adopt(v) => (
                Transition::to(COMPLETE).result(v.clone()).error(None),
                "accepted",
                "adopt",
            ),
        };
        let after = self.exec_in(&tx, &before, &t, true)?;
        if self.cfg.external_id.is_some() {
            tx.execute(
                "UPDATE rk_job_submissions SET state=?3, updated_at=?4 WHERE job_id=?1 AND attempt=?2",
                params![job_id, before.job.attempts, sub_state, now()],
            )?;
        }
        add_event_on(&tx, job_id, None, "resolved", None, None, Some(label))?;
        if let Some(h) = &self.cfg.hook {
            h.on_resolve(&tx, &after, after.job.attempts, r)?;
        }
        tx.commit()?;
        drop(conn);
        if after.job.is_terminal() {
            self.settle(job_id, false);
        }
        Ok(after)
    }

    /// Submission attempts recorded for a job, oldest first.
    pub fn submissions(&self, job_id: &str) -> JobResult<Vec<SubmissionRecord>> {
        self.external_col()?;
        let conn = self.c();
        let mut st = conn.prepare(
            "SELECT job_id, attempt, idempotency_key, state, external_id FROM rk_job_submissions WHERE job_id=?1 ORDER BY attempt",
        )?;
        let v = st
            .query_map([job_id], |r| {
                Ok(SubmissionRecord {
                    job_id: r.get(0)?,
                    attempt: r.get(1)?,
                    idempotency_key: r.get(2)?,
                    state: r.get(3)?,
                    external_id: r.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    /// A consumer confirms it has taken a completed job's result.
    pub fn ack(&self, job_id: &str, consumer: &str) -> JobResult<Job> {
        let j = self.get(job_id)?;
        if j.status != COMPLETE {
            return Err(JobError::new(
                "not_complete",
                format!(
                    "job {job_id} is {}, only complete jobs can be acknowledged",
                    j.status
                ),
            ));
        }
        if j.acked_by.is_none() {
            self.c().execute(
                "UPDATE jobs SET acked_by=?2, acked_at=?3, updated_at=?3 WHERE job_id=?1",
                params![job_id, consumer, now()],
            )?;
        }
        self.get(job_id)
    }

    /// Append a progress event (oldest beyond [`MAX_EVENTS`] are dropped).
    /// Status and attempt are read from the job row.
    pub fn add_event(
        &self,
        job_id: &str,
        stage: &str,
        progress: Option<f64>,
        message: Option<&str>,
    ) -> JobResult<Event> {
        self.add_event_with(job_id, None, stage, None, progress, message)
    }

    /// [`JobStore::add_event`] for the attempt holding `generation` (fenced):
    /// a stale attempt's progress never reaches the event stream.
    pub fn add_event_fenced(
        &self,
        job_id: &str,
        generation: i64,
        stage: &str,
        progress: Option<f64>,
        message: Option<&str>,
    ) -> JobResult<Event> {
        let mut conn = self.c();
        let tx = write_txn(&mut conn)?;
        check_fence(&tx, job_id, Some(generation))?;
        self.flush_pending_in(&tx, job_id)?;
        let ev = add_event_on(&tx, job_id, None, stage, None, progress, message)?;
        tx.commit()?;
        Ok(ev)
    }

    /// A handler progress tick, written under the store's [`crate::ProgressPolicy`]:
    ///
    /// - default policy: identical to [`JobStore::add_event_fenced`], always `Ok(Some(_))`;
    /// - `relaxed_durability`: the event commits with `synchronous=NORMAL` (WAL only); the
    ///   connection is back to its configured durability before this returns;
    /// - `coalesce(interval)`: a tick that carries a `progress` fraction is written at most once
    ///   per interval per job; a tick inside the interval replaces the pending one and returns
    ///   `Ok(None)`. The latest pending tick is written by the next tick after the interval, by
    ///   any state change or lifecycle event of the job (in that commit), by
    ///   [`JobStore::events_after`] once it is due, or by [`JobStore::flush_progress`]. A crash
    ///   loses at most the pending tick. Ticks without a fraction are never coalesced. A pending
    ///   tick of a superseded attempt is dropped, never written.
    pub fn progress_fenced(
        &self,
        job_id: &str,
        generation: i64,
        stage: &str,
        progress: Option<f64>,
        message: Option<&str>,
    ) -> JobResult<Option<Event>> {
        let pol = self.cfg.progress;
        let coalescing = pol.coalesce.zip(progress).map(|(i, _)| i);
        if let Some(interval) = coalescing {
            let mut map = self.prog.lock().unwrap_or_else(|p| p.into_inner());
            let st = map.entry(job_id.to_string()).or_default();
            if st.last_write.is_some_and(|t| t.elapsed() < interval) {
                st.pending = Some(Pending {
                    generation,
                    stage: stage.into(),
                    progress,
                    message: message.map(Into::into),
                });
                drop(map);
                self.st_coalesced.fetch_add(1, Ordering::Relaxed);
                return Ok(None);
            }
            st.pending = None;
        }
        let relax = pol.relaxed_durability && self.wal && self.base_sync == DurabilityProfile::Full;
        let mut conn = self.c();
        if relax {
            conn.pragma_update(None, "synchronous", "NORMAL")?;
        }
        let out = (|| -> JobResult<Event> {
            let tx = write_txn(&mut conn)?;
            check_fence(&tx, job_id, Some(generation))?;
            self.flush_pending_in(&tx, job_id)?;
            let ev = add_event_on(&tx, job_id, None, stage, None, progress, message)?;
            tx.commit()?;
            Ok(ev)
        })();
        if relax {
            let restored = conn.pragma_update(None, "synchronous", "FULL");
            if out.is_ok() {
                restored?;
            }
        }
        drop(conn);
        let ev = out?;
        self.st_written.fetch_add(1, Ordering::Relaxed);
        if relax {
            self.st_relaxed.fetch_add(1, Ordering::Relaxed);
        }
        if coalescing.is_some() {
            self.prog
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .entry(job_id.to_string())
                .or_default()
                .last_write = Some(Instant::now());
        }
        Ok(Some(ev))
    }

    /// Write the pending coalesced progress tick of `job_id`, if any (and still current).
    pub fn flush_progress(&self, job_id: &str) -> JobResult<()> {
        if self.cfg.progress.coalesce.is_none() {
            return Ok(());
        }
        let mut conn = self.c();
        let tx = write_txn(&mut conn)?;
        self.flush_pending_in(&tx, job_id)?;
        tx.commit()?;
        Ok(())
    }

    /// Flush the pending tick of `job_id` once its interval has elapsed.
    fn flush_if_due(&self, job_id: &str) {
        let Some(interval) = self.cfg.progress.coalesce else {
            return;
        };
        let due = self
            .prog
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(job_id)
            .is_some_and(|s| {
                s.pending.is_some() && s.last_write.is_none_or(|t| t.elapsed() >= interval)
            });
        if due {
            let _ = self.flush_progress(job_id);
        }
    }

    /// Write `job_id`'s pending coalesced tick on `conn` (inside the caller's transaction),
    /// unless its attempt has been superseded.
    fn flush_pending_in(&self, conn: &Connection, job_id: &str) -> JobResult<()> {
        if self.cfg.progress.coalesce.is_none() {
            return Ok(());
        }
        let pending = self
            .prog
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_mut(job_id)
            .and_then(|s| s.pending.take());
        let Some(p) = pending else {
            return Ok(());
        };
        let current: Option<i64> = conn
            .query_row(
                "SELECT attempt_gen FROM jobs WHERE job_id=?1",
                [job_id],
                |r| r.get(0),
            )
            .optional()?;
        if current != Some(p.generation) {
            return Ok(());
        }
        add_event_on(
            conn,
            job_id,
            None,
            &p.stage,
            None,
            p.progress,
            p.message.as_deref(),
        )?;
        self.st_written.fetch_add(1, Ordering::Relaxed);
        if let Some(s) = self
            .prog
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_mut(job_id)
        {
            s.last_write = Some(Instant::now());
        }
        Ok(())
    }

    /// Counters of the progress path since this store opened.
    pub fn progress_stats(&self) -> ProgressStats {
        ProgressStats {
            written: self.st_written.load(Ordering::Relaxed),
            coalesced: self.st_coalesced.load(Ordering::Relaxed),
            relaxed_writes: self.st_relaxed.load(Ordering::Relaxed),
        }
    }

    /// Like [`JobStore::add_event`], recording an explicit `status` and/or
    /// `attempt` on the event instead of the job row's (`None` keeps the row
    /// value). An explicit status must be in the configured vocabulary
    /// (`bad_input` otherwise).
    pub fn add_event_with(
        &self,
        job_id: &str,
        status: Option<&str>,
        stage: &str,
        attempt: Option<i64>,
        progress: Option<f64>,
        message: Option<&str>,
    ) -> JobResult<Event> {
        if let Some(s) = status {
            if !self.cfg.statuses.is_known(s) {
                return Err(JobError::bad_input(format!("unknown event status: {s}")));
            }
        }
        let conn = self.c();
        self.flush_pending_in(&conn, job_id)?;
        add_event_on(&conn, job_id, status, stage, attempt, progress, message)
    }

    /// Events with `seq > after`, in order. Poll with the last seen `seq` to stream.
    pub fn events_after(&self, job_id: &str, after: i64) -> JobResult<Vec<Event>> {
        self.flush_if_due(job_id);
        let conn = self.c();
        let mut st = conn.prepare(
            "SELECT seq, at, status, stage, attempt, progress, message FROM job_events WHERE job_id=?1 AND seq>?2 ORDER BY seq",
        )?;
        let ev = st
            .query_map(params![job_id, after], |r| {
                Ok(Event {
                    seq: r.get(0)?,
                    job_id: job_id.to_string(),
                    at: r.get(1)?,
                    status: r.get(2)?,
                    stage: r.get(3)?,
                    attempt: r.get(4)?,
                    progress: r.get(5)?,
                    message: r.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ev)
    }

    // ---------------------------------------------------------- artifacts ----

    fn art_sql(&self, tail: &str) -> String {
        let a = &self.cfg.artifacts;
        format!(
            "SELECT {}, job_id, path, sha256, bytes, mime FROM {} {tail}",
            a.id_column, a.table
        )
    }

    /// Artifact wire record using the configured id field (default `artifact_id`).
    pub fn artifact_value(&self, a: &Artifact) -> Value {
        let mut v = a.to_value();
        let wire = &self.cfg.artifacts.wire_id;
        if wire != "artifact_id" {
            if let Some(o) = v.as_object_mut() {
                if let Some(id) = o.remove("artifact_id") {
                    o.insert(wire.clone(), id);
                }
            }
        }
        v
    }

    /// Hash `path` (streaming sha256) and index it as a result artifact of `job_id`.
    pub fn add_artifact(&self, job_id: &str, path: &Path, mime: &str) -> JobResult<Artifact> {
        self.add_artifact_with_id(job_id, &uuid::Uuid::new_v4().to_string(), path, mime)
    }

    /// [`JobStore::add_artifact`] for the attempt holding `generation` (fenced).
    /// The generation is checked again when the row is written.
    pub fn add_artifact_fenced(
        &self,
        job_id: &str,
        generation: i64,
        path: &Path,
        mime: &str,
    ) -> JobResult<Artifact> {
        let (sha256, bytes) = hash_file(path)?;
        let a = Artifact {
            artifact_id: uuid::Uuid::new_v4().to_string(),
            job_id: job_id.into(),
            path: path.to_string_lossy().into_owned(),
            sha256,
            bytes: bytes as i64,
            mime: mime.into(),
        };
        let mut conn = self.c();
        let tx = write_txn(&mut conn)?;
        check_fence(&tx, job_id, Some(generation))?;
        self.insert_artifact(&tx, &a)?;
        tx.commit()?;
        Ok(a)
    }

    /// Like [`JobStore::add_artifact`] with an app-chosen (e.g. content-derived)
    /// id. Idempotent: the same id replaces its row.
    pub fn add_artifact_with_id(
        &self,
        job_id: &str,
        artifact_id: &str,
        path: &Path,
        mime: &str,
    ) -> JobResult<Artifact> {
        let (sha256, bytes) = hash_file(path)?;
        let a = Artifact {
            artifact_id: artifact_id.into(),
            job_id: job_id.into(),
            path: path.to_string_lossy().into_owned(),
            sha256,
            bytes: bytes as i64,
            mime: mime.into(),
        };
        self.record_artifact(&a)?;
        Ok(a)
    }

    /// Index an already-hashed artifact (idempotent by id).
    pub fn record_artifact(&self, a: &Artifact) -> JobResult<()> {
        self.get(&a.job_id)?;
        self.insert_artifact(&self.c(), a)
    }

    fn insert_artifact(&self, conn: &Connection, a: &Artifact) -> JobResult<()> {
        let n = &self.cfg.artifacts;
        conn.execute(
            &format!(
                "INSERT OR REPLACE INTO {} ({}, job_id, path, sha256, bytes, mime, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                n.table, n.id_column
            ),
            params![a.artifact_id, a.job_id, a.path, a.sha256, a.bytes, a.mime, now()],
        )?;
        Ok(())
    }

    pub fn artifacts(&self, job_id: &str) -> JobResult<Vec<Artifact>> {
        let conn = self.c();
        let mut st = conn.prepare(&self.art_sql("WHERE job_id=?1 ORDER BY created_at, rowid"))?;
        let a = st
            .query_map([job_id], artifact_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(a)
    }

    pub fn get_artifact(&self, artifact_id: &str) -> JobResult<Artifact> {
        let id_col = self.cfg.artifacts.id_column.clone();
        self.c()
            .query_row(
                &self.art_sql(&format!("WHERE {id_col}=?1")),
                [artifact_id],
                artifact_row,
            )
            .optional()?
            .ok_or_else(|| {
                JobError::new(
                    "artifact_not_found",
                    format!("no such artifact: {artifact_id}"),
                )
            })
    }

    /// True when the file on disk still has the recorded size and sha256.
    pub fn verify_artifact(&self, a: &Artifact) -> JobResult<bool> {
        match hash_file(Path::new(&a.path)) {
            Ok((sha, bytes)) => Ok(sha == a.sha256 && bytes as i64 == a.bytes),
            Err(e) if e.code == "io" => Ok(false),
            Err(e) => Err(e),
        }
    }
}

/// Reject a write from a superseded attempt. `None` is an unfenced store call.
fn check_fence(conn: &Connection, job_id: &str, fence: Option<i64>) -> JobResult<()> {
    let Some(expected) = fence else {
        return Ok(());
    };
    let current: i64 = conn
        .query_row(
            "SELECT attempt_gen FROM jobs WHERE job_id=?1",
            [job_id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| JobError::new("job_not_found", format!("no such job: {job_id}")))?;
    if current == expected {
        Ok(())
    } else {
        Err(JobError::stale_attempt(job_id))
    }
}

/// Append one event on `conn` (a connection or a transaction).
fn add_event_on(
    conn: &Connection,
    job_id: &str,
    status: Option<&str>,
    stage: &str,
    attempt: Option<i64>,
    progress: Option<f64>,
    message: Option<&str>,
) -> JobResult<Event> {
    let (row_status, row_attempt): (String, i64) = conn
        .query_row(
            "SELECT status, attempts FROM jobs WHERE job_id=?1",
            [job_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| JobError::new("job_not_found", format!("no such job: {job_id}")))?;
    let status = status.map(String::from).unwrap_or(row_status);
    let attempt = attempt.unwrap_or(row_attempt);
    let at = now();
    conn.execute(
        "INSERT INTO job_events (job_id, at, status, stage, attempt, progress, message) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![job_id, at, status, stage, attempt, progress, message],
    )?;
    let seq = conn.last_insert_rowid();
    conn.execute(
        "DELETE FROM job_events WHERE job_id=?1 AND seq <= COALESCE((SELECT seq FROM job_events WHERE job_id=?1 ORDER BY seq DESC LIMIT 1 OFFSET ?2), 0)",
        params![job_id, MAX_EVENTS],
    )?;
    Ok(Event {
        seq,
        job_id: job_id.into(),
        at,
        status,
        stage: stage.into(),
        attempt,
        progress,
        message: message.map(Into::into),
    })
}

/// Create or additively adopt the schema. Never drops or rewrites data.
/// SQLite errors are the outer result (so busy is retried); schema refusals the inner.
fn init_schema(conn: &Connection, cfg: &StoreConfig) -> rusqlite::Result<JobResult<Adoption>> {
    let existed: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='jobs'",
            [],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    let mut adoption = Adoption {
        existing_jobs_table: existed,
        added_columns: vec![],
    };
    // Refuse before touching anything: a required column the app did not
    // declare would make every insert fail.
    let mut st =
        conn.prepare("SELECT name, \"notnull\", dflt_value FROM pragma_table_info('jobs')")?;
    let cols = st
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(st);
    let undeclared: Vec<String> = cols
        .into_iter()
        .filter(|(name, notnull, dflt)| {
            *notnull == 1
                && dflt.is_none()
                && !crate::config::CORE_JOB_COLUMNS.contains(&name.as_str())
                && cfg.column_named(name).is_none()
        })
        .map(|(n, _, _)| n)
        .collect();
    if !undeclared.is_empty() {
        return Ok(Err(JobError::new(
            "schema",
            format!(
                "jobs has NOT NULL columns without defaults that are not declared: {} (declare them with StoreConfig::column)",
                undeclared.join(", ")
            ),
        )));
    }
    let mut ddl = String::from(
        "CREATE TABLE IF NOT EXISTS jobs (
  job_id TEXT PRIMARY KEY,
  request_id TEXT UNIQUE,
  kind TEXT NOT NULL,
  request_json TEXT NOT NULL,
  status TEXT NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0,
  error_json TEXT,
  result_json TEXT,
  acked_by TEXT,
  acked_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  attempt_gen INTEGER NOT NULL DEFAULT 0",
    );
    for c in &cfg.columns {
        ddl.push_str(",\n  ");
        ddl.push_str(&c.ddl().unwrap_or_default());
    }
    ddl.push_str("\n);");
    conn.execute_batch(&ddl)?;
    conn.execute_batch(CORE_SCHEMA)?;
    let a = &cfg.artifacts;
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS {t} (
  {id} TEXT PRIMARY KEY,
  job_id TEXT NOT NULL,
  path TEXT NOT NULL,
  sha256 TEXT NOT NULL,
  bytes INTEGER NOT NULL,
  mime TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS {t}_job ON {t}(job_id);",
        t = a.table,
        id = a.id_column
    ))?;
    conn.execute_batch(META_SCHEMA)?;
    conn.execute_batch(delete::DELETING_SCHEMA)?;
    if cfg.external_id.is_some() {
        conn.execute_batch(SUBMISSIONS_SCHEMA)?;
    }
    // Columns later schemas (or the other app's schema) may lack.
    let mut wanted: Vec<(String, String, String)> = vec![
        ("jobs".into(), "acked_by".into(), "TEXT".into()),
        ("jobs".into(), "acked_at".into(), "TEXT".into()),
        ("job_events".into(), "progress".into(), "REAL".into()),
        (
            "jobs".into(),
            "attempt_gen".into(),
            "INTEGER NOT NULL DEFAULT 0".into(),
        ),
    ];
    for c in &cfg.columns {
        if c.required() && !has_col(conn, "jobs", &c.name)? {
            return Ok(Err(JobError::new(
                "schema",
                format!(
                    "existing jobs table lacks NOT NULL column {} and it has no default to add it with",
                    c.name
                ),
            )));
        }
        let d = c.ddl().unwrap_or_default();
        let decl = d[c.name.len()..].trim().to_string();
        wanted.push(("jobs".into(), c.name.clone(), decl));
    }
    for (table, col, decl) in wanted {
        if has_col(conn, &table, &col)? {
            continue;
        }
        match conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {col} {decl};")) {
            Ok(()) => adoption.added_columns.push(format!("{table}.{col}")),
            // Another process added it first.
            Err(e) if is_busy(&e) => return Err(e),
            Err(_) if has_col(conn, &table, &col)? => {}
            Err(e) => return Err(e),
        }
    }
    Ok(Ok(adoption))
}

fn has_col(conn: &Connection, table: &str, col: &str) -> rusqlite::Result<bool> {
    column_exists(conn, table, col).map_err(db_to_sqlite)
}

fn artifact_row(r: &Row) -> rusqlite::Result<Artifact> {
    Ok(Artifact {
        artifact_id: r.get(0)?,
        job_id: r.get(1)?,
        path: r.get(2)?,
        sha256: r.get(3)?,
        bytes: r.get(4)?,
        mime: r.get(5)?,
    })
}

fn hash_file(path: &Path) -> JobResult<(String, u64)> {
    Ok(rightkit_fs::digest::sha256_file_with_len(path)?)
}

fn db_to_sqlite(e: rightkit_db::DbError) -> rusqlite::Error {
    match e {
        rightkit_db::DbError::Sqlite(e) => e,
        other => rusqlite::Error::InvalidParameterName(other.to_string()),
    }
}

fn is_busy(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(f, _)
        if matches!(f.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
}

fn backoff<T, E>(mut f: impl FnMut() -> Result<T, E>, busy: impl Fn(&E) -> bool) -> Result<T, E> {
    let started = std::time::Instant::now();
    let mut pause = std::time::Duration::from_millis(5);
    loop {
        match f() {
            Err(e) if busy(&e) && started.elapsed() < std::time::Duration::from_secs(30) => {
                std::thread::sleep(pause);
                pause = (pause * 2).min(std::time::Duration::from_millis(100));
            }
            other => return other,
        }
    }
}

fn retry_busy<T>(f: impl FnMut() -> rusqlite::Result<T>) -> JobResult<T> {
    backoff(f, is_busy).map_err(Into::into)
}

fn retry_busy_db<T>(f: impl FnMut() -> rightkit_db::Result<T>) -> JobResult<T> {
    backoff(
        f,
        |e| matches!(e, rightkit_db::DbError::Sqlite(s) if is_busy(s)),
    )
    .map_err(Into::into)
}

#[cfg(test)]
mod now_format_tests {
    use super::format_utc;

    #[test]
    fn formats_known_instants() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_utc(1_709_210_096), "2024-02-29T12:34:56Z");
        assert_eq!(format_utc(-1), "1969-12-31T23:59:59Z");
        assert_eq!(format_utc(4_102_444_799), "2099-12-31T23:59:59Z");
    }
}
