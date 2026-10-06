use std::io::Read;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rightkit_db::{write_txn, DbOptions, DurabilityProfile};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use time::macros::format_description;
use time::OffsetDateTime;

use crate::error::{JobError, JobResult};

pub const QUEUED: &str = "queued";
pub const RUNNING: &str = "running";
pub const COMPLETE: &str = "complete";
pub const FAILED: &str = "failed";
pub const CANCELLED: &str = "cancelled";
const NON_TERMINAL: &str = "('queued','running')";

/// Progress events kept per job (oldest are dropped).
pub const MAX_EVENTS: i64 = 200;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS jobs (
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
  updated_at TEXT NOT NULL
);
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
CREATE TABLE IF NOT EXISTS artifacts (
  artifact_id TEXT PRIMARY KEY,
  job_id TEXT NOT NULL,
  path TEXT NOT NULL,
  sha256 TEXT NOT NULL,
  bytes INTEGER NOT NULL,
  mime TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS artifacts_job ON artifacts(job_id);
";

pub fn now() -> String {
    OffsetDateTime::now_utc()
        .format(format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]Z"))
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
        matches!(self.status.as_str(), COMPLETE | FAILED | CANCELLED)
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

const COLS: &str = "job_id, request_id, kind, request_json, status, attempts, error_json, result_json, acked_by, acked_at, created_at, updated_at";

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
    pub fn to_value(&self) -> Value {
        json!({"artifact_id": self.artifact_id, "job_id": self.job_id, "path": self.path,
               "sha256": self.sha256, "bytes": self.bytes, "mime": self.mime})
    }
}

/// SQLite job store. One database may be shared by several processes (GUI
/// sidecar plus CLI invocations): WAL plus a busy timeout via `rightkit-db`.
pub struct JobStore {
    conn: Mutex<Connection>,
}

impl JobStore {
    pub fn open(path: impl AsRef<Path>) -> JobResult<Self> {
        if let Some(dir) = path.as_ref().parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut options = DbOptions::new(path.as_ref());
        options.durability = DurabilityProfile::Full;
        Self::open_with(&options)
    }

    pub fn open_with(options: &DbOptions) -> JobResult<Self> {
        let mut conn = options.open_writer()?;
        // Several processes may initialise a new database at once; the
        // statements are idempotent, so retry only on SQLITE_BUSY/LOCKED.
        retry_busy(|| conn.execute_batch(SCHEMA))?;
        retry_busy(|| {
            let tx = write_txn(&mut conn).map_err(|e| match e {
                rightkit_db::DbError::Sqlite(e) => e,
                other => rusqlite::Error::InvalidParameterName(other.to_string()),
            })?;
            tx.pragma_update(None, "user_version", 1)?;
            tx.commit()
        })?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn c(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Insert a `queued` job, or return the existing one for the same
    /// `request_id` (the mapping lives in the same transaction as the row).
    /// Returns `(job, deduplicated)`.
    pub fn submit(&self, n: &NewJob) -> JobResult<(Job, bool)> {
        let mut conn = self.c();
        let tx = write_txn(&mut conn)?;
        if let Some(rid) = &n.request_id {
            let found = tx
                .query_row(&format!("SELECT {COLS} FROM jobs WHERE request_id=?1"), [rid], row)
                .optional()?;
            if let Some(j) = found {
                return Ok((j, true));
            }
        }
        let id = uuid::Uuid::new_v4().to_string();
        let t = now();
        tx.execute(
            "INSERT INTO jobs (job_id, request_id, kind, request_json, status, attempts, created_at, updated_at)
             VALUES (?1,?2,?3,?4,'queued',0,?5,?5)",
            params![id, n.request_id, n.kind, n.request.to_string(), t],
        )?;
        let job = tx.query_row(&format!("SELECT {COLS} FROM jobs WHERE job_id=?1"), [&id], row)?;
        tx.commit()?;
        Ok((job, false))
    }

    pub fn get(&self, job_id: &str) -> JobResult<Job> {
        self.c()
            .query_row(&format!("SELECT {COLS} FROM jobs WHERE job_id=?1"), [job_id], row)
            .optional()?
            .ok_or_else(|| JobError::new("job_not_found", format!("no such job: {job_id}")))
    }

    pub fn get_by_request(&self, request_id: &str) -> JobResult<Job> {
        self.c()
            .query_row(&format!("SELECT {COLS} FROM jobs WHERE request_id=?1"), [request_id], row)
            .optional()?
            .ok_or_else(|| {
                JobError::new("job_not_found", format!("no job for request_id: {request_id}"))
            })
    }

    /// Newest-first; returns up to `limit` (default 50) rows and whether more matched.
    pub fn list(&self, f: &ListFilter) -> JobResult<(Vec<Job>, bool)> {
        let limit = f.limit.unwrap_or(50).clamp(1, 1000);
        let conn = self.c();
        let mut st = conn.prepare(&format!(
            "SELECT {COLS} FROM jobs WHERE (?1 IS NULL OR request_id=?1) AND (?2 IS NULL OR status=?2) ORDER BY created_at DESC, rowid DESC LIMIT ?3"
        ))?;
        let mut jobs = st
            .query_map(params![f.request_id, f.status, limit + 1], row)?
            .collect::<Result<Vec<_>, _>>()?;
        let truncated = jobs.len() as i64 > limit;
        jobs.truncate(limit as usize);
        Ok((jobs, truncated))
    }

    /// Ids of queued/running jobs, oldest first. Passive: changes nothing.
    pub fn non_terminal_ids(&self) -> JobResult<Vec<String>> {
        let conn = self.c();
        let mut st = conn.prepare(&format!(
            "SELECT job_id FROM jobs WHERE status IN {NON_TERMINAL} ORDER BY created_at, rowid"
        ))?;
        let ids = st.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
        Ok(ids)
    }

    /// Mark a non-terminal job running and count the attempt. `Ok(None)` when
    /// the job is already terminal (e.g. cancelled meanwhile).
    pub fn begin_attempt(&self, job_id: &str) -> JobResult<Option<Job>> {
        let n = self.c().execute(
            &format!("UPDATE jobs SET status='running', attempts=attempts+1, updated_at=?2 WHERE job_id=?1 AND status IN {NON_TERMINAL}"),
            params![job_id, now()],
        )?;
        if n == 0 {
            self.get(job_id)?;
            return Ok(None);
        }
        self.get(job_id).map(Some)
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
        let n = self.c().execute(
            &format!("UPDATE jobs SET status='failed', error_json=?2, updated_at=?3 WHERE job_id=?1 AND status IN {NON_TERMINAL}"),
            params![job_id, error.to_json().to_string(), now()],
        )?;
        Ok(n == 1)
    }

    /// Complete with a result. False if the job went terminal (cancelled) first.
    pub fn complete(&self, job_id: &str, result: &Value) -> JobResult<bool> {
        let n = self.c().execute(
            &format!("UPDATE jobs SET status='complete', result_json=?2, error_json=NULL, updated_at=?3 WHERE job_id=?1 AND status IN {NON_TERMINAL}"),
            params![job_id, result.to_string(), now()],
        )?;
        Ok(n == 1)
    }

    pub fn cancel(&self, job_id: &str) -> JobResult<Job> {
        self.get(job_id)?;
        self.c().execute(
            &format!("UPDATE jobs SET status='cancelled', updated_at=?2 WHERE job_id=?1 AND status IN {NON_TERMINAL}"),
            params![job_id, now()],
        )?;
        self.get(job_id)
    }

    pub fn is_cancelled(&self, job_id: &str) -> JobResult<bool> {
        Ok(self.get(job_id)?.status == CANCELLED)
    }

    /// A consumer confirms it has taken a completed job's result.
    pub fn ack(&self, job_id: &str, consumer: &str) -> JobResult<Job> {
        let j = self.get(job_id)?;
        if j.status != COMPLETE {
            return Err(JobError::new(
                "not_complete",
                format!("job {job_id} is {}, only complete jobs can be acknowledged", j.status),
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
            .query_row("SELECT status, attempts FROM jobs WHERE job_id=?1", [job_id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
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

    /// Hash `path` (streaming sha256) and index it as a result artifact of `job_id`.
    pub fn add_artifact(&self, job_id: &str, path: &Path, mime: &str) -> JobResult<Artifact> {
        self.get(job_id)?;
        let (sha256, bytes) = hash_file(path)?;
        let a = Artifact {
            artifact_id: uuid::Uuid::new_v4().to_string(),
            job_id: job_id.into(),
            path: path.to_string_lossy().into_owned(),
            sha256,
            bytes: bytes as i64,
            mime: mime.into(),
        };
        self.c().execute(
            "INSERT INTO artifacts (artifact_id, job_id, path, sha256, bytes, mime, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![a.artifact_id, a.job_id, a.path, a.sha256, a.bytes, a.mime, now()],
        )?;
        Ok(a)
    }

    pub fn artifacts(&self, job_id: &str) -> JobResult<Vec<Artifact>> {
        let conn = self.c();
        let mut st = conn.prepare(
            "SELECT artifact_id, job_id, path, sha256, bytes, mime FROM artifacts WHERE job_id=?1 ORDER BY created_at, rowid",
        )?;
        let a = st.query_map([job_id], artifact_row)?.collect::<Result<Vec<_>, _>>()?;
        Ok(a)
    }

    pub fn get_artifact(&self, artifact_id: &str) -> JobResult<Artifact> {
        self.c()
            .query_row(
                "SELECT artifact_id, job_id, path, sha256, bytes, mime FROM artifacts WHERE artifact_id=?1",
                [artifact_id],
                artifact_row,
            )
            .optional()?
            .ok_or_else(|| JobError::new("artifact_not_found", format!("no such artifact: {artifact_id}")))
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

fn retry_busy<T>(mut f: impl FnMut() -> rusqlite::Result<T>) -> JobResult<T> {
    let started = std::time::Instant::now();
    let mut pause = std::time::Duration::from_millis(5);
    loop {
        match f() {
            Err(rusqlite::Error::SqliteFailure(e, _))
                if matches!(
                    e.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                ) && started.elapsed() < std::time::Duration::from_secs(30) =>
            {
                std::thread::sleep(pause);
                pause = (pause * 2).min(std::time::Duration::from_millis(100));
            }
            other => return other.map_err(Into::into),
        }
    }
}
