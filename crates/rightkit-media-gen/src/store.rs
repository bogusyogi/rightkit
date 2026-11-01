//! SQLite job store (§4.3): rightkit-job-queue' `JobStore` adopted over the host's
//! on-disk schema. One database shared by the GUI sidecar and CLI processes
//! (WAL, busy wait, retry and adoption of existing databases are RightKit's).
//! the host keeps its `submitted`/`running` vocabulary (plus `outcome_unknown`,
//! a paid submit that may have been delivered and is held for the user), the
//! provider columns, the `assets` table and the spend ledger as app extensions.

use rightkit_db::rusqlite::{self, params, OptionalExtension};
use rightkit_job_queue::{
    Artifact, ArtifactNames, Event, Ext, ExtColumn, JobError, JobRecord, JobStore, ListFilter,
    NewJob as RkNewJob, Resolution, StatusModel, StoreConfig,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use time::macros::format_description;
use time::OffsetDateTime;

use crate::{Context, GenError, GenResult};

/// The shared database handle: a cheap, cloneable reference to one [`JobStore`] (one SQLite
/// connection behind a mutex, so it is `Send + Sync`). It derefs to the store, so every call
/// site uses it exactly as it used the store itself. In a resident process (see
/// [`mark_resident`]) every handle for one database is a clone of the same store.
#[derive(Clone)]
pub struct Db {
    store: Arc<JobStore>,
    /// The spend ledger of this database (`rightkit-spend`), shared with the store's hook.
    spend: crate::ledger::SpendHandle,
}

impl Db {
    pub(crate) fn spend(&self) -> &crate::ledger::SpendHandle {
        &self.spend
    }
}

impl std::ops::Deref for Db {
    type Target = JobStore;
    fn deref(&self) -> &JobStore {
        &self.store
    }
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Db(JobStore)")
    }
}

pub use rightkit_job_queue::MAX_EVENTS;

/// Non-terminal statuses of the host's vocabulary.
pub const SUBMITTED: &str = "submitted";
pub const RUNNING: &str = "running";
/// A paid submit that may have reached the provider and cannot be repeated
/// blindly. Not terminal, never runnable: only `job resolve` leaves it. This is
/// RightKit's held status (rightkit-job-queue 0.2.9 `StatusModel::hold`): it is
/// entered with `JobStore::hold_submission` ([`mark_outcome_unknown`]) and left
/// only with `JobStore::resolve` ([`resolve`]); the spend ledger follows through
/// `StoreHook::on_resolve` (`ledger::SpendHook`). The stored string is the one
/// the host always used, so databases written before the move read unchanged.
pub const OUTCOME_UNKNOWN: &str = "outcome_unknown";

/// rightkit-job-queue error to the host's wire error (same codes; `bad_input` is the host's `invalid_input`).
pub fn je(e: JobError) -> GenError {
    let code = if e.code == "bad_input" {
        "invalid_input".to_string()
    } else {
        e.code
    };
    GenError::new(code, e.message)
}

/// the host error to a rightkit-job-queue error (what `error_json` stores).
pub fn rk_err(e: &GenError) -> JobError {
    JobError::new(e.code.clone(), e.message.clone())
}

pub fn db_err(e: rusqlite::Error) -> GenError {
    GenError::new("db", format!("database error: {e}"))
}

pub fn io_err(e: std::io::Error) -> GenError {
    GenError::new("io", format!("io error: {e}"))
}

pub fn now() -> String {
    ts(OffsetDateTime::now_utc())
}

pub fn ts(t: OffsetDateTime) -> String {
    t.format(format_description!(
        "[year]-[month]-[day]T[hour]:[minute]:[second]Z"
    ))
    .unwrap_or_default()
}

pub fn today() -> String {
    OffsetDateTime::now_utc()
        .format(format_description!("[year]-[month]-[day]"))
        .unwrap_or_default()
}

/// the host's app-owned tables (settings and asset facts), created alongside the job tables;
/// existing databases already hold them. The spend ledger is `rightkit-spend`'s (its `spend_*`
/// tables) plus the host's attempt index, both created by `ledger::attach` after the store opens;
/// a database from before that move keeps its old `ledger` table until `ledger::attach` imports
/// it and renames it.
const APP_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS settings (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
-- Asset facts (origin, evidentiary meaning, rights) and scope ids: one additive side table keyed by
-- asset id, so the rightkit-job-queue `assets` table is untouched. An asset with no row here predates
-- the facts and reads as explicit `unknown`.
CREATE TABLE IF NOT EXISTS asset_facts (
  asset_id TEXT PRIMARY KEY,
  job_id TEXT NOT NULL,
  facts_json TEXT NOT NULL,
  scope_json TEXT,
  recorded_at TEXT NOT NULL
);
";

fn config(hook: crate::ledger::SpendHook) -> StoreConfig {
    StoreConfig::new()
        .column(ExtColumn::text("provider").not_null())
        .column(ExtColumn::text("model").not_null())
        .column(ExtColumn::text("provider_job_id"))
        .column(ExtColumn::real("est_usd").not_null().default(0))
        .column(ExtColumn::real("actual_usd"))
        .statuses(
            // The held status has no `allow` entries: it is entered by `hold_submission` and
            // left by `resolve`, nothing else moves a job into or out of it.
            StatusModel::new(SUBMITTED, [SUBMITTED, RUNNING, OUTCOME_UNKNOWN])
                .allow(SUBMITTED, RUNNING)
                .allow(RUNNING, SUBMITTED)
                .hold(OUTCOME_UNKNOWN),
        )
        .artifacts(ArtifactNames::new("assets", "asset_id", "asset_id"))
        .external_id("provider_job_id")
        // the host's wire error is {code, message}.
        .include_retryable(false)
        .app_schema(APP_SCHEMA)
        // Library search index (PENDING 89): additive, idempotent.
        .app_schema(crate::search::SCHEMA)
        // the host never set a schema version; keep it that way.
        .user_version(None)
        .hook(Arc::new(hook))
}

static RESIDENT: AtomicBool = AtomicBool::new(false);

/// Declare this process long-lived (the sidecar, the detached `job run` runner). From now on
/// [`open`] hands out one shared store per database path instead of opening per call, so the
/// settlement journal is replayed once per resident process and database. Call it once, early.
pub fn mark_resident() {
    RESIDENT.store(true, Ordering::SeqCst);
}

pub fn is_resident() -> bool {
    RESIDENT.load(Ordering::SeqCst)
}

/// Identity of a database file (device and inode on unix), so a handle to a file that was
/// deleted and recreated at the same path is not reused.
type FileId = Option<(u64, u64)>;

fn file_id(path: &Path) -> FileId {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

struct Resident {
    db: Db,
    id: FileId,
}

fn residents() -> &'static Mutex<HashMap<PathBuf, Resident>> {
    static MAP: OnceLock<Mutex<HashMap<PathBuf, Resident>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The database path with its folder canonicalised: the key of the resident map.
fn canonical_db_path(path: &Path) -> PathBuf {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return path.to_path_buf();
    };
    let _ = rightkit_fs::ensure_private_dir(dir);
    std::fs::canonicalize(dir)
        .map(|d| d.join(name))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// The settlement journal folder rightkit-job-queue derives for a database (`<db file>.settlements`).
fn journal_dir(db: &Path) -> Option<PathBuf> {
    let name = db.file_name()?.to_str()?;
    Some(db.with_file_name(format!("{name}.settlements")))
}

/// Typed error for a journal folder that exists but cannot be listed. Never retried around and
/// never read as "no jobs": a journal that cannot be read may hold a paid job's settlement.
fn journal_error(dir: &Path, cause: &dyn std::fmt::Display) -> GenError {
    GenError::new(
        "io",
        format!(
            "the job store's journal folder cannot be read: {} ({cause})",
            dir.display()
        ),
    )
}

/// Fail when `<db>.settlements` exists and cannot be listed. A missing folder is normal.
fn check_journal(db: &Path) -> GenResult<()> {
    let Some(dir) = journal_dir(db) else {
        return Ok(());
    };
    match std::fs::metadata(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(journal_error(&dir, &e)),
        Ok(_) => std::fs::read_dir(&dir)
            .map(|_| ())
            .map_err(|e| journal_error(&dir, &e)),
    }
}

fn open_new(ctx: &Context) -> GenResult<Db> {
    let path = ctx.db_path();
    check_journal(&path)?;
    let hook = crate::ledger::SpendHook::new();
    let store = JobStore::open_configured(&path, config(hook.clone())).map_err(|e| {
        // An `io` failure of the open is the journal folder when that folder cannot be listed.
        if e.code == "io" {
            if let Err(journal) = check_journal(&path) {
                return journal;
            }
        }
        je(e)
    })?;
    // The ledger attaches once the database exists; until then the hook refuses to reserve.
    let spend = crate::ledger::attach(&path)?;
    hook.bind(spend.clone());
    Ok(Db {
        store: Arc::new(store),
        spend,
    })
}

/// Open (creating and adopting if needed) the shared database.
///
/// In a non-resident process (the short-lived CLI, unit tests) every call opens its own store.
/// In a resident process one store per canonical database path is opened on first use and
/// shared by every later call; a failed open is not cached, so the next call retries it.
pub fn open(ctx: &Context) -> GenResult<Db> {
    if !is_resident() {
        return open_new(ctx);
    }
    let path = ctx.db_path();
    let key = canonical_db_path(&path);
    let mut map = residents().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(r) = map.get(&key) {
        if path.exists() && file_id(&path) == r.id {
            return Ok(r.db.clone());
        }
        // The database file is gone or was replaced: that handle points at a dead file.
        map.remove(&key);
    }
    // The map lock is held across the open so two threads never open (and replay) twice.
    let db = open_new(ctx)?;
    map.insert(
        key,
        Resident {
            db: db.clone(),
            id: file_id(&path),
        },
    );
    Ok(db)
}

#[derive(Debug, Clone)]
pub struct Job {
    pub job_id: String,
    pub request_id: Option<String>,
    pub kind: String,
    pub provider: String,
    pub model: String,
    pub request_json: String,
    pub status: String,
    pub provider_job_id: Option<String>,
    pub attempts: i64,
    pub est_usd: f64,
    pub actual_usd: Option<f64>,
    pub error_json: Option<String>,
    pub result_json: Option<String>,
    pub acked_by: Option<String>,
    pub acked_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// Held attempt, reserved amount and idempotency key of an
    /// `outcome_unknown` job; filled by [`get_job`] and its siblings.
    pub outcome_unknown: Option<Value>,
}

impl From<JobRecord> for Job {
    fn from(r: JobRecord) -> Self {
        let text = |k: &str| r.ext_str(k).map(str::to_string);
        Job {
            provider: text("provider").unwrap_or_default(),
            model: text("model").unwrap_or_default(),
            provider_job_id: text("provider_job_id"),
            est_usd: r.ext_f64("est_usd").unwrap_or(0.0),
            actual_usd: r.ext_f64("actual_usd"),
            job_id: r.job.job_id,
            request_id: r.job.request_id,
            kind: r.job.kind,
            request_json: r.job.request.to_string(),
            status: r.job.status,
            attempts: r.job.attempts,
            error_json: r.job.error.map(|v| v.to_string()),
            result_json: r.job.result.map(|v| v.to_string()),
            acked_by: r.job.acked_by,
            acked_at: r.job.acked_at,
            created_at: r.job.created_at,
            updated_at: r.job.updated_at,
            outcome_unknown: None,
        }
    }
}

impl Job {
    pub fn is_terminal(&self) -> bool {
        matches!(self.status.as_str(), "complete" | "failed" | "cancelled")
    }

    /// Waiting for the user to `job resolve` it: not terminal, never runnable.
    pub fn awaits_resolution(&self) -> bool {
        self.status == OUTCOME_UNKNOWN
    }

    /// Held with a received paid result that could not be stored (error code
    /// `result_recovered`): `job resolve --adopt` stores it, `--resubmit` is refused.
    pub fn awaits_adoption(&self) -> bool {
        self.awaits_resolution()
            && self
                .error_json
                .as_deref()
                .and_then(|x| serde_json::from_str::<Value>(x).ok())
                .is_some_and(|e| e["code"] == crate::jobs::RESULT_RECOVERED)
    }

    /// Held because the provider finished and billed but returned no usable output (error code
    /// `result_unusable`): the attempt is settled as spent, so `--resubmit` is refused.
    pub fn awaits_abandon_only(&self) -> bool {
        self.awaits_resolution()
            && self
                .error_json
                .as_deref()
                .and_then(|x| serde_json::from_str::<Value>(x).ok())
                .is_some_and(|e| e["code"] == crate::jobs::RESULT_UNUSABLE)
    }

    /// Record for `job.status` / `job.list`. Holds no secrets: requests carry
    /// prompt and params only; keys never enter the database.
    pub fn to_value(&self) -> Value {
        let error = self
            .error_json
            .as_deref()
            .and_then(|x| serde_json::from_str::<Value>(x).ok())
            .unwrap_or(Value::Null);
        let mut v = json!({
            "job_id": self.job_id,
            "request_id": self.request_id,
            "kind": self.kind,
            "provider": self.provider,
            "model": self.model,
            "request": serde_json::from_str::<Value>(&self.request_json).unwrap_or(Value::Null),
            "scope": job_scope(self),
            "route": job_route(self),
            "status": self.status,
            "provider_job_id": self.provider_job_id,
            "attempts": self.attempts,
            "est_usd": self.est_usd,
            "actual_usd": self.actual_usd,
            "error": error,
            "acked_by": self.acked_by,
            "acked_at": self.acked_at,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
        });
        if let Some(held) = &self.outcome_unknown {
            v["outcome_unknown"] = held.clone();
        }
        v
    }
}

/// The scope ids stored with a job's request (`null` for an unscoped or older job).
pub fn job_scope(job: &Job) -> Value {
    serde_json::from_str::<Value>(&job.request_json)
        .ok()
        .and_then(|v| v.get("scope").cloned())
        .unwrap_or(Value::Null)
}

/// The routing basis and reason stored with a job that request-level routing chose (`null` otherwise).
pub fn job_route(job: &Job) -> Value {
    serde_json::from_str::<Value>(&job.request_json)
        .ok()
        .and_then(|v| v.get("route").cloned())
        .unwrap_or(Value::Null)
}

/// What an `outcome_unknown` job holds: the attempt, the reservation kept for
/// it and the idempotency key it was sent under.
fn with_outcome(db: &Db, mut job: Job) -> GenResult<Job> {
    if job.awaits_resolution() {
        let key = db
            .submissions(&job.job_id)
            .map_err(je)?
            .into_iter()
            .find(|s| s.attempt == job.attempts)
            .map(|s| s.idempotency_key);
        let reserved = crate::ledger::attempt_reserved(db, &job.job_id, job.attempts)?;
        let recovered = job.awaits_adoption();
        job.outcome_unknown = Some(json!({
            "attempt": job.attempts,
            "reserved_usd": reserved,
            "idempotency_key": key,
            // A recovered result is already paid and received: adopt it, never resubmit.
            "recovered": recovered,
            // A result_unusable job is settled as spent: resubmit is refused, so it is not offered.
            "resolve": if recovered {
                json!(["adopt", "abandon"])
            } else if job.awaits_abandon_only() {
                json!(["abandon"])
            } else {
                json!(["resubmit", "abandon"])
            },
        }));
    }
    Ok(job)
}

pub fn get_job(db: &Db, job_id: &str) -> GenResult<Job> {
    with_outcome(db, db.record(job_id).map(Job::from).map_err(je)?)
}

/// Job for a caller-supplied request id.
pub fn get_job_by_request(db: &Db, request_id: &str) -> GenResult<Job> {
    with_outcome(
        db,
        db.record_by_request(request_id)
            .map(Job::from)
            .map_err(je)?,
    )
}

/// Newest-first jobs, optionally filtered. Returns up to `limit` rows and
/// whether more matched.
pub fn list_jobs(
    db: &Db,
    limit: i64,
    request_id: Option<&str>,
    status: Option<&str>,
) -> GenResult<(Vec<Job>, bool)> {
    let (rows, truncated) = db
        .list_records(&ListFilter {
            limit: Some(limit),
            status: status.map(Into::into),
            request_id: request_id.map(Into::into),
        })
        .map_err(je)?;
    let jobs = rows
        .into_iter()
        .map(|r| with_outcome(db, Job::from(r)))
        .collect::<GenResult<Vec<_>>>()?;
    Ok((jobs, truncated))
}

/// Ids of jobs a runner may drive, oldest first: not terminal and not held
/// as `outcome_unknown`.
pub fn runnable_ids(db: &Db) -> GenResult<Vec<String>> {
    db.read(|c| {
        let mut st = c.prepare(
            "SELECT job_id FROM jobs WHERE status IN (?1, ?2) ORDER BY created_at, rowid",
        )?;
        let ids = st
            .query_map([SUBMITTED, RUNNING], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ids)
    })
    .map_err(je)
}

pub struct NewJob<'a> {
    pub request_id: Option<&'a str>,
    pub kind: &'a str,
    pub provider: &'a str,
    pub model: &'a str,
    pub request_json: &'a str,
    pub est_usd: f64,
}

/// Insert a job as `submitted`, or return the existing job for the same
/// `request_id`. The mapping is created in the same transaction as the row.
/// Returns `(job, deduplicated)`.
pub fn insert_or_existing(db: &Db, n: &NewJob) -> GenResult<(Job, bool)> {
    let request: Value = serde_json::from_str(n.request_json)
        .map_err(|e| GenError::new("invalid_input", format!("request is not JSON: {e}")))?;
    let ext: Ext = json!({"provider": n.provider, "model": n.model, "est_usd": n.est_usd})
        .as_object()
        .cloned()
        .unwrap_or_default();
    let (rec, dedup) = db
        .submit_ext(
            &RkNewJob {
                request_id: n.request_id.map(Into::into),
                kind: n.kind.into(),
                request,
            },
            &ext,
        )
        .map_err(je)?;
    Ok((rec.into(), dedup))
}

/// Store the provider job id and move to `running` (no-op once terminal).
pub fn set_provider_id(db: &Db, job_id: &str, pid: &str) -> GenResult<()> {
    db.accept_submission(job_id, pid, Some(RUNNING))
        .map_err(je)?;
    Ok(())
}

/// Hold the job as `outcome_unknown` with the error that led there. `false`
/// when the job went terminal meanwhile (nothing changes).
pub fn mark_outcome_unknown(db: &Db, job_id: &str, error: &JobError) -> GenResult<bool> {
    Ok(db
        .hold_submission(job_id, error, OUTCOME_UNKNOWN)
        .map_err(je)?
        .is_some())
}

/// What an owner [`resolve`] did.
#[derive(Debug)]
pub enum Resolved {
    /// The job was held and the decision was applied (status and spend ledger moved together).
    Applied,
    /// RightKit refused with `illegal_transition` because the job had already been settled (the
    /// runner's outcome was stored first). Nothing was changed; this is the job as it stands.
    AlreadySettled(Job),
}

/// The note returned with [`Resolved::AlreadySettled`].
pub fn already_settled_note(job: &Job) -> String {
    format!(
        "the job had already been settled as {}; nothing was changed",
        job.status
    )
}

/// Settle a held job through RightKit, in one transaction with the spend ledger
/// (`ledger::SpendHook::on_resolve`). `Resubmit` returns the job to `submitted`; the next
/// `begin_submission` counts exactly one new attempt. `Abandon` ends it `failed`; `Adopt`
/// ends it `complete` with the supplied result. Not held: `not_outcome_unknown`.
///
/// `illegal_transition` is never retried and never touches the spend reservation: RightKit
/// checks the held status before it runs the hook, so `on_resolve` is not called and the
/// transaction is rolled back. When the job is terminal (the runner's outcome was stored just
/// before this settle) the row is re-read and returned as [`Resolved::AlreadySettled`]; any
/// other state stays the `not_outcome_unknown` error.
pub fn resolve(db: &Db, job_id: &str, how: &Resolution) -> GenResult<Resolved> {
    let to = matches!(how, Resolution::Resubmit).then_some(SUBMITTED);
    match db.resolve(job_id, how, to) {
        Ok(_) => Ok(Resolved::Applied),
        Err(e) if e.code == "illegal_transition" => {
            let job = get_job(db, job_id)?;
            if job.is_terminal() {
                Ok(Resolved::AlreadySettled(job))
            } else {
                Err(GenError::new("not_outcome_unknown", e.message))
            }
        }
        Err(e) => Err(je(e)),
    }
}

/// Keep a retryable error on the job without changing its status.
pub fn set_error(db: &Db, job_id: &str, error: &JobError) -> GenResult<()> {
    db.record_error(job_id, error).map_err(je)
}

pub fn mark_failed(db: &Db, job_id: &str, error: &JobError) -> GenResult<()> {
    db.fail(job_id, error).map_err(je)?;
    Ok(())
}

/// Complete a job with its stored result. Returns false if the job was
/// cancelled (or otherwise terminal) in the meantime.
pub fn mark_complete(db: &Db, job_id: &str, result: &Value) -> GenResult<bool> {
    db.complete(job_id, result).map_err(je)
}

pub fn cancel(db: &Db, job_id: &str) -> GenResult<Job> {
    db.cancel(job_id).map_err(je)?;
    get_job(db, job_id)
}

pub fn ack(db: &Db, job_id: &str, consumer: &str) -> GenResult<Job> {
    db.ack(job_id, consumer).map_err(je)?;
    get_job(db, job_id)
}

/// The `settings(key, value)` table through the owner: `rightkit-app-settings` `SqlSettings` (feature `sql`). Call
/// `get`/`set`/`list` on it with the connection a `Db::read`/`Db::write` closure hands you.
pub fn settings() -> rightkit_app_settings::SqlSettings {
    rightkit_app_settings::SqlSettings::new(rightkit_app_settings::Schema::new(
        crate::host::config().settings_namespace.as_str(),
        1,
    ))
}

/// A settings failure as the error type the `Db::read`/`Db::write` closures return.
pub fn settings_err(e: rightkit_app_settings::SqlError) -> JobError {
    JobError::new("db", e.to_string())
}

/// Append a progress event; its status and attempt are the job's current ones.
/// Returns the wire form.
pub fn add_event(db: &Db, job_id: &str, stage: &str, message: Option<&str>) -> GenResult<Value> {
    db.add_event(job_id, stage, None, message)
        .map(|e| event_value(&e))
        .map_err(je)
}

/// Like [`add_event`], recording an explicit status and attempt.
pub fn add_event_with(
    db: &Db,
    job_id: &str,
    status: &str,
    attempt: i64,
    stage: &str,
    message: Option<&str>,
) -> GenResult<Value> {
    db.add_event_with(job_id, Some(status), stage, Some(attempt), None, message)
        .map(|e| event_value(&e))
        .map_err(je)
}

/// the host's `job.progress` wire form (no `seq`, no `progress` fraction).
fn event_value(e: &Event) -> Value {
    let mut v = json!({
        "event": "job.progress",
        "job_id": e.job_id,
        "status": e.status,
        "stage": e.stage,
        "attempt": e.attempt,
        "at": e.at,
    });
    if let Some(m) = &e.message {
        v["message"] = json!(m);
    }
    v
}

/// Events for a job in order, as `(seq, event)`, with `seq > after`.
pub fn events_after(db: &Db, job_id: &str, after: i64) -> GenResult<Vec<(i64, Value)>> {
    Ok(db
        .events_after(job_id, after)
        .map_err(je)?
        .iter()
        .map(|e| (e.seq, event_value(e)))
        .collect())
}

pub fn events(db: &Db, job_id: &str) -> GenResult<Vec<Value>> {
    Ok(events_after(db, job_id, 0)?
        .into_iter()
        .map(|(_, v)| v)
        .collect())
}

pub fn latest_event(db: &Db, job_id: &str) -> GenResult<Value> {
    let all = events_after(db, job_id, 0)?;
    Ok(all
        .into_iter()
        .last()
        .map(|(_, v)| v)
        .unwrap_or(Value::Null))
}

/// One stored asset (§4.4), addressable by its stable id.
#[derive(Debug, Clone)]
pub struct AssetRow {
    pub asset_id: String,
    pub job_id: String,
    pub path: String,
    pub sha256: String,
    pub bytes: i64,
    pub mime: String,
}

impl From<Artifact> for AssetRow {
    fn from(a: Artifact) -> Self {
        AssetRow {
            asset_id: a.artifact_id,
            job_id: a.job_id,
            path: a.path,
            sha256: a.sha256,
            bytes: a.bytes,
            mime: a.mime,
        }
    }
}

/// Index every asset of a descriptor by asset id (idempotent).
pub fn record_assets(db: &Db, job_id: &str, descriptor: &Value) -> GenResult<()> {
    for a in descriptor["assets"].as_array().into_iter().flatten() {
        db.record_artifact(&Artifact {
            artifact_id: a["asset_id"].as_str().unwrap_or_default().into(),
            job_id: job_id.into(),
            path: a["path"].as_str().unwrap_or_default().into(),
            sha256: a["sha256"].as_str().unwrap_or_default().into(),
            bytes: a["bytes"].as_i64().unwrap_or(0),
            mime: a["mime"].as_str().unwrap_or_default().into(),
        })
        .map_err(je)?;
        if let Some(facts) = a.get("facts").filter(|f| !f.is_null()) {
            let (id, facts, scope) = (
                a["asset_id"].as_str().unwrap_or_default().to_string(),
                facts.to_string(),
                a.get("scope")
                    .filter(|s| !s.is_null())
                    .map(|s| s.to_string()),
            );
            let job_id = job_id.to_string();
            // First write wins: a repeat (job retry, adopt) keeps the recorded facts.
            db.write(move |c| {
                c.execute(
                    "INSERT OR IGNORE INTO asset_facts(asset_id, job_id, facts_json, scope_json, recorded_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![id, job_id, facts, scope, now()],
                )?;
                Ok(())
            })
            .map_err(je)?;
        }
    }
    Ok(())
}

/// Record one library asset that no generation produced (an acquired stock or archive item), the same
/// way an import records its asset: a completed job of `kind` owns the `assets` row, and the asset's
/// facts (origin, evidence, rights) go to the facts side table. Returns the job id. `asset` carries
/// `asset_id`, `path`, `sha256`, `bytes`, `mime` and `facts`.
pub fn record_standalone_asset(
    db: &Db,
    kind: &str,
    provider: &str,
    model: &str,
    request: &Value,
    asset: Value,
) -> GenResult<String> {
    let request_json = request.to_string();
    let (job, _) = insert_or_existing(
        db,
        &NewJob {
            request_id: None,
            kind,
            provider,
            model,
            request_json: &request_json,
            est_usd: 0.0,
        },
    )?;
    let descriptor = json!({
        "job_id": job.job_id, "request_id": Value::Null, "provider": provider, "model": model,
        "kind": kind, "created_at": now(), "acked_by": Value::Null, "acked_at": Value::Null,
        "assets": [asset],
    });
    record_assets(db, &job.job_id, &descriptor)?;
    mark_complete(db, &job.job_id, &descriptor)?;
    Ok(job.job_id)
}

/// The three asset facts and scope of one asset, read from the side table. An asset recorded
/// before the facts existed returns explicit `unknown` facts and a null scope, never a guess.
pub fn asset_facts(
    db: &Db,
    asset_id: &str,
) -> GenResult<(crate::host::asset_facts::AssetFacts, Value)> {
    use crate::host::asset_facts::AssetFacts;
    let id = asset_id.to_string();
    let row: Option<(String, Option<String>)> = db
        .read(move |c| {
            let row = c
                .query_row(
                    "SELECT facts_json, scope_json FROM asset_facts WHERE asset_id=?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            Ok(row)
        })
        .map_err(je)?;
    Ok(match row {
        Some((f, s)) => (
            serde_json::from_str::<Value>(&f)
                .map(|v| AssetFacts::from_value(&v))
                .unwrap_or_default(),
            s.and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .unwrap_or(Value::Null),
        ),
        None => (AssetFacts::unknown(), Value::Null),
    })
}

pub fn get_asset(db: &Db, asset_id: &str) -> GenResult<Option<AssetRow>> {
    match db.get_artifact(asset_id) {
        Ok(a) => return Ok(Some(a.into())),
        Err(e) if e.code == "artifact_not_found" => {}
        Err(e) => return Err(je(e)),
    }
    // Jobs finished before the assets table existed: read the descriptor.
    let like = format!("%{asset_id}%");
    let rows: Vec<(String, String)> = db
        .read(|c| {
            let mut st =
                c.prepare("SELECT job_id, result_json FROM jobs WHERE result_json LIKE ?1")?;
            let rows = st
                .query_map([like], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .map_err(je)?;
    for (job_id, rj) in rows {
        let Ok(v) = serde_json::from_str::<Value>(&rj) else {
            continue;
        };
        for a in v["assets"].as_array().into_iter().flatten() {
            if a["asset_id"].as_str() == Some(asset_id) {
                return Ok(Some(AssetRow {
                    asset_id: asset_id.to_string(),
                    job_id,
                    path: a["path"].as_str().unwrap_or_default().to_string(),
                    sha256: a["sha256"].as_str().unwrap_or_default().to_string(),
                    bytes: a["bytes"].as_i64().unwrap_or(0),
                    mime: a["mime"].as_str().unwrap_or_default().to_string(),
                }));
            }
        }
    }
    Ok(None)
}
