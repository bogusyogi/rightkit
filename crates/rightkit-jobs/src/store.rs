use std::io::Read;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rightkit_db::rusqlite::{self, params, params_from_iter, Connection, OptionalExtension, Row};
use rightkit_db::{column_exists, write_txn, DbOptions, DurabilityProfile};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use time::macros::format_description;
use time::OffsetDateTime;

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

pub fn now() -> String {
    OffsetDateTime::now_utc()
        .format(format_description!(
            "[year]-[month]-[day]T[hour]:[minute]:[second]Z"
        ))
        .unwrap_or_default()
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
        let mut conn = retry_busy_db(|| options.open_writer())?;
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
        let job = row(r)?;
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
                return Ok((j, true));
            }
        }
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
        let rec = self.load(&tx, &id)?;
        if let Some(h) = &self.cfg.hook {
            h.on_create(&tx, &rec)?;
        }
        tx.commit()?;
        Ok((rec, false))
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
        let model = &self.cfg.statuses;
        if let Some(to) = &t.to {
            if !model.can_move(&before.job.status, to) {
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
        let mut conn = self.c();
        let tx = write_txn(&mut conn)?;
        let before = self.load(&tx, job_id)?;
        if before.job.is_terminal() {
            return Ok(None);
        }
        let after = self.exec(&tx, &before, t)?;
        tx.commit()?;
        Ok(Some(after))
    }

    /// Set extension columns without changing status.
    pub fn set_ext(&self, job_id: &str, ext: &Ext) -> JobResult<JobRecord> {
        let t = Transition {
            ext: ext.clone(),
            ..Transition::default()
        };
        match self.transition(job_id, &t)? {
            Some(r) => Ok(r),
            // Terminal jobs still accept extension data (e.g. a final cost).
            None => self.write(|conn| {
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
        self.transition(
            job_id,
            &Transition {
                to: self.cfg.statuses.run_status_name().map(Into::into),
                count_attempt,
                ..Transition::default()
            },
        )
    }

    /// Give back the attempt a paused-by-handoff run consumed (a handoff is
    /// not the job's fault, so it must not count toward `max_attempts`).
    pub fn refund_attempt(&self, job_id: &str) -> JobResult<()> {
        self.c().execute(
            "UPDATE jobs SET attempts=MAX(attempts-1,0), updated_at=?2 WHERE job_id=?1",
            params![job_id, now()],
        )?;
        Ok(())
    }

    /// Record a non-fatal error without changing status (paused, recoverable).
    pub fn record_error(&self, job_id: &str, error: &JobError) -> JobResult<()> {
        self.c().execute(
            "UPDATE jobs SET error_json=?2, updated_at=?3 WHERE job_id=?1",
            params![job_id, error.to_json().to_string(), now()],
        )?;
        Ok(())
    }

    pub fn fail(&self, job_id: &str, error: &JobError) -> JobResult<bool> {
        Ok(self
            .transition(job_id, &Transition::to(FAILED).error(Some(error.to_json())))?
            .is_some())
    }

    /// Complete with a result. False if the job went terminal (cancelled) first.
    pub fn complete(&self, job_id: &str, result: &Value) -> JobResult<bool> {
        Ok(self
            .transition(
                job_id,
                &Transition::to(COMPLETE).result(result.clone()).error(None),
            )?
            .is_some())
    }

    pub fn cancel(&self, job_id: &str) -> JobResult<Job> {
        self.transition(job_id, &Transition::to(CANCELLED))?;
        self.get(job_id)
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
        let col = self.external_col()?.to_string();
        let mut conn = self.c();
        let tx = write_txn(&mut conn)?;
        let before = self.load(&tx, job_id)?;
        if before.job.is_terminal() {
            return Ok(None);
        }
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
        let col = self.external_col()?.to_string();
        let mut conn = self.c();
        let tx = write_txn(&mut conn)?;
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
        let col = self.external_col()?.to_string();
        let mut conn = self.c();
        let tx = write_txn(&mut conn)?;
        let before = self.load(&tx, job_id)?;
        if before.job.is_terminal() {
            return Ok(None);
        }
        let t = Transition {
            to: to.map(Into::into),
            ..Transition::default()
        }
        .set(col, Value::Null)
        .error(Some(error.to_json()));
        let after = self.exec(&tx, &before, &t)?;
        tx.execute(
            "UPDATE rk_job_submissions SET state='failed', error_json=?3, updated_at=?4 WHERE job_id=?1 AND attempt=?2",
            params![job_id, before.job.attempts, error.to_json().to_string(), now()],
        )?;
        tx.commit()?;
        Ok(Some(after))
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
    pub fn add_event(
        &self,
        job_id: &str,
        stage: &str,
        progress: Option<f64>,
        message: Option<&str>,
    ) -> JobResult<Event> {
        let conn = self.c();
        let (status, attempt): (String, i64) = conn
            .query_row(
                "SELECT status, attempts FROM jobs WHERE job_id=?1",
                [job_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| JobError::new("job_not_found", format!("no such job: {job_id}")))?;
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

    /// Events with `seq > after`, in order. Poll with the last seen `seq` to stream.
    pub fn events_after(&self, job_id: &str, after: i64) -> JobResult<Vec<Event>> {
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
        let n = &self.cfg.artifacts;
        self.c().execute(
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
  updated_at TEXT NOT NULL",
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
    if cfg.external_id.is_some() {
        conn.execute_batch(SUBMISSIONS_SCHEMA)?;
    }
    // Columns later schemas (or the other app's schema) may lack.
    let mut wanted: Vec<(String, String, String)> = vec![
        ("jobs".into(), "acked_by".into(), "TEXT".into()),
        ("jobs".into(), "acked_at".into(), "TEXT".into()),
        ("job_events".into(), "progress".into(), "REAL".into()),
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
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        total += n as u64;
    }
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    Ok((hex, total))
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
