use std::fs::{File, OpenOptions};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};

use crate::error::{JobError, JobResult};
use crate::store::{Artifact, Event, Job, JobStore, CANCELLED};

/// Attempts allowed before a job is failed with `max_attempts`.
pub const DEFAULT_MAX_ATTEMPTS: i64 = 3;

/// Receives every stored progress event (e.g. stderr NDJSON or a stdio event frame).
pub type EventSink = Arc<dyn Fn(&Event) + Send + Sync>;

/// Exclusive per-job lock; released on drop or process death.
pub struct JobLock {
    _file: File,
}

/// Directory of per-job lock files. A non-terminal job whose lock is free has
/// no live runner, so it is recoverable.
#[derive(Clone)]
pub struct JobLocks {
    dir: PathBuf,
}

impl JobLocks {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path(&self, job_id: &str) -> JobResult<PathBuf> {
        if job_id.is_empty() || job_id.contains(['/', '\\', '.']) {
            return Err(JobError::bad_input("bad job_id"));
        }
        Ok(self.dir.join(format!("{job_id}.lock")))
    }

    /// `Ok(None)` when another holder has the lock.
    pub fn try_lock(&self, job_id: &str) -> JobResult<Option<JobLock>> {
        std::fs::create_dir_all(&self.dir)?;
        let f = OpenOptions::new().create(true).truncate(false).write(true).open(self.path(job_id)?)?;
        match f.try_lock() {
            Ok(()) => Ok(Some(JobLock { _file: f })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
        }
    }
}

/// What a job does. The only product-specific part of an engine.
pub trait JobHandler: Send + Sync {
    /// Run one attempt. Return the result value; report progress and register
    /// artifacts through `ctl`; poll `ctl.is_cancelled()` between steps.
    /// A `retryable` error pauses the job (still recoverable); any other error fails it.
    fn run(&self, job: &Job, ctl: &JobControl) -> JobResult<Value>;
}

/// Handler-facing view of one running job.
pub struct JobControl {
    store: Arc<JobStore>,
    job_id: String,
    sink: Option<EventSink>,
}

impl JobControl {
    pub fn job_id(&self) -> &str {
        &self.job_id
    }
    /// Store and publish a progress event. Best-effort: never fails the job.
    pub fn progress(&self, stage: &str, progress: Option<f64>, message: Option<&str>) {
        if let Ok(ev) = self.store.add_event(&self.job_id, stage, progress, message) {
            if let Some(s) = &self.sink {
                s(&ev);
            }
        }
    }
    pub fn is_cancelled(&self) -> bool {
        self.store.is_cancelled(&self.job_id).unwrap_or(false)
    }
    /// Artifacts recorded by earlier attempts whose file still matches its
    /// recorded size and sha256, so a resumed attempt can skip finished stages.
    pub fn verified_artifacts(&self) -> JobResult<Vec<Artifact>> {
        let mut ok = Vec::new();
        for a in self.store.artifacts(&self.job_id)? {
            if self.store.verify_artifact(&a)? {
                ok.push(a);
            }
        }
        Ok(ok)
    }
    pub fn add_artifact(&self, path: &Path, mime: &str) -> JobResult<Artifact> {
        self.store.add_artifact(&self.job_id, path, mime)
    }
}

#[derive(Debug, Clone)]
pub enum RunOutcome {
    /// Reached a terminal state (complete, failed, or cancelled).
    Finished(Job),
    /// Another runner holds the job.
    LockedElsewhere(String),
    /// A retryable error paused the job; it stays recoverable.
    Paused(Job),
}

impl RunOutcome {
    pub fn to_value(&self) -> Value {
        match self {
            RunOutcome::Finished(j) => j.to_value(),
            RunOutcome::LockedElsewhere(id) => json!({"job_id": id, "status": "locked_elsewhere"}),
            RunOutcome::Paused(j) => {
                let mut v = j.to_value();
                v["recoverable"] = json!(true);
                v
            }
        }
    }
}

pub struct Runner {
    store: Arc<JobStore>,
    locks: JobLocks,
    handler: Arc<dyn JobHandler>,
    sink: Option<EventSink>,
    max_attempts: i64,
}

impl Runner {
    pub fn new(store: Arc<JobStore>, locks: JobLocks, handler: Arc<dyn JobHandler>) -> Self {
        Self { store, locks, handler, sink: None, max_attempts: DEFAULT_MAX_ATTEMPTS }
    }
    pub fn with_sink(mut self, sink: EventSink) -> Self {
        self.sink = Some(sink);
        self
    }
    pub fn with_max_attempts(mut self, n: i64) -> Self {
        self.max_attempts = n.max(1);
        self
    }
    pub fn store(&self) -> &Arc<JobStore> {
        &self.store
    }
    pub fn locks(&self) -> &JobLocks {
        &self.locks
    }

    fn control(&self, job_id: &str, sink: Option<EventSink>) -> JobControl {
        JobControl { store: self.store.clone(), job_id: job_id.into(), sink }
    }

    /// Queued/running jobs with no live runner. Passive: does not run anything.
    pub fn recoverable_ids(&self) -> JobResult<Vec<String>> {
        let mut out = Vec::new();
        for id in self.store.non_terminal_ids()? {
            if self.locks.try_lock(&id)?.is_some() {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// Explicitly run (or resume) one job. A terminal job is returned as-is.
    pub fn run(&self, job_id: &str) -> JobResult<RunOutcome> {
        self.run_with(job_id, self.sink.clone())
    }

    /// Like [`Runner::run`], publishing this run's events to `sink` (per-request streaming).
    pub fn run_with(&self, job_id: &str, sink: Option<EventSink>) -> JobResult<RunOutcome> {
        let Some(_lock) = self.locks.try_lock(job_id)? else {
            return Ok(RunOutcome::LockedElsewhere(job_id.into()));
        };
        let job = self.store.get(job_id)?;
        if job.is_terminal() {
            return Ok(RunOutcome::Finished(job));
        }
        if job.attempts >= self.max_attempts {
            let e = JobError::new("max_attempts", "job used every allowed attempt");
            self.store.fail(job_id, &e)?;
            self.control(job_id, sink).progress("failed", None, Some(&e.message));
            return Ok(RunOutcome::Finished(self.store.get(job_id)?));
        }
        let Some(job) = self.store.begin_attempt(job_id)? else {
            return Ok(RunOutcome::Finished(self.store.get(job_id)?));
        };
        let ctl = self.control(job_id, sink);
        ctl.progress("started", None, None);
        let result = catch_unwind(AssertUnwindSafe(|| self.handler.run(&job, &ctl)))
            .unwrap_or_else(|_| Err(JobError::new("panic", "job handler panicked")));
        match result {
            Ok(value) => {
                if self.store.complete(job_id, &value)? {
                    ctl.progress("complete", Some(1.0), None);
                } else if self.store.get(job_id)?.status == CANCELLED {
                    ctl.progress("cancelled", None, None);
                }
                Ok(RunOutcome::Finished(self.store.get(job_id)?))
            }
            Err(e) if e.retryable => {
                self.store.record_error(job_id, &e)?;
                ctl.progress("paused", None, Some(&format!("paused, will resume: {}", e.message)));
                Ok(RunOutcome::Paused(self.store.get(job_id)?))
            }
            Err(e) => {
                if self.store.fail(job_id, &e)? {
                    ctl.progress("failed", None, Some(&e.message));
                }
                Ok(RunOutcome::Finished(self.store.get(job_id)?))
            }
        }
    }

    /// Remove per-job scratch directories (`<root>/<job_id>`) that no
    /// queued/running job owns, e.g. left by a crash. Returns removed names.
    /// Pass `root` only when this process is the sole engine for it.
    pub fn sweep_orphan_scratch(&self, root: &Path) -> JobResult<Vec<String>> {
        let live: std::collections::HashSet<String> = self.store.non_terminal_ids()?.into_iter().collect();
        let mut removed = Vec::new();
        let Ok(rd) = std::fs::read_dir(root) else { return Ok(removed) };
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.path().is_dir() && self.store.get(&name).is_ok() && !live.contains(&name) {
                std::fs::remove_dir_all(entry.path())?;
                removed.push(name);
            }
        }
        Ok(removed)
    }

    /// Explicitly resume every recoverable job, oldest first. Never called by
    /// passive reads.
    pub fn resume_recoverable(&self) -> JobResult<Vec<RunOutcome>> {
        let mut out = Vec::new();
        for id in self.recoverable_ids()? {
            out.push(self.run(&id)?);
        }
        Ok(out)
    }
}
