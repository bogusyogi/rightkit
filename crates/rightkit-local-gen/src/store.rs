// Local image generation: durable SQLite store — see docs/plans/local-image-generation.md

use std::io::Read;
use std::path::Path;

use rightkit_db::rusqlite::{
    self, params, params_from_iter, Connection, ErrorCode, OptionalExtension, Row, ToSql,
};
use rightkit_db::DbOptions;
use rightkit_fs::digest::Sha256Hasher;
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

const SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS conversations(
    id TEXT PRIMARY KEY,
    title TEXT,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS messages(
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    parent_id TEXT,
    role TEXT NOT NULL,
    kind TEXT NOT NULL,
    prompt TEXT,
    reference_ids TEXT NOT NULL DEFAULT '[]',
    job_id TEXT,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS jobs(
    id TEXT PRIMARY KEY,
    request_id TEXT NOT NULL UNIQUE,
    request_hash TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    message_id TEXT,
    parent_message_id TEXT,
    profile_id TEXT NOT NULL,
    profile_revision INTEGER NOT NULL,
    prompt TEXT NOT NULL,
    negative_prompt TEXT NOT NULL DEFAULT '',
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    seed INTEGER NOT NULL,
    reference_ids TEXT NOT NULL DEFAULT '[]',
    overrides TEXT NOT NULL DEFAULT '{}',
    state TEXT NOT NULL,
    stage TEXT NOT NULL DEFAULT '',
    progress REAL,
    queue_position INTEGER NOT NULL DEFAULT 0,
    provider_job_id TEXT,
    runtime_session_id TEXT,
    error_code TEXT,
    error_message TEXT,
    retry_of TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    finished_at TEXT
);
CREATE TABLE IF NOT EXISTS job_events(
    job_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    state TEXT NOT NULL,
    stage TEXT NOT NULL,
    progress REAL,
    runtime_session_id TEXT,
    error_code TEXT,
    at TEXT NOT NULL,
    PRIMARY KEY(job_id, revision)
);
CREATE TABLE IF NOT EXISTS image_refs(
    id TEXT PRIMARY KEY,
    stored_path TEXT NOT NULL,
    sha256 TEXT NOT NULL UNIQUE,
    bytes INTEGER NOT NULL,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    mime TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS artifacts(
    id TEXT PRIMARY KEY,
    job_id TEXT NOT NULL,
    path TEXT NOT NULL,
    sidecar_path TEXT,
    manifest_path TEXT,
    sha256 TEXT,
    bytes INTEGER,
    width INTEGER,
    height INTEGER,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS components(
    profile_id TEXT NOT NULL,
    profile_revision INTEGER NOT NULL,
    kind TEXT NOT NULL,
    file TEXT NOT NULL,
    sha256 TEXT,
    bytes INTEGER,
    state TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(profile_id, profile_revision, kind)
);
CREATE TABLE IF NOT EXISTS meta(
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

/// Schema steps, numbered by `PRAGMA user_version`: v1 is the original schema, so databases
/// created before the move to `rightkit_db::migrate` are already at v1 and are left as they are.
const MIGRATIONS: &[rightkit_db::Migration] =
    &[rightkit_db::Migration::sql(1, "initial", SCHEMA_V1)];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Loading,
    Submitting,
    Running,
    Finalizing,
    Succeeded,
    Failed,
    Cancelled,
    Cancelling,
    Interrupted,
    Paused,
}

impl JobState {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Loading => "loading",
            JobState::Submitting => "submitting",
            JobState::Running => "running",
            JobState::Finalizing => "finalizing",
            JobState::Succeeded => "succeeded",
            JobState::Failed => "failed",
            JobState::Cancelled => "cancelled",
            JobState::Cancelling => "cancelling",
            JobState::Interrupted => "interrupted",
            JobState::Paused => "paused",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        Some(match value {
            "queued" => JobState::Queued,
            "loading" => JobState::Loading,
            "submitting" => JobState::Submitting,
            "running" => JobState::Running,
            "finalizing" => JobState::Finalizing,
            "succeeded" => JobState::Succeeded,
            "failed" => JobState::Failed,
            "cancelled" => JobState::Cancelled,
            "cancelling" => JobState::Cancelling,
            "interrupted" => JobState::Interrupted,
            "paused" => JobState::Paused,
            _ => return None,
        })
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            JobState::Succeeded | JobState::Failed | JobState::Cancelled | JobState::Interrupted
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationRecord {
    pub id: String,
    pub title: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRecord {
    pub id: String,
    pub conversation_id: String,
    pub parent_id: Option<String>,
    pub role: String,
    pub kind: String,
    pub prompt: Option<String>,
    pub reference_ids: Vec<String>,
    pub job_id: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: String,
    pub request_id: String,
    pub request_hash: String,
    pub conversation_id: String,
    pub message_id: Option<String>,
    pub parent_message_id: Option<String>,
    pub profile_id: String,
    pub profile_revision: i64,
    pub prompt: String,
    pub negative_prompt: String,
    pub width: i64,
    pub height: i64,
    pub seed: i64,
    pub reference_ids: Vec<String>,
    pub overrides: serde_json::Value,
    pub state: JobState,
    pub stage: String,
    pub progress: Option<f64>,
    pub queue_position: i64,
    pub provider_job_id: Option<String>,
    pub runtime_session_id: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub retry_of: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobEventRecord {
    pub job_id: String,
    pub revision: i64,
    pub state: JobState,
    pub stage: String,
    pub progress: Option<f64>,
    pub runtime_session_id: Option<String>,
    pub error_code: Option<String>,
    pub at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceRecord {
    pub id: String,
    pub stored_path: String,
    pub sha256: String,
    pub bytes: i64,
    pub width: i64,
    pub height: i64,
    pub mime: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRecord {
    pub id: String,
    pub job_id: String,
    pub path: String,
    pub sidecar_path: Option<String>,
    pub manifest_path: Option<String>,
    pub sha256: Option<String>,
    pub bytes: i64,
    pub width: i64,
    pub height: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentRecord {
    pub profile_id: String,
    pub profile_revision: i64,
    pub kind: String,
    pub file: String,
    pub sha256: Option<String>,
    pub bytes: Option<i64>,
    pub state: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewJob {
    pub request_id: String,
    pub conversation_id: String,
    pub message_id: Option<String>,
    pub parent_message_id: Option<String>,
    pub profile_id: String,
    pub profile_revision: i64,
    pub prompt: String,
    pub negative_prompt: String,
    pub width: i64,
    pub height: i64,
    pub seed: i64,
    pub reference_ids: Vec<String>,
    pub overrides: serde_json::Value,
    pub retry_of: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewMessage {
    pub conversation_id: String,
    pub parent_id: Option<String>,
    pub role: String,
    pub kind: String,
    pub prompt: Option<String>,
    pub reference_ids: Vec<String>,
    pub job_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobUpdate {
    pub state: JobState,
    pub stage: String,
    pub progress: Option<f64>,
    pub provider_job_id: Option<Option<String>>,
    pub runtime_session_id: Option<Option<String>>,
    pub error_code: Option<Option<String>>,
    pub error_message: Option<Option<String>>,
}

impl JobUpdate {
    pub fn transition(state: JobState, stage: impl Into<String>) -> Self {
        JobUpdate {
            state,
            stage: stage.into(),
            progress: None,
            provider_job_id: None,
            runtime_session_id: None,
            error_code: None,
            error_message: None,
        }
    }
}

#[derive(Debug)]
pub enum SubmitOutcome {
    Created(JobRecord),
    Existing(JobRecord),
}

#[derive(Debug, Default)]
pub struct ReconcileReport {
    pub interrupted: u32,
    pub cancelled: u32,
    pub paused: u32,
    pub finalizing_left: u32,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        let mut options = DbOptions::new(path);
        options.foreign_keys = true;
        let conn = options
            .open_writer()
            .map_err(|e| format!("open {}: {e}", path.display()))?;
        let store = Store { conn };
        store.migrate(path)?;
        Ok(store)
    }

    fn migrate(&self, path: &Path) -> Result<(), String> {
        rightkit_db::migrate(
            &self.conn,
            Some(path),
            MIGRATIONS,
            &rightkit_db::MigrateOptions::default(),
        )
        .map_err(|e| e.to_string())?;
        // Additive: the scope ids (venture, channel, account) of the host job that
        // adopted this artifact. Null for an unscoped job and for every older row.
        rightkit_db::add_column_if_missing(&self.conn, "artifacts", "scope_json", "TEXT")
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Runs simple store operations in one transaction; closure must not start nested transactions.
    pub fn transaction<T>(
        &self,
        action: impl FnOnce(&Store) -> Result<T, String>,
    ) -> Result<T, String> {
        let tx = rightkit_db::write_txn_unchecked(&self.conn).map_err(|e| e.to_string())?;
        let result = action(self)?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(result)
    }

    pub fn submit(&self, new_job: &NewJob) -> Result<SubmitOutcome, String> {
        let request_hash = sha256_hex(
            serde_json::to_string(&json!({
                "conversation_id": new_job.conversation_id,
                "parent_message_id": new_job.parent_message_id,
                "profile_id": new_job.profile_id,
                "profile_revision": new_job.profile_revision,
                "prompt": new_job.prompt,
                "negative_prompt": new_job.negative_prompt,
                "width": new_job.width,
                "height": new_job.height,
                "seed": new_job.seed,
                "reference_ids": new_job.reference_ids,
                "overrides": new_job.overrides,
            }))
            .map_err(|e| e.to_string())?
            .as_bytes(),
        );
        let id = new_id("job");
        let now = now_iso();
        let reference_ids =
            serde_json::to_string(&new_job.reference_ids).map_err(|e| e.to_string())?;
        let overrides = serde_json::to_string(&new_job.overrides).map_err(|e| e.to_string())?;
        let result = self.conn.execute(
            "INSERT INTO jobs (id, request_id, request_hash, conversation_id, message_id,
                parent_message_id, profile_id, profile_revision, prompt, negative_prompt,
                width, height, seed, reference_ids, overrides, state, queue_position,
                retry_of, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,'queued',
                COALESCE((SELECT MAX(queue_position) FROM jobs WHERE conversation_id=?4)+1,0),
                ?16,?17,?17)",
            params![
                id,
                new_job.request_id,
                request_hash,
                new_job.conversation_id,
                new_job.message_id,
                new_job.parent_message_id,
                new_job.profile_id,
                new_job.profile_revision,
                new_job.prompt,
                new_job.negative_prompt,
                new_job.width,
                new_job.height,
                new_job.seed,
                reference_ids,
                overrides,
                new_job.retry_of,
                now,
            ],
        );
        match result {
            Ok(_) => self
                .job(&id)?
                .map(SubmitOutcome::Created)
                .ok_or_else(|| format!("job vanished after insert: {id}")),
            Err(rusqlite::Error::SqliteFailure(failure, _))
                if failure.code == ErrorCode::ConstraintViolation =>
            {
                match self.job_by_request_id(&new_job.request_id)? {
                    Some(existing) if existing.request_hash == request_hash => {
                        Ok(SubmitOutcome::Existing(existing))
                    }
                    Some(_) => Err(format!("request_id_conflict: {}", new_job.request_id)),
                    None => Err(format!("job constraint violation: {}", new_job.request_id)),
                }
            }
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn next_queued(&self) -> Result<Option<JobRecord>, String> {
        self.conn
            .query_row(
                "SELECT * FROM jobs WHERE state='queued'
                 ORDER BY queue_position ASC, created_at ASC LIMIT 1",
                [],
                job_from_row,
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    /// Atomically claims the oldest queued job for a worker.
    pub fn claim_next_queued(&self) -> Result<Option<JobRecord>, String> {
        self.claim_next_queued_for(None)
    }

    /// Atomically claims oldest queued job matching worker request prefix.
    /// `None` keeps desktop behavior and claims any queued job.
    pub fn claim_next_queued_for(
        &self,
        request_prefix: Option<&str>,
    ) -> Result<Option<JobRecord>, String> {
        let tx = rightkit_db::write_txn_unchecked(&self.conn).map_err(|e| e.to_string())?;
        let candidate = match request_prefix {
            Some(prefix) => {
                let pattern = format!("{prefix}%");
                tx.query_row(
                    "SELECT id FROM jobs WHERE state='queued' AND request_id LIKE ?1
                     ORDER BY queue_position ASC, created_at ASC LIMIT 1",
                    params![pattern],
                    |row| row.get::<_, String>(0),
                )
                .optional()
            }
            None => tx
                .query_row(
                    "SELECT id FROM jobs WHERE state='queued'
                     ORDER BY queue_position ASC, created_at ASC LIMIT 1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .optional(),
        }
        .map_err(|e| e.to_string())?;
        let Some(id) = candidate else {
            tx.commit().map_err(|e| e.to_string())?;
            return Ok(None);
        };
        let mut update = JobUpdate::transition(JobState::Loading, "loading");
        update.progress = Some(0.0);
        let claimed = set_state_in_connection(&tx, &id, &update, false)?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(Some(claimed))
    }

    /// Cancels only when the row is still queued, closing stale-read races with worker claims.
    pub fn cancel_queued_if_queued(&self, job_id: &str) -> Result<Option<JobRecord>, String> {
        let tx = rightkit_db::write_txn_unchecked(&self.conn).map_err(|e| e.to_string())?;
        let Some(prior) = query_job(&tx, job_id).map_err(|e| e.to_string())? else {
            return Err(format!("job not found: {job_id}"));
        };
        if prior.state != JobState::Queued {
            tx.commit().map_err(|e| e.to_string())?;
            return Ok(None);
        }
        let now = now_iso();
        let changed = tx
            .execute(
                "UPDATE jobs SET state='cancelled', stage='cancelled', error_code='user_cancel',
                    updated_at=?1, finished_at=?1 WHERE id=?2 AND state='queued'",
                params![now, job_id],
            )
            .map_err(|e| e.to_string())?;
        if changed == 0 {
            tx.commit().map_err(|e| e.to_string())?;
            return Ok(None);
        }
        tx.execute(
            "INSERT INTO job_events (job_id, revision, state, stage, progress,
                runtime_session_id, error_code, at)
             VALUES (?1, COALESCE((SELECT MAX(revision) FROM job_events WHERE job_id=?1)+1,0),
                'cancelled','cancelled',?2,?3,'user_cancel',?4)",
            params![job_id, prior.progress, prior.runtime_session_id, now],
        )
        .map_err(|e| e.to_string())?;
        let record = query_job(&tx, job_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("job vanished: {job_id}"))?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(Some(record))
    }

    pub fn set_state(&self, job_id: &str, upd: &JobUpdate) -> Result<JobRecord, String> {
        let tx = rightkit_db::write_txn_unchecked(&self.conn).map_err(|e| e.to_string())?;
        let record = set_state_in_connection(&tx, job_id, upd, false)?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(record)
    }

    /// Commit validated artifact rows and terminal success in one SQLite transaction.
    pub fn complete_finalization(
        &self,
        job_id: &str,
        artifacts: &[ArtifactRecord],
    ) -> Result<JobRecord, String> {
        self.complete_finalization_with_stage(job_id, artifacts, "succeeded")
    }

    /// Recover a validated on-disk finalization, replacing any partial artifact rows atomically.
    pub fn recover_finalization(
        &self,
        job_id: &str,
        artifacts: &[ArtifactRecord],
    ) -> Result<JobRecord, String> {
        self.complete_finalization_with_stage(job_id, artifacts, "recovered")
    }

    fn complete_finalization_with_stage(
        &self,
        job_id: &str,
        artifacts: &[ArtifactRecord],
        stage: &str,
    ) -> Result<JobRecord, String> {
        if artifacts.is_empty() {
            return Err("cannot succeed finalization without artifacts".to_string());
        }
        let tx = rightkit_db::write_txn_unchecked(&self.conn).map_err(|e| e.to_string())?;
        let prior = query_job(&tx, job_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("job not found: {job_id}"))?;
        if prior.state != JobState::Finalizing {
            return Err(format!(
                "cannot complete finalization from {}",
                prior.state.as_str()
            ));
        }
        tx.execute("DELETE FROM artifacts WHERE job_id=?1", params![job_id])
            .map_err(|e| e.to_string())?;
        for artifact in artifacts {
            insert_artifact_on(&tx, artifact)?;
        }
        let mut update = JobUpdate::transition(JobState::Succeeded, stage);
        update.progress = Some(1.0);
        let record = set_state_in_connection(&tx, job_id, &update, true)?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(record)
    }

    pub fn finalizing_jobs(&self) -> Result<Vec<JobRecord>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM jobs WHERE state='finalizing' ORDER BY created_at, rowid")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], job_from_row)
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }

    pub fn reconcile_startup(&self) -> Result<ReconcileReport, String> {
        self.reconcile_startup_inner(true)
    }

    /// Reconcile after waiting for another service owner without pausing
    /// newly queued work.
    pub fn reconcile_startup_preserving_queue(&self) -> Result<ReconcileReport, String> {
        self.reconcile_startup_inner(false)
    }

    fn reconcile_startup_inner(&self, pause_queued: bool) -> Result<ReconcileReport, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, state, stage FROM jobs
                 WHERE state IN ('loading','submitting','running','cancelling')
                 ORDER BY created_at, rowid",
            )
            .map_err(|e| e.to_string())?;
        let rows: Vec<(String, String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<_>>()
            .map_err(|e| e.to_string())?;
        drop(stmt);
        let mut report = ReconcileReport::default();
        for (id, state, stage) in rows {
            if state == "cancelling" {
                self.set_state(&id, &JobUpdate::transition(JobState::Cancelled, stage))?;
                report.cancelled += 1;
            } else {
                let mut upd = JobUpdate::transition(JobState::Interrupted, stage);
                upd.error_code = Some(Some("restart_interrupted".to_string()));
                self.set_state(&id, &upd)?;
                report.interrupted += 1;
            }
        }
        report.finalizing_left = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE state='finalizing'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map_err(|e| e.to_string())? as u32;
        report.paused = if pause_queued {
            let queued: Vec<String> = {
                let mut stmt = self
                    .conn
                    .prepare("SELECT id FROM jobs WHERE state='queued' AND request_id NOT LIKE 'engine:%'")
                    .map_err(|e| e.to_string())?;
                let rows = stmt
                    .query_map([], |r| r.get::<_, String>(0))
                    .map_err(|e| e.to_string())?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(|e| e.to_string())?;
                rows
            };
            let mut count = 0u32;
            for id in queued {
                self.set_state(&id, &JobUpdate::transition(JobState::Paused, "paused"))?;
                count += 1;
            }
            count
        } else {
            0
        };
        Ok(report)
    }

    /// Graceful service shutdown preserves finalizing commits and pauses unclaimed work.
    pub fn reconcile_shutdown(&self) -> Result<(), String> {
        let tx = rightkit_db::write_txn_unchecked(&self.conn).map_err(|e| e.to_string())?;
        let mut stmt = tx
            .prepare(
                "SELECT id, state FROM jobs WHERE state IN
                    ('queued','loading','submitting','running','cancelling')
                    AND (state <> 'queued' OR request_id NOT LIKE 'engine:%')
                 ORDER BY created_at, rowid",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        drop(stmt);
        for (id, state) in rows {
            let prior = JobState::from_str(&state)
                .ok_or_else(|| format!("unknown job state during shutdown: {state}"))?;
            let update = if prior == JobState::Queued {
                JobUpdate::transition(JobState::Paused, "paused")
            } else {
                let mut update = JobUpdate::transition(JobState::Cancelled, "shutdown");
                update.error_code = Some(Some("app_shutdown".to_string()));
                update.error_message = Some(Some("application shut down".to_string()));
                update
            };
            set_state_in_connection(&tx, &id, &update, false)?;
        }
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn job(&self, id: &str) -> Result<Option<JobRecord>, String> {
        query_job(&self.conn, id).map_err(|e| e.to_string())
    }

    pub fn job_by_request_id(&self, request_id: &str) -> Result<Option<JobRecord>, String> {
        self.conn
            .query_row(
                "SELECT * FROM jobs WHERE request_id=?1",
                params![request_id],
                job_from_row,
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    pub fn job_events(&self, job_id: &str) -> Result<Vec<JobEventRecord>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM job_events WHERE job_id=?1 ORDER BY revision ASC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![job_id], event_from_row)
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }

    pub fn list_jobs(
        &self,
        conversation_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<JobRecord>, String> {
        match conversation_id {
            Some(conversation_id) => {
                let mut stmt = self
                    .conn
                    .prepare(
                        "SELECT * FROM jobs WHERE conversation_id=?1
                         ORDER BY created_at DESC, rowid DESC LIMIT ?2",
                    )
                    .map_err(|e| e.to_string())?;
                let rows = stmt
                    .query_map(params![conversation_id, limit as i64], job_from_row)
                    .map_err(|e| e.to_string())?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(|e| e.to_string())?;
                Ok(rows)
            }
            None => {
                let mut stmt = self
                    .conn
                    .prepare("SELECT * FROM jobs ORDER BY created_at DESC, rowid DESC LIMIT ?1")
                    .map_err(|e| e.to_string())?;
                let rows = stmt
                    .query_map(params![limit as i64], job_from_row)
                    .map_err(|e| e.to_string())?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(|e| e.to_string())?;
                Ok(rows)
            }
        }
    }

    pub fn conversation(&self, id: &str) -> Result<Option<ConversationRecord>, String> {
        self.conn
            .query_row(
                "SELECT * FROM conversations WHERE id=?1",
                params![id],
                conversation_from_row,
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    pub fn list_conversations(&self, limit: u32) -> Result<Vec<ConversationRecord>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM conversations ORDER BY created_at DESC, rowid DESC LIMIT ?1")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![limit as i64], conversation_from_row)
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }

    pub fn create_conversation(&self, title: Option<&str>) -> Result<ConversationRecord, String> {
        let id = new_id("conv");
        let now = now_iso();
        self.conn
            .execute(
                "INSERT INTO conversations (id, title, created_at) VALUES (?1,?2,?3)",
                params![id, title, now],
            )
            .map_err(|e| e.to_string())?;
        self.conversation(&id)?
            .ok_or_else(|| format!("conversation vanished: {id}"))
    }

    pub fn insert_message(&self, msg: &NewMessage) -> Result<MessageRecord, String> {
        let id = new_id("msg");
        let now = now_iso();
        let reference_ids = serde_json::to_string(&msg.reference_ids).map_err(|e| e.to_string())?;
        self.conn
            .execute(
                "INSERT INTO messages (id, conversation_id, parent_id, role, kind, prompt,
                    reference_ids, job_id, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    id,
                    msg.conversation_id,
                    msg.parent_id,
                    msg.role,
                    msg.kind,
                    msg.prompt,
                    reference_ids,
                    msg.job_id,
                    now,
                ],
            )
            .map_err(|e| e.to_string())?;
        self.message(&id)?
            .ok_or_else(|| format!("message vanished: {id}"))
    }

    pub fn list_messages(&self, conversation_id: &str) -> Result<Vec<MessageRecord>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT * FROM messages WHERE conversation_id=?1
                 ORDER BY created_at ASC, rowid ASC",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![conversation_id], message_from_row)
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }

    pub fn message(&self, id: &str) -> Result<Option<MessageRecord>, String> {
        self.conn
            .query_row(
                "SELECT * FROM messages WHERE id=?1",
                params![id],
                message_from_row,
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    pub fn insert_reference(&self, reference: &ReferenceRecord) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO image_refs (id, stored_path, sha256, bytes, width, height, mime,
                    created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    reference.id,
                    reference.stored_path,
                    reference.sha256,
                    reference.bytes,
                    reference.width,
                    reference.height,
                    reference.mime,
                    reference.created_at,
                ],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn reference(&self, id: &str) -> Result<Option<ReferenceRecord>, String> {
        self.conn
            .query_row(
                "SELECT * FROM image_refs WHERE id=?1",
                params![id],
                reference_from_row,
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    pub fn reference_by_sha256(&self, sha256: &str) -> Result<Option<ReferenceRecord>, String> {
        self.conn
            .query_row(
                "SELECT * FROM image_refs WHERE sha256=?1",
                params![sha256],
                reference_from_row,
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    pub fn references_by_ids(&self, ids: &[String]) -> Result<Vec<ReferenceRecord>, String> {
        let mut records = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(record) = self.reference(id)? {
                records.push(record);
            }
        }
        Ok(records)
    }

    pub fn insert_artifact(&self, artifact: &ArtifactRecord) -> Result<(), String> {
        insert_artifact_on(&self.conn, artifact)
    }

    /// Record the scope ids of the scoped host job that adopted the artifact at `path`
    /// (`scope_json` is the `{venture, channel, account}` object). Returns the rows updated; zero
    /// when no row matched.
    pub fn set_artifact_scope_by_path(
        &self,
        path: &str,
        scope_json: &str,
    ) -> Result<usize, String> {
        self.conn
            .execute(
                "UPDATE artifacts SET scope_json=?2 WHERE path=?1",
                params![path, scope_json],
            )
            .map_err(|e| e.to_string())
    }

    pub fn artifacts_for_job(&self, job_id: &str) -> Result<Vec<ArtifactRecord>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM artifacts WHERE job_id=?1 ORDER BY created_at ASC, rowid ASC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![job_id], artifact_from_row)
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }

    pub fn artifact_by_path(&self, path: &str) -> Result<Option<ArtifactRecord>, String> {
        self.conn
            .query_row(
                "SELECT * FROM artifacts WHERE path=?1",
                params![path],
                artifact_from_row,
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    /// Drop the history row(s) for one gallery output path. The caller removes the file itself;
    /// returns how many rows went (zero when none matched, so a repeat call is harmless).
    pub fn delete_artifacts_by_path(&self, path: &str) -> Result<usize, String> {
        self.conn
            .execute("DELETE FROM artifacts WHERE path=?1", params![path])
            .map_err(|e| e.to_string())
    }

    /// Drops every component record of a profile, all revisions (uninstall).
    pub fn delete_components_for_profile(&self, profile_id: &str) -> Result<usize, String> {
        self.conn
            .execute(
                "DELETE FROM components WHERE profile_id=?1",
                params![profile_id],
            )
            .map_err(|e| e.to_string())
    }

    pub fn upsert_component(&self, component: &ComponentRecord) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO components (profile_id, profile_revision, kind, file, sha256, bytes,
                    state, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                 ON CONFLICT(profile_id, profile_revision, kind) DO UPDATE SET
                    file=excluded.file, sha256=excluded.sha256, bytes=excluded.bytes,
                    state=excluded.state, updated_at=excluded.updated_at",
                params![
                    component.profile_id,
                    component.profile_revision,
                    component.kind,
                    component.file,
                    component.sha256,
                    component.bytes,
                    component.state,
                    component.updated_at,
                ],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn components_for_profile(
        &self,
        profile_id: &str,
        revision: i64,
    ) -> Result<Vec<ComponentRecord>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT * FROM components WHERE profile_id=?1 AND profile_revision=?2
                 ORDER BY kind ASC",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![profile_id, revision], component_from_row)
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }
}

fn query_job(conn: &Connection, id: &str) -> rusqlite::Result<Option<JobRecord>> {
    conn.query_row("SELECT * FROM jobs WHERE id=?1", params![id], job_from_row)
        .optional()
}

fn insert_artifact_on(conn: &Connection, artifact: &ArtifactRecord) -> Result<(), String> {
    conn.execute(
        "INSERT INTO artifacts (id, job_id, path, sidecar_path, manifest_path, sha256,
            bytes, width, height, created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            artifact.id,
            artifact.job_id,
            artifact.path,
            artifact.sidecar_path,
            artifact.manifest_path,
            artifact.sha256,
            artifact.bytes,
            artifact.width,
            artifact.height,
            artifact.created_at,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn set_state_in_connection(
    conn: &Connection,
    job_id: &str,
    upd: &JobUpdate,
    allow_success: bool,
) -> Result<JobRecord, String> {
    let prior = query_job(conn, job_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("job not found: {job_id}"))?;
    if prior.state.is_terminal() {
        return Err(format!("job is terminal: {}", prior.state.as_str()));
    }
    if upd.state == JobState::Succeeded && !allow_success {
        return Err("succeeded state requires atomic artifact finalization".to_string());
    }
    if !legal_transition(prior.state, upd.state) {
        return Err(format!(
            "illegal job transition: {} -> {}",
            prior.state.as_str(),
            upd.state.as_str()
        ));
    }
    let now = now_iso();
    let mut sql = String::from("UPDATE jobs SET state=?1, stage=?2, updated_at=?3");
    let mut values: Vec<Box<dyn ToSql>> = vec![
        Box::new(upd.state.as_str().to_string()),
        Box::new(upd.stage.clone()),
        Box::new(now.clone()),
    ];
    let mut index = 4usize;
    if let Some(progress) = upd.progress {
        sql.push_str(&format!(", progress=?{index}"));
        values.push(Box::new(progress));
        index += 1;
    }
    for (column, value) in [
        ("provider_job_id", &upd.provider_job_id),
        ("runtime_session_id", &upd.runtime_session_id),
        ("error_code", &upd.error_code),
        ("error_message", &upd.error_message),
    ] {
        if let Some(value) = value {
            sql.push_str(&format!(", {column}=?{index}"));
            values.push(Box::new(value.clone()));
            index += 1;
        }
    }
    if upd.state.is_terminal() {
        sql.push_str(&format!(", finished_at=?{index}"));
        values.push(Box::new(now.clone()));
        index += 1;
    }
    sql.push_str(&format!(" WHERE id=?{index} AND state=?{}", index + 1));
    values.push(Box::new(job_id.to_string()));
    values.push(Box::new(prior.state.as_str().to_string()));
    let bound: Vec<&dyn ToSql> = values.iter().map(|v| &**v as &dyn ToSql).collect();
    let changed = conn
        .execute(&sql, params_from_iter(bound))
        .map_err(|e| e.to_string())?;
    if changed != 1 {
        return Err(format!("job state changed concurrently: {job_id}"));
    }
    conn.execute(
        "INSERT INTO job_events (job_id, revision, state, stage, progress,
            runtime_session_id, error_code, at)
         VALUES (?1,
            COALESCE((SELECT MAX(revision) FROM job_events WHERE job_id=?1)+1,0),
            ?2,?3,?4,?5,?6,?7)",
        params![
            job_id,
            upd.state.as_str(),
            upd.stage,
            upd.progress.or(prior.progress),
            upd.runtime_session_id
                .clone()
                .unwrap_or_else(|| prior.runtime_session_id.clone()),
            upd.error_code
                .clone()
                .unwrap_or_else(|| prior.error_code.clone()),
            now,
        ],
    )
    .map_err(|e| e.to_string())?;
    query_job(conn, job_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("job vanished: {job_id}"))
}

fn legal_transition(from: JobState, to: JobState) -> bool {
    use JobState::*;
    matches!(
        (from, to),
        (Queued, Loading | Paused | Cancelled)
            | (
                Loading,
                Loading | Submitting | Cancelling | Cancelled | Interrupted | Failed
            )
            | (
                Submitting,
                Submitting | Running | Cancelling | Cancelled | Interrupted | Failed
            )
            | (
                Running,
                Running | Finalizing | Cancelling | Cancelled | Interrupted | Failed
            )
            | (Finalizing, Succeeded | Interrupted | Failed)
            | (Cancelling, Cancelled | Interrupted)
            | (Paused, Queued | Cancelled)
    )
}

fn json_column<T: serde::de::DeserializeOwned>(text: &str, column: &str) -> rusqlite::Result<T> {
    serde_json::from_str(text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("{column}: {e}").into(),
        )
    })
}

fn state_column(text: &str) -> rusqlite::Result<JobState> {
    JobState::from_str(text).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("unknown job state: {text}").into(),
        )
    })
}

fn conversation_from_row(row: &Row<'_>) -> rusqlite::Result<ConversationRecord> {
    Ok(ConversationRecord {
        id: row.get("id")?,
        title: row.get("title")?,
        created_at: row.get("created_at")?,
    })
}

fn message_from_row(row: &Row<'_>) -> rusqlite::Result<MessageRecord> {
    let reference_ids: String = row.get("reference_ids")?;
    Ok(MessageRecord {
        id: row.get("id")?,
        conversation_id: row.get("conversation_id")?,
        parent_id: row.get("parent_id")?,
        role: row.get("role")?,
        kind: row.get("kind")?,
        prompt: row.get("prompt")?,
        reference_ids: json_column(&reference_ids, "reference_ids")?,
        job_id: row.get("job_id")?,
        created_at: row.get("created_at")?,
    })
}

fn job_from_row(row: &Row<'_>) -> rusqlite::Result<JobRecord> {
    let reference_ids: String = row.get("reference_ids")?;
    let overrides: String = row.get("overrides")?;
    let state: String = row.get("state")?;
    Ok(JobRecord {
        id: row.get("id")?,
        request_id: row.get("request_id")?,
        request_hash: row.get("request_hash")?,
        conversation_id: row.get("conversation_id")?,
        message_id: row.get("message_id")?,
        parent_message_id: row.get("parent_message_id")?,
        profile_id: row.get("profile_id")?,
        profile_revision: row.get("profile_revision")?,
        prompt: row.get("prompt")?,
        negative_prompt: row.get("negative_prompt")?,
        width: row.get("width")?,
        height: row.get("height")?,
        seed: row.get("seed")?,
        reference_ids: json_column(&reference_ids, "reference_ids")?,
        overrides: json_column(&overrides, "overrides")?,
        state: state_column(&state)?,
        stage: row.get("stage")?,
        progress: row.get("progress")?,
        queue_position: row.get("queue_position")?,
        provider_job_id: row.get("provider_job_id")?,
        runtime_session_id: row.get("runtime_session_id")?,
        error_code: row.get("error_code")?,
        error_message: row.get("error_message")?,
        retry_of: row.get("retry_of")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        finished_at: row.get("finished_at")?,
    })
}

fn event_from_row(row: &Row<'_>) -> rusqlite::Result<JobEventRecord> {
    let state: String = row.get("state")?;
    Ok(JobEventRecord {
        job_id: row.get("job_id")?,
        revision: row.get("revision")?,
        state: state_column(&state)?,
        stage: row.get("stage")?,
        progress: row.get("progress")?,
        runtime_session_id: row.get("runtime_session_id")?,
        error_code: row.get("error_code")?,
        at: row.get("at")?,
    })
}

fn reference_from_row(row: &Row<'_>) -> rusqlite::Result<ReferenceRecord> {
    Ok(ReferenceRecord {
        id: row.get("id")?,
        stored_path: row.get("stored_path")?,
        sha256: row.get("sha256")?,
        bytes: row.get("bytes")?,
        width: row.get("width")?,
        height: row.get("height")?,
        mime: row.get("mime")?,
        created_at: row.get("created_at")?,
    })
}

fn artifact_from_row(row: &Row<'_>) -> rusqlite::Result<ArtifactRecord> {
    Ok(ArtifactRecord {
        id: row.get("id")?,
        job_id: row.get("job_id")?,
        path: row.get("path")?,
        sidecar_path: row.get("sidecar_path")?,
        manifest_path: row.get("manifest_path")?,
        sha256: row.get("sha256")?,
        bytes: row.get("bytes")?,
        width: row.get("width")?,
        height: row.get("height")?,
        created_at: row.get("created_at")?,
    })
}

fn component_from_row(row: &Row<'_>) -> rusqlite::Result<ComponentRecord> {
    Ok(ComponentRecord {
        profile_id: row.get("profile_id")?,
        profile_revision: row.get("profile_revision")?,
        kind: row.get("kind")?,
        file: row.get("file")?,
        sha256: row.get("sha256")?,
        bytes: row.get("bytes")?,
        state: row.get("state")?,
        updated_at: row.get("updated_at")?,
    })
}

pub fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{:016x}", rand::random::<u64>())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    rightkit_fs::digest::sha256_bytes(bytes)
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    sha256_file_cancellable(path, || false)
}

/// Identity of a file for hash caching: any change to length or mtime invalidates the entry.
#[derive(Clone, PartialEq, Eq)]
struct FileFingerprint {
    len: u64,
    modified: std::time::SystemTime,
}

/// Process-wide cache of completed file hashes. Model files are multi-GB; re-hashing them on
/// every job start made unoptimised builds take minutes before any server spawned. Only fully
/// computed hashes are stored, so cancelled or failed reads are never cached.
fn hash_cache() -> &'static std::sync::Mutex<
    std::collections::HashMap<std::path::PathBuf, (FileFingerprint, String)>,
> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, (FileFingerprint, String)>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

pub fn sha256_file_cancellable(
    path: &Path,
    mut cancelled: impl FnMut() -> bool,
) -> Result<String, String> {
    if cancelled() {
        return Err("cancelled: component verification cancelled".into());
    }
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let fingerprint = file.metadata().ok().and_then(|meta| {
        Some(FileFingerprint {
            len: meta.len(),
            modified: meta.modified().ok()?,
        })
    });
    let cache_key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if let Some(fingerprint) = &fingerprint {
        if let Ok(cache) = hash_cache().lock() {
            if let Some((cached, hash)) = cache.get(&cache_key) {
                if cached == fingerprint {
                    return Ok(hash.clone());
                }
            }
        }
    }
    let mut hasher = Sha256Hasher::new();
    let mut buffer = [0u8; 1024 * 1024];
    let mut checked = std::time::Instant::now();
    loop {
        if checked.elapsed() >= std::time::Duration::from_millis(50) {
            if cancelled() {
                return Err("cancelled: component verification cancelled".into());
            }
            checked = std::time::Instant::now();
        }
        let read = file
            .read(&mut buffer)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let hash = hasher.finalize();
    if let Some(fingerprint) = fingerprint {
        if let Ok(mut cache) = hash_cache().lock() {
            cache.insert(cache_key, (fingerprint, hash.clone()));
        }
    }
    Ok(hash)
}
