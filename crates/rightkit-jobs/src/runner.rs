use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rightkit_fs::{FileLock, FsError};
use serde_json::{json, Value};

use crate::config::Ext;
use crate::error::{JobError, JobResult, HOST_HANDOFF};
use crate::sched::{Gate, Priority};
use crate::store::{
    Artifact, CancelToken, Event, Job, JobRecord, JobStore, Recovery, Submission, Transition,
    CANCELLED,
};

/// Attempts allowed before a job is failed with `max_attempts`.
pub const DEFAULT_MAX_ATTEMPTS: i64 = 3;

/// Who counts attempts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AttemptPolicy {
    /// Every run counts one attempt (0.1 behaviour); `max_attempts` bounds runs.
    #[default]
    PerRun,
    /// Runs are free (polling, resuming); the handler counts attempts through
    /// [`JobControl::begin_submission`], bounded by `max_attempts`.
    ByHandler,
}

/// Receives every stored progress event (e.g. stderr NDJSON or a stdio event frame).
pub type EventSink = Arc<dyn Fn(&Event) + Send + Sync>;

/// Exclusive per-job lock; released on drop or process death.
///
/// Backed by `rightkit_fs::FileLock`: a POSIX record lock on unix (process-owned, so a
/// child forked by another thread cannot keep it alive) and `LockFileEx` elsewhere, with an
/// in-process registry so a process never holds two descriptors on one lock file.
pub struct JobLock {
    _lock: FileLock,
}

fn lock_error(error: FsError) -> JobError {
    match error {
        FsError::Io { path, source } => JobError::from(std::io::Error::new(
            source.kind(),
            format!("{}: {source}", path.display()),
        )),
        other => JobError::from(std::io::Error::other(other.to_string())),
    }
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

    /// `Ok(None)` when another holder has the lock, in this process or another.
    pub fn try_lock(&self, job_id: &str) -> JobResult<Option<JobLock>> {
        let path = self.path(job_id)?;
        std::fs::create_dir_all(&self.dir)?;
        match FileLock::try_acquire(&path) {
            Ok(lock) => Ok(Some(JobLock { _lock: lock })),
            Err(FsError::Locked { .. }) => Ok(None),
            Err(error) => Err(lock_error(error)),
        }
    }
}

/// What a job does. The only product-specific part of an engine.
pub trait JobHandler: Send + Sync {
    /// Run one attempt. Return the result value; report progress and register
    /// artifacts through `ctl`; poll `ctl.is_cancelled()` between steps.
    /// A `retryable` error pauses the job (still recoverable); any other error fails it.
    fn run(&self, job: &Job, ctl: &JobControl) -> JobResult<Value>;

    /// What [`Runner`] calls: the same attempt with the shared control handle, for handlers
    /// that hand the control to another thread. The default calls [`JobHandler::run`], so
    /// existing handlers are unchanged.
    fn run_shared(&self, job: &Job, ctl: &Arc<JobControl>) -> JobResult<Value> {
        self.run(job, ctl)
    }
}

/// Handler-facing view of one running job.
pub struct JobControl {
    store: Arc<JobStore>,
    job_id: String,
    sink: Option<EventSink>,
    gate: Arc<Gate>,
    priority: Priority,
    max_attempts: i64,
    cancel: CancelToken,
    /// Attempt generation this control writes under (see [`JobStore::fence`]).
    /// Every write is rejected with `stale_attempt` once a newer one exists.
    generation: i64,
    /// Set by the stall watchdog: this attempt was abandoned and must stop.
    abandoned: AtomicBool,
    started: Instant,
    /// Milliseconds since `started` at the last sign of life.
    beat_ms: AtomicU64,
}

impl JobControl {
    pub fn job_id(&self) -> &str {
        &self.job_id
    }
    pub fn priority(&self) -> Priority {
        self.priority
    }
    pub fn store(&self) -> &Arc<JobStore> {
        &self.store
    }
    pub fn max_attempts(&self) -> i64 {
        self.max_attempts
    }
    /// The attempt generation this control writes under.
    pub fn generation(&self) -> i64 {
        self.generation
    }
    /// Current job row with extension columns.
    pub fn record(&self) -> JobResult<JobRecord> {
        self.store.record(&self.job_id)
    }
    pub fn ext(&self) -> JobResult<Ext> {
        self.store.ext(&self.job_id)
    }
    pub fn set_ext(&self, ext: &Ext) -> JobResult<JobRecord> {
        self.store
            .set_ext_fenced(&self.job_id, self.generation, ext)
    }
    /// Validated status move (see [`JobStore::transition`]). Fenced: a stale
    /// attempt gets `stale_attempt` and writes nothing.
    pub fn transition(&self, t: &Transition) -> JobResult<Option<JobRecord>> {
        self.store
            .transition_fenced(&self.job_id, self.generation, t)
    }
    /// See [`JobStore::begin_submission`]; bounded by the runner's `max_attempts`.
    /// `Ok(None)` when the job went terminal.
    pub fn begin_submission(&self) -> JobResult<Option<Submission>> {
        self.heartbeat();
        self.store
            .begin_submission_fenced(&self.job_id, self.generation, self.max_attempts)
    }
    pub fn accept_submission(
        &self,
        external_id: &str,
        to: Option<&str>,
    ) -> JobResult<Option<JobRecord>> {
        self.heartbeat();
        self.store
            .accept_submission_fenced(&self.job_id, self.generation, external_id, to)
    }
    pub fn fail_submission(&self, e: &JobError, to: Option<&str>) -> JobResult<Option<JobRecord>> {
        self.heartbeat();
        self.store
            .fail_submission_fenced(&self.job_id, self.generation, e, to)
    }
    /// Store and publish a progress event. Best-effort: never fails the job.
    /// Counts as a sign of life for the stall watchdog. Written under the store's
    /// [`crate::ProgressPolicy`]: with coalescing, a tick inside the interval is held back and
    /// reaches neither the database nor the sink until it is flushed (see
    /// [`JobStore::progress_fenced`]); with the default policy every tick is written at once.
    pub fn progress(&self, stage: &str, progress: Option<f64>, message: Option<&str>) {
        self.heartbeat();
        if let Ok(Some(ev)) =
            self.store
                .progress_fenced(&self.job_id, self.generation, stage, progress, message)
        {
            if let Some(s) = &self.sink {
                s(&ev);
            }
        }
    }
    /// A lifecycle event (`started`, `complete`, `preempted`, ...): always written at once and
    /// fully durable, whatever the progress policy.
    fn lifecycle(&self, stage: &str, progress: Option<f64>, message: Option<&str>) {
        self.heartbeat();
        if let Ok(ev) =
            self.store
                .add_event_fenced(&self.job_id, self.generation, stage, progress, message)
        {
            if let Some(s) = &self.sink {
                s(&ev);
            }
        }
    }
    /// Tell the stall watchdog the handler is alive in the middle of a long step.
    pub fn heartbeat(&self) {
        self.beat_ms
            .store(self.started.elapsed().as_millis() as u64, Ordering::SeqCst);
    }
    fn idle(&self) -> Duration {
        self.started
            .elapsed()
            .saturating_sub(Duration::from_millis(self.beat_ms.load(Ordering::SeqCst)))
    }
    /// The in-memory cancel flag: one atomic load, set at once by a cancel in this process
    /// ([`JobStore::cancel`], a single-flight supersede). It does not see a cancel made by
    /// another process; [`JobControl::is_cancelled`] and [`JobControl::step_boundary`] read the
    /// database for that and set the flag when they find the job cancelled. The database status
    /// is the source of truth; the token is its fast path.
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }
    /// True when the job is cancelled. Checks the in-memory flag first, then the database
    /// (cross-process cancel); a cancel found there sets the flag.
    pub fn is_cancelled(&self) -> bool {
        if self.cancel.is_cancelled() {
            return true;
        }
        let cancelled = self.store.is_cancelled(&self.job_id).unwrap_or(false);
        if cancelled {
            self.cancel.set();
        }
        cancelled
    }
    /// Why the handler must stop now, if it must: cancelled, abandoned after a
    /// stall, or the host is handing off. Cheap enough to poll inside a step.
    pub fn abort_reason(&self) -> Option<JobError> {
        if self.abandoned.load(Ordering::SeqCst) {
            return Some(JobError::stalled(self.idle().as_secs_f64()));
        }
        if self.is_cancelled() {
            return Some(JobError::cancelled());
        }
        if self.gate.handoff_requested() {
            return Some(JobError::handoff());
        }
        None
    }
    /// Call between steps (e.g. once per audio window). Returns `Err` when the
    /// job was cancelled, abandoned, or the host is handing off (return it from
    /// the handler as-is). Batch jobs block here, emitting `preempted` and
    /// `resumed` events, while interactive work is active; interactive jobs
    /// never wait.
    pub fn step_boundary(&self) -> JobResult<()> {
        self.heartbeat();
        if let Some(e) = self.abort_reason() {
            return Err(e);
        }
        if self.priority == Priority::Batch && self.gate.interactive_active() > 0 {
            self.lifecycle("preempted", None, Some("paused for interactive work"));
            while self.gate.interactive_active() > 0 {
                self.gate.wait_idle(Duration::from_millis(50));
                self.heartbeat();
                if let Some(e) = self.abort_reason() {
                    return Err(e);
                }
            }
            self.lifecycle("resumed", None, None);
        }
        Ok(())
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
        if self.abandoned.load(Ordering::SeqCst) {
            return Err(JobError::stalled(self.idle().as_secs_f64()));
        }
        self.store
            .add_artifact_fenced(&self.job_id, self.generation, path, mime)
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
    pub fn job(&self) -> Option<&Job> {
        match self {
            RunOutcome::Finished(j) | RunOutcome::Paused(j) => Some(j),
            RunOutcome::LockedElsewhere(_) => None,
        }
    }
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
    gate: Arc<Gate>,
    stall_timeout: Option<Duration>,
    attempts: AttemptPolicy,
    implicit_events: bool,
}

impl Runner {
    pub fn new(store: Arc<JobStore>, locks: JobLocks, handler: Arc<dyn JobHandler>) -> Self {
        Self {
            store,
            locks,
            handler,
            sink: None,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            gate: Gate::new(),
            stall_timeout: None,
            attempts: AttemptPolicy::PerRun,
            implicit_events: true,
        }
    }
    pub fn with_sink(mut self, sink: EventSink) -> Self {
        self.sink = Some(sink);
        self
    }
    pub fn with_max_attempts(mut self, n: i64) -> Self {
        self.max_attempts = n.max(1);
        self
    }
    /// Share a [`Gate`] with the service handler so interactive work, batch work
    /// and host handoff coordinate.
    pub fn with_gate(mut self, gate: Arc<Gate>) -> Self {
        self.gate = gate;
        self
    }
    /// Pause a run that shows no sign of life (no `progress`, `step_boundary` or
    /// `heartbeat`) for `d`. There is deliberately no total-duration limit: a
    /// long file that keeps making progress is never cut off.
    pub fn with_stall_timeout(mut self, d: Duration) -> Self {
        self.stall_timeout = Some(d);
        self
    }
    pub fn with_attempt_policy(mut self, p: AttemptPolicy) -> Self {
        self.attempts = p;
        self
    }
    /// Whether the runner itself emits `started`, `complete`, `paused`,
    /// `failed` and `cancelled` events (default `true`). With `false` the
    /// handler owns the event stream (via [`JobControl::progress`]); status
    /// changes, attempts, locks and outcomes are unchanged.
    pub fn with_implicit_events(mut self, on: bool) -> Self {
        self.implicit_events = on;
        self
    }
    pub fn implicit_events(&self) -> bool {
        self.implicit_events
    }
    fn implicit(&self, ctl: &JobControl, stage: &str, progress: Option<f64>, msg: Option<&str>) {
        if self.implicit_events {
            ctl.lifecycle(stage, progress, msg);
        }
    }
    pub fn attempt_policy(&self) -> AttemptPolicy {
        self.attempts
    }
    pub fn max_attempts(&self) -> i64 {
        self.max_attempts
    }
    pub fn gate(&self) -> &Arc<Gate> {
        &self.gate
    }
    pub fn store(&self) -> &Arc<JobStore> {
        &self.store
    }
    pub fn locks(&self) -> &JobLocks {
        &self.locks
    }

    fn control(
        &self,
        job_id: &str,
        sink: Option<EventSink>,
        priority: Priority,
        generation: i64,
    ) -> JobControl {
        let cancel = self.store.cancel_token(job_id);
        JobControl {
            cancel,
            store: self.store.clone(),
            job_id: job_id.into(),
            sink,
            gate: self.gate.clone(),
            priority,
            max_attempts: self.max_attempts,
            generation,
            abandoned: AtomicBool::new(false),
            started: Instant::now(),
            beat_ms: AtomicU64::new(0),
        }
    }

    /// Queued/running jobs with no live runner that may be run again. Runs no handler. One
    /// side effect: a `Recovery::Never` job that began an attempt and lost its runner is not
    /// listed; it is failed with the `interrupted` error here (and published to the sink).
    pub fn recoverable_ids(&self) -> JobResult<Vec<String>> {
        Ok(self
            .scan_recoverable()?
            .into_iter()
            .filter_map(|s| match s {
                Scanned::Resume(id) => Some(id),
                Scanned::Interrupted(_) => None,
            })
            .collect())
    }

    /// Oldest first: jobs to run again, and `Never` jobs just failed as interrupted.
    fn scan_recoverable(&self) -> JobResult<Vec<Scanned>> {
        let mut out = Vec::new();
        for id in self.store.non_terminal_ids()? {
            if let Some(s) = self.scan_one(id)? {
                out.push(s);
            }
        }
        Ok(out)
    }

    /// One job's recovery decision. `None`: held, or another runner has it.
    fn scan_one(&self, id: String) -> JobResult<Option<Scanned>> {
        // A held job leaves only through `resolve`; the runner never lists it.
        if self.is_held(&self.store.get(&id)?.status) {
            return Ok(None);
        }
        let Some(_lock) = self.locks.try_lock(&id)? else {
            return Ok(None);
        };
        let (recovery, gen) = self.store.run_class(&id)?;
        if recovery == Recovery::Never && gen > 0 {
            Ok(self
                .interrupt(&id)?
                .map(|job| Scanned::Interrupted(Box::new(job))))
        } else {
            Ok(Some(Scanned::Resume(id)))
        }
    }

    fn is_held(&self, status: &str) -> bool {
        self.store.config().status_model().is_held(status)
    }

    /// Fail a started `Never` job (its lock is held by the caller) and publish the event.
    fn interrupt(&self, job_id: &str) -> JobResult<Option<Job>> {
        let Some((job, ev)) = self.store.interrupt(job_id)? else {
            return Ok(None);
        };
        if let Some(s) = &self.sink {
            s(&ev);
        }
        Ok(Some(job))
    }

    /// Explicitly run (or resume) one job. A terminal job is returned as-is. A `Never` job
    /// that already began an attempt is failed as interrupted instead of run (see [`Recovery`]).
    pub fn run(&self, job_id: &str) -> JobResult<RunOutcome> {
        self.run_with(job_id, self.sink.clone())
    }

    /// Like [`Runner::run`], publishing this run's events to `sink` (per-request streaming).
    pub fn run_with(&self, job_id: &str, sink: Option<EventSink>) -> JobResult<RunOutcome> {
        let Some(_lock) = self.locks.try_lock(job_id)? else {
            return Ok(RunOutcome::LockedElsewhere(job_id.into()));
        };
        let _live = LiveToken(&self.store, job_id);
        let job = self.store.get(job_id)?;
        if job.is_terminal() {
            return Ok(RunOutcome::Finished(job));
        }
        // Held: waiting for an operator's `resolve`. Nothing runs, nothing is counted.
        if self.is_held(&job.status) {
            return Ok(RunOutcome::Paused(job));
        }
        let (recovery, started) = self.store.run_class(job_id)?;
        let never = recovery == Recovery::Never;
        if never && started > 0 {
            self.interrupt(job_id)?;
            return Ok(RunOutcome::Finished(self.store.get(job_id)?));
        }
        let per_run = self.attempts == AttemptPolicy::PerRun;
        if per_run && job.attempts >= self.max_attempts {
            let e = JobError::new("max_attempts", "job used every allowed attempt");
            let gen = self.store.generation(job_id)?;
            self.store.fail_fenced(job_id, gen, &e)?;
            self.implicit(
                &self.control(job_id, sink, Priority::of(&job), gen),
                "failed",
                None,
                Some(&e.message),
            );
            return Ok(RunOutcome::Finished(self.store.get(job_id)?));
        }
        let (job, gen) = match self.store.begin_run_fenced(job_id, per_run) {
            Ok(Some((r, gen))) => (r.job, gen),
            Ok(None) => return Ok(RunOutcome::Finished(self.store.get(job_id)?)),
            // A store hook (e.g. a spend cap) refused the attempt; nothing was counted.
            Err(e) if e.code == "db" => return Err(e),
            Err(e) => {
                let gen = self.store.generation(job_id)?;
                let ctl = self.control(job_id, sink, Priority::of(&job), gen);
                if e.retryable {
                    self.store.record_error_fenced(job_id, gen, &e)?;
                    self.implicit(
                        &ctl,
                        "paused",
                        None,
                        Some(&format!("paused, will resume: {}", e.message)),
                    );
                    return Ok(RunOutcome::Paused(self.store.get(job_id)?));
                }
                if self.store.fail_fenced(job_id, gen, &e)? {
                    self.implicit(&ctl, "failed", None, Some(&e.message));
                }
                return Ok(RunOutcome::Finished(self.store.get(job_id)?));
            }
        };
        let priority = Priority::of(&job);
        // Interactive work holds the gate for the whole run so batch jobs yield at their boundaries.
        let _interactive =
            (priority == Priority::Interactive).then(|| self.gate.begin_interactive());
        let ctl = Arc::new(self.control(job_id, sink.clone(), priority, gen));
        // A cancel that landed before the flag existed is still in the database.
        if self.store.is_cancelled(job_id).unwrap_or(false) {
            ctl.cancel.set();
        }
        self.implicit(&ctl, "started", None, None);
        let result = self.run_handler(&job, &ctl);
        // Generation this runner writes under. A stalled attempt is fenced
        // before the lock is released: its worker thread may still be running,
        // but every write it attempts now fails with `stale_attempt`.
        let (owner, ctl) = if ctl.abandoned.load(Ordering::SeqCst) {
            let owner = self.store.fence(job_id, gen)?;
            (owner, Arc::new(self.control(job_id, sink, priority, owner)))
        } else {
            (gen, ctl)
        };
        match result {
            Ok(value) => {
                if self.store.complete_fenced(job_id, owner, &value)? {
                    self.implicit(&ctl, "complete", Some(1.0), None);
                } else if self.store.get(job_id)?.status == CANCELLED {
                    self.implicit(&ctl, "cancelled", None, None);
                }
                Ok(RunOutcome::Finished(self.store.get(job_id)?))
            }
            // The handler began; it may already have had effects, so a `Never` job cannot pause
            // for a later re-run.
            Err(e) if e.retryable && never => {
                let e = JobError::interrupted_by(job_id, &e);
                if self.store.fail_fenced(job_id, owner, &e)? {
                    self.implicit(&ctl, "failed", None, Some(&e.message));
                }
                Ok(RunOutcome::Finished(self.store.get(job_id)?))
            }
            Err(e) if e.retryable => {
                if e.code == HOST_HANDOFF && per_run {
                    self.store.refund_attempt_fenced(job_id, owner)?;
                }
                self.store.record_error_fenced(job_id, owner, &e)?;
                self.implicit(
                    &ctl,
                    "paused",
                    None,
                    Some(&format!("paused, will resume: {}", e.message)),
                );
                Ok(RunOutcome::Paused(self.store.get(job_id)?))
            }
            Err(e) => {
                if self.store.fail_fenced(job_id, owner, &e)? {
                    self.implicit(&ctl, "failed", None, Some(&e.message));
                } else if self.store.get(job_id)?.status == CANCELLED {
                    self.implicit(&ctl, "cancelled", None, None);
                }
                Ok(RunOutcome::Finished(self.store.get(job_id)?))
            }
        }
    }

    /// Run the handler inline, or on a watched worker thread when a stall
    /// timeout is set. A stalled attempt is abandoned: its thread is told to
    /// stop (`abort_reason`), its result is discarded, and the job pauses.
    fn run_handler(&self, job: &Job, ctl: &Arc<JobControl>) -> JobResult<Value> {
        let Some(limit) = self.stall_timeout else {
            return catch_unwind(AssertUnwindSafe(|| self.handler.run_shared(job, ctl)))
                .unwrap_or_else(|_| Err(JobError::new("panic", "job handler panicked")));
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let (handler, job2, ctl2) = (self.handler.clone(), job.clone(), ctl.clone());
        std::thread::Builder::new()
            .name(format!("rk-job-{}", job.job_id))
            .spawn(move || {
                let r = catch_unwind(AssertUnwindSafe(|| handler.run_shared(&job2, &ctl2)))
                    .unwrap_or_else(|_| Err(JobError::new("panic", "job handler panicked")));
                let _ = tx.send(r);
            })
            .map_err(JobError::from)?;
        let poll = (limit / 4).clamp(Duration::from_millis(10), Duration::from_millis(250));
        loop {
            match rx.recv_timeout(poll) {
                Ok(r) => return r,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if ctl.idle() > limit {
                        ctl.abandoned.store(true, Ordering::SeqCst);
                        return Err(JobError::stalled(ctl.idle().as_secs_f64()));
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(JobError::new(
                        "panic",
                        "job handler thread ended without a result",
                    ))
                }
            }
        }
    }

    /// Remove per-job scratch directories (`<root>/<job_id>`) that no
    /// queued/running job owns, e.g. left by a crash. Returns removed names.
    /// Pass `root` only when this process is the sole engine for it.
    pub fn sweep_orphan_scratch(&self, root: &Path) -> JobResult<Vec<String>> {
        let live: std::collections::HashSet<String> =
            self.store.non_terminal_ids()?.into_iter().collect();
        let mut removed = Vec::new();
        let Ok(rd) = std::fs::read_dir(root) else {
            return Ok(removed);
        };
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.path().is_dir() && self.store.get(&name).is_ok() && !live.contains(&name) {
                std::fs::remove_dir_all(entry.path())?;
                removed.push(name);
            }
        }
        Ok(removed)
    }

    /// Remove child directories of `root` that contain no path claimed by a non-terminal job's
    /// artifacts (see [`JobStore::claimed_paths`]). Plain files and symlinks under `root` are
    /// left alone, and a symlink is never followed out of `root`. Returns the removed
    /// directories. Pass `root` only when this process is the sole engine for it.
    pub fn sweep_unclaimed(&self, root: &Path) -> JobResult<Vec<PathBuf>> {
        let mut claimed: Vec<PathBuf> = Vec::new();
        for p in self.store.claimed_paths()? {
            if let Some(c) = canonical_or_none(&p) {
                claimed.push(c);
            }
            claimed.push(p);
        }
        let mut removed = Vec::new();
        let Ok(rd) = std::fs::read_dir(root) else {
            return Ok(removed);
        };
        for entry in rd.flatten() {
            let path = entry.path();
            // `file_type` does not follow symlinks: a link to a directory is not a directory.
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if !is_dir {
                continue;
            }
            let canon = std::fs::canonicalize(&path).ok();
            let owned = claimed
                .iter()
                .any(|c| c.starts_with(&path) || canon.as_ref().is_some_and(|d| c.starts_with(d)));
            if owned {
                continue;
            }
            std::fs::remove_dir_all(&path)?;
            removed.push(path);
        }
        Ok(removed)
    }

    /// Like [`Runner::resume_recoverable`], but one failing job never stops the rest: every
    /// recoverable job is attempted, oldest first, and its id is returned with its own result.
    /// A job that cannot even be examined is reported by id too. `Err` only when the job list
    /// itself cannot be read.
    pub fn resume_recoverable_each(&self) -> JobResult<Vec<(String, JobResult<RunOutcome>)>> {
        let mut out = Vec::new();
        for id in self.store.non_terminal_ids()? {
            match self.scan_one(id.clone()) {
                Ok(None) => {}
                Ok(Some(Scanned::Resume(id))) => {
                    let r = self.run(&id);
                    out.push((id, r));
                }
                Ok(Some(Scanned::Interrupted(job))) => {
                    out.push((job.job_id.clone(), Ok(RunOutcome::Finished(*job))));
                }
                Err(e) => out.push((id, Err(e))),
            }
        }
        Ok(out)
    }

    /// Explicitly resume every recoverable job, oldest first. Never called by
    /// passive reads.
    pub fn resume_recoverable(&self) -> JobResult<Vec<RunOutcome>> {
        let mut out = Vec::new();
        for scanned in self.scan_recoverable()? {
            out.push(match scanned {
                Scanned::Resume(id) => self.run(&id)?,
                Scanned::Interrupted(job) => RunOutcome::Finished(*job),
            });
        }
        Ok(out)
    }
}

/// Canonical form of `p`, or of its nearest existing parent joined with the rest.
fn canonical_or_none(p: &Path) -> Option<PathBuf> {
    if let Ok(c) = std::fs::canonicalize(p) {
        return Some(c);
    }
    let parent = std::fs::canonicalize(p.parent()?).ok()?;
    Some(parent.join(p.file_name()?))
}

enum Scanned {
    Resume(String),
    Interrupted(Box<Job>),
}

/// Drops the job's cancel flag from the store registry when its run ends, however it ends.
struct LiveToken<'a>(&'a JobStore, &'a str);

impl Drop for LiveToken<'_> {
    fn drop(&mut self) {
        self.0.release_cancel_token(self.1);
    }
}
