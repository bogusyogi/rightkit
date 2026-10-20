use std::collections::{HashMap, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rightkit_fs::{FileLock, FsError, LockOptions};
use serde_json::{json, Value};

use crate::config::Ext;
use crate::error::{JobError, JobResult, HOST_HANDOFF};
use crate::lock_queue::{self, Joined, TicketSlot};
use crate::sched::{Gate, Priority};
use crate::store::{
    is_storage_error, now, projected, Artifact, CancelToken, ClaimScope, Event, FlushReport, Job,
    JobRecord, JobStore, LifecycleEvent, Recovery, Settle, SettleRequest, Settlement,
    SettlementShutdown, Submission, Transition, UnsettledJob, CANCELLED, COMPLETE, FAILED,
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

impl JobLock {
    /// Release the lock and delete its `<id>.lock` file. The runner calls this once the job's
    /// terminal state is stored, so finished jobs leave no lock files behind. Only for a job
    /// that will not run again: a runner that opened the old file just before it was removed
    /// could lock a file nobody else sees (see [`JobLocks::sweep`]). Removal is best effort; a
    /// file that cannot be removed is left for [`JobLocks::sweep`].
    pub(crate) fn release_removing(self) {
        let path = self._lock.path().to_path_buf();
        // Unix: unlink while the lock is still held, so no other process locks the old file
        // between the release and the removal. Elsewhere an open lock file cannot be removed:
        // release first, and a file another process has opened simply stays.
        #[cfg(unix)]
        {
            let _ = std::fs::remove_file(&path);
            drop(self);
        }
        #[cfg(not(unix))]
        {
            drop(self);
            let _ = std::fs::remove_file(&path);
        }
    }
}

pub(crate) fn lock_error(error: FsError) -> JobError {
    match error {
        FsError::Io { path, source } => JobError::from(std::io::Error::new(
            source.kind(),
            format!("{}: {source}", path.display()),
        )),
        other => JobError::from(std::io::Error::other(other.to_string())),
    }
}

/// How a wait for a job lock ended ([`JobLocks::acquire_cancellable`]).
#[non_exhaustive]
pub enum LockWait {
    Acquired(JobLock),
    TimedOut,
    Cancelled,
}

/// How far [`JobLocks`] waiters are served in arrival order ([`JobLocks::with_fifo_scope`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum FifoScope {
    /// Waiters in this process (clones of one `JobLocks`) are served in arrival order; another
    /// process, or a `JobLocks` built separately, competes for the file lock unordered. The
    /// default.
    #[default]
    Process,
    /// Waiters are served in arrival order across processes and across separate `JobLocks`
    /// values on the same directory, through ticket files under `<dir>/<job id>.queue/`. Every
    /// process that waits on the same lock directory must use this scope: a waiter built with
    /// [`FifoScope::Process`], and [`JobLocks::try_lock`], join no line and can take the lock out
    /// of turn.
    CrossProcess,
}

const LOCK_WAIT_SLICE: Duration = Duration::from_millis(100);
const LOCK_QUEUE_PAUSE: Duration = Duration::from_millis(10);
/// Each poll of a cross-process line reads its directory, so a waiter behind the head polls less.
const LOCK_TICKET_PAUSE: Duration = Duration::from_millis(25);

/// One waiter's place in the same-process FIFO for one job id; leaving on drop.
struct WaitSlot {
    waiters: Arc<Mutex<HashMap<String, VecDeque<u64>>>>,
    job_id: String,
    ticket: u64,
}

impl WaitSlot {
    fn join(
        waiters: &Arc<Mutex<HashMap<String, VecDeque<u64>>>>,
        job_id: &str,
        ticket: u64,
    ) -> Self {
        waiters
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(job_id.to_string())
            .or_default()
            .push_back(ticket);
        Self {
            waiters: Arc::clone(waiters),
            job_id: job_id.to_string(),
            ticket,
        }
    }
    fn is_head(&self) -> bool {
        self.waiters
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&self.job_id)
            .and_then(|q| q.front())
            == Some(&self.ticket)
    }
}

impl Drop for WaitSlot {
    fn drop(&mut self) {
        let mut map = self.waiters.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(q) = map.get_mut(&self.job_id) {
            q.retain(|t| *t != self.ticket);
            if q.is_empty() {
                map.remove(&self.job_id);
            }
        }
    }
}

/// A waiter's place in the line its [`FifoScope`] selects.
enum Turn {
    Process(WaitSlot),
    Ticket(TicketSlot),
}

impl Turn {
    fn is_head(&self) -> JobResult<bool> {
        match self {
            Self::Process(slot) => Ok(slot.is_head()),
            Self::Ticket(slot) => slot.is_head(),
        }
    }
}

/// Directory of per-job lock files. A non-terminal job whose lock is free has
/// no live runner, so it is recoverable.
#[derive(Clone)]
pub struct JobLocks {
    dir: PathBuf,
    waiters: Arc<Mutex<HashMap<String, VecDeque<u64>>>>,
    next_ticket: Arc<AtomicU64>,
    scope: FifoScope,
}

impl JobLocks {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            waiters: Arc::default(),
            next_ticket: Arc::new(AtomicU64::new(1)),
            scope: FifoScope::Process,
        }
    }

    /// Sets how far waiters are served in arrival order (default [`FifoScope::Process`]).
    /// Clones made afterwards keep the scope.
    pub fn with_fifo_scope(mut self, scope: FifoScope) -> Self {
        self.scope = scope;
        self
    }

    /// The arrival-order scope of [`JobLocks::acquire`] and [`JobLocks::acquire_cancellable`].
    pub fn fifo_scope(&self) -> FifoScope {
        self.scope
    }

    /// `<dir>/<job_id>.queue`: where [`FifoScope::CrossProcess`] waiters keep their tickets.
    fn queue_dir(&self, job_id: &str) -> PathBuf {
        self.dir.join(format!("{job_id}.queue"))
    }

    /// Lock-file directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Path of `job_id`'s lock file (`<dir>/<job_id>.lock`); may not exist yet.
    pub fn lock_path(&self, job_id: &str) -> JobResult<PathBuf> {
        self.path(job_id)
    }

    fn path(&self, job_id: &str) -> JobResult<PathBuf> {
        if job_id.is_empty() || job_id.contains(['/', '\\', '.']) {
            return Err(JobError::bad_input("bad job_id"));
        }
        Ok(self.dir.join(format!("{job_id}.lock")))
    }

    /// Waits up to `timeout` for `job_id`'s lock: `Ok(Some)` held, `Ok(None)` timed out.
    /// Same-process waiters (clones share the queue) are served in arrival order; with
    /// [`FifoScope::CrossProcess`] (see [`JobLocks::with_fifo_scope`]) so are waiters in other
    /// processes. Exclusion itself is the file lock's. `try_lock` bypasses the queue.
    pub fn acquire(&self, job_id: &str, timeout: Duration) -> JobResult<Option<JobLock>> {
        Ok(match self.acquire_cancellable(job_id, timeout, || false)? {
            LockWait::Acquired(lock) => Some(lock),
            _ => None,
        })
    }

    /// As [`JobLocks::acquire`]; `cancel` is checked before each attempt & about every
    /// 100 ms. A timed-out/cancelled/errored waiter leaves the FIFO at once, so a later
    /// waiter takes its place. Only the queue head attempts the file lock.
    pub fn acquire_cancellable(
        &self,
        job_id: &str,
        timeout: Duration,
        cancel: impl Fn() -> bool,
    ) -> JobResult<LockWait> {
        let path = self.path(job_id)?;
        std::fs::create_dir_all(&self.dir)?;
        let deadline = Instant::now().checked_add(timeout);
        // One line only: mixing the in-process and the ticket order could leave two waiters each
        // waiting on the other.
        let turn = match self.scope {
            FifoScope::Process => {
                let ticket = self.next_ticket.fetch_add(1, Ordering::Relaxed);
                Turn::Process(WaitSlot::join(&self.waiters, job_id, ticket))
            }
            FifoScope::CrossProcess => {
                match TicketSlot::join(&self.queue_dir(job_id), deadline, &cancel)? {
                    Joined::Slot(slot) => Turn::Ticket(slot),
                    Joined::Cancelled => return Ok(LockWait::Cancelled),
                    Joined::TimedOut => return Ok(LockWait::TimedOut),
                }
            }
        };
        let pause = match self.scope {
            FifoScope::Process => LOCK_QUEUE_PAUSE,
            FifoScope::CrossProcess => LOCK_TICKET_PAUSE,
        };
        loop {
            if cancel() {
                return Ok(LockWait::Cancelled);
            }
            let remaining = deadline.map(|d| d.saturating_duration_since(Instant::now()));
            if turn.is_head()? {
                let slice = remaining.map_or(LOCK_WAIT_SLICE, |r| r.min(LOCK_WAIT_SLICE));
                match FileLock::acquire(&path, &LockOptions::wait(slice)) {
                    Ok(lock) => return Ok(LockWait::Acquired(JobLock { _lock: lock })),
                    Err(FsError::Locked { .. } | FsError::LockTimeout { .. }) => {}
                    Err(error) => return Err(lock_error(error)),
                }
            } else {
                std::thread::sleep(remaining.map_or(pause, |r| r.min(pause)));
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Ok(LockWait::TimedOut);
            }
        }
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

    /// Delete the lock file of `job_id` if nobody holds it. `Ok(true)` when a file was removed,
    /// `Ok(false)` when there was none or it is held. Call it only for a job that is finished or
    /// gone: a runner that locks the file while it is being removed could end up holding a lock
    /// on a file nobody else sees.
    pub fn remove(&self, job_id: &str) -> JobResult<bool> {
        let path = self.path(job_id)?;
        // The ticket directory of a cross-process line, when no waiter is left in it.
        lock_queue::discard(&self.queue_dir(job_id));
        if !path.exists() {
            return Ok(false);
        }
        Ok(match self.try_lock(job_id)? {
            Some(lock) => {
                lock.release_removing();
                // A record of a dead owner that an earlier crash left beside the lock file.
                let mut owner = path.as_os_str().to_os_string();
                owner.push(".owner");
                let _ = std::fs::remove_file(PathBuf::from(owner));
                !path.exists()
            }
            None => false,
        })
    }

    /// Remove every unheld `<id>.lock` file in the directory whose job id `keep` rejects (return
    /// `true` for a job that may still run). Held locks are never touched. Returns the job ids
    /// whose files were removed, sorted. Files that are not `<id>.lock` are ignored. This is the
    /// sweep an app runs at startup for locks left by crashed runs and by jobs that ended before
    /// 0.3.2. See [`JobLocks::remove`] for when it is safe.
    pub fn sweep(&self, keep: impl Fn(&str) -> bool) -> JobResult<Vec<String>> {
        let mut removed = Vec::new();
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return Ok(removed);
        };
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(id) = name.strip_suffix(".queue") {
                // An emptied cross-process line of a job `keep` rejects; not reported as a lock.
                if !id.is_empty() && !keep(id) && self.path(id).is_ok() {
                    lock_queue::discard(&entry.path());
                }
                continue;
            }
            let Some(id) = name.strip_suffix(".lock") else {
                continue;
            };
            if id.is_empty() || keep(id) {
                continue;
            }
            // A name `path` would refuse (a dot in the id) is not ours.
            if self.path(id).is_err() {
                continue;
            }
            if self.remove(id)? {
                removed.push(id.to_string());
            }
        }
        removed.sort();
        Ok(removed)
    }
}

/// How a handler attempt ended, when the handler (not the runner) owns the job's outcome.
/// Returned by [`JobHandler::run_outcome`]; an error is returned as `Err` exactly as from
/// [`JobHandler::run`] (a `retryable` error pauses the job, any other fails it).
///
/// Use `Settled` or `Pending` when the handler has itself written the job's terminal state or
/// handed the job to something that will (a provider callback, a held status). The runner then
/// writes nothing for the job: no `FAILED` and no `COMPLETE`. That is how a handler that
/// settles its own jobs avoids a second, conflicting write after it returns.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum HandlerOutcome {
    /// The attempt produced this result and the runner completes the job with it: what
    /// `Ok(value)` from [`JobHandler::run`] has always meant.
    Done(Value),
    /// The handler already wrote the job's terminal state (through [`JobControl::transition`],
    /// [`JobControl::settle`] or the store). The runner writes nothing, checks that the row
    /// really is terminal, releases its claim (the per-job lock) and emits the usual lifecycle
    /// event for that terminal status with the reason "settled by handler". A row that is not
    /// terminal is a handler bug: the job is failed with [`crate::HANDLER_UNSETTLED`] rather
    /// than left running with no owner. A row in a held status counts as `Pending`. If the
    /// handler's own terminal write was deferred by a storage error ([`Settlement::Deferred`]),
    /// the runner takes over the claim and returns [`RunOutcome::Unsettled`].
    Settled,
    /// The outcome is pending with something that will settle the job later: the string says
    /// what, for the event log. The runner writes nothing, releases its claim and emits the
    /// usual `paused` lifecycle event with the reason "pending: ...". The job keeps whatever
    /// status the handler left it in, and the call returns [`RunOutcome::Paused`]. A job in a
    /// held status ([`crate::StatusModel::hold`]) waits for [`JobStore::resolve`] and is never
    /// run again meanwhile. A job that is not held stays recoverable (the next
    /// `resume_recoverable` runs the handler again, which is how a polling handler resumes), so
    /// a `Recovery::Never` job that is not held cannot be `Pending`: it is failed with
    /// [`crate::HANDLER_UNSETTLED`] because recovery would otherwise fail it as `interrupted`.
    Pending(String),
}

/// Reason on the lifecycle event the runner emits for [`HandlerOutcome::Settled`].
const SETTLED_BY_HANDLER: &str = "settled by handler";

/// What recovery does with a job whose handler finished but whose terminal write storage has
/// not taken (see [`Runner::pending_settlements`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum UnsettledRecovery {
    /// Never run a handler again for a job that reported its outcome: `resume_recoverable`,
    /// `recoverable_ids` and `run` skip it (`run` returns [`RunOutcome::Unsettled`]) until the
    /// held write lands, which `flush_pending_settlements` and the next successful store write
    /// retry. The default.
    #[default]
    Settle,
    /// Discard a held write that came from the journal of an earlier run and treat the job as
    /// unfinished: recovery runs the handler again (a `Recovery::Never` job is failed as
    /// `interrupted`). Choose it only for idempotent handlers.
    Rerun,
}

/// What a job does. The only product-specific part of an engine.
pub trait JobHandler: Send + Sync {
    /// Run one attempt. Return the result value; report progress and register
    /// artifacts through `ctl`; poll `ctl.is_cancelled()` between steps.
    /// A `retryable` error pauses the job (still recoverable); any other error fails it.
    fn run(&self, job: &Job, ctl: &JobControl) -> JobResult<Value>;

    /// What [`Runner`] calls first: the same attempt with the shared control handle, for handlers
    /// that hand the control to another thread. The default calls [`JobHandler::run`], so
    /// existing handlers are unchanged.
    fn run_shared(&self, job: &Job, ctl: &Arc<JobControl>) -> JobResult<Value> {
        self.run(job, ctl)
    }

    /// What [`Runner`] actually calls: an attempt that may end with a [`HandlerOutcome`].
    /// The default is [`JobHandler::run_shared`] with `Ok(value)` read as
    /// [`HandlerOutcome::Done`], so a handler that implements only `run` behaves exactly as
    /// before. Override it when the handler settles its own jobs; `run` must still be
    /// implemented (return the same result or an error, it is not called by the runner then).
    fn run_outcome(&self, job: &Job, ctl: &Arc<JobControl>) -> JobResult<HandlerOutcome> {
        self.run_shared(job, ctl).map(HandlerOutcome::Done)
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
    /// Write this attempt's terminal state (`complete`, `failed` or `cancelled`) itself. If the
    /// database refuses it on storage (`db` or `io`) the write is not lost: it is kept in
    /// memory and in the journal, and `Settlement::Deferred` is returned. Return
    /// [`HandlerOutcome::Settled`] then: the runner takes over the job's claim so it is not
    /// started again, retries the write when the store next completes one (or on
    /// [`JobStore::flush_pending_settlements`]) and returns [`RunOutcome::Unsettled`]. Fenced: a
    /// stale attempt gets `stale_attempt` and writes nothing. No lifecycle event is written; the
    /// handler owns the event stream, or the runner's own event follows the settled outcome.
    /// This is an explicit owner settle: it replaces a write already held for the job (extension
    /// values included) while the row is non-terminal, and once the terminal row is stored a
    /// different status is refused with `illegal_transition` (see [`JobStore::settle_terminal`]).
    pub fn settle(&self, t: &Transition) -> JobResult<Settlement> {
        self.heartbeat();
        if self.abandoned.load(Ordering::SeqCst) {
            return Err(JobError::stalled(self.idle().as_secs_f64()));
        }
        self.store
            .settle_terminal_fenced(&self.job_id, self.generation, t)
    }
    /// Hand a stored event to the sink.
    fn publish(&self, ev: &Event) {
        if let Some(s) = &self.sink {
            s(ev);
        }
    }
    /// A terminal lifecycle event of `job`, a row this run already holds. If the row has since
    /// gone (removed after its terminal write) the event is built from `job` instead of being
    /// dropped: it reaches the sink with `seq` 0, as it is stored nowhere.
    fn lifecycle_terminal(
        &self,
        job: &Job,
        stage: &str,
        progress: Option<f64>,
        message: Option<&str>,
    ) {
        self.heartbeat();
        match self
            .store
            .add_event_fenced(&self.job_id, self.generation, stage, progress, message)
        {
            Ok(ev) => self.publish(&ev),
            Err(e) if e.code == "job_not_found" => self.publish(&Event {
                seq: 0,
                job_id: self.job_id.clone(),
                at: now(),
                status: job.status.clone(),
                stage: stage.into(),
                attempt: job.attempts,
                progress,
                message: message.map(Into::into),
            }),
            Err(_) => {}
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
#[allow(clippy::large_enum_variant)]
pub enum RunOutcome {
    /// Reached a terminal state (complete, failed, or cancelled).
    Finished(Job),
    /// Another runner holds the job.
    LockedElsewhere(String),
    /// A retryable error paused the job; it stays recoverable. Also the result for a job that is
    /// held, and for a handler that reported [`HandlerOutcome::Pending`].
    Paused(Job),
    /// The job's outcome is final but storage refused its terminal write: it is held in memory
    /// and in the journal, with the job's claim, and retried (see
    /// [`JobStore::flush_pending_settlements`]). The `Job` is the row as it will read once the
    /// write lands; the stored row is still non-terminal until then. Before settle-later this case was
    /// an `Err` and the job stayed `running` and recoverable; opt out with
    /// [`Runner::with_settle_later`]`(false)`.
    Unsettled(Job),
    /// A terminal write held from an earlier run (the journal) landed while this call started,
    /// so the handler was not run. The record is the stored row: the job plus every extension
    /// value, including those the held write restored. Use it instead of any state read earlier.
    Restored(JobRecord),
}

impl RunOutcome {
    pub fn job(&self) -> Option<&Job> {
        match self {
            RunOutcome::Finished(j) | RunOutcome::Paused(j) | RunOutcome::Unsettled(j) => Some(j),
            RunOutcome::Restored(r) => Some(&r.job),
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
            RunOutcome::Unsettled(j) => {
                let mut v = j.to_value();
                v["unsettled"] = json!(true);
                v
            }
            RunOutcome::Restored(r) => r.to_value(),
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
    settle_later: bool,
    unsettled: UnsettledRecovery,
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
            settle_later: true,
            unsettled: UnsettledRecovery::Settle,
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
    /// Whether a terminal write that storage refuses (`db` or `io`) is held and retried instead
    /// of lost (default `true`). The run then returns [`RunOutcome::Unsettled`], the job's lock
    /// stays held, and the write lands on [`Runner::flush_pending_settlements`] or the store's
    /// next successful write; a restart applies it from the journal. With `false` the write's
    /// error is returned and the job stays `running` and recoverable, as before.
    pub fn with_settle_later(mut self, on: bool) -> Self {
        self.settle_later = on;
        self
    }
    pub fn settle_later(&self) -> bool {
        self.settle_later
    }
    /// What recovery does with a job whose terminal write is held (default
    /// [`UnsettledRecovery::Settle`]: never run its handler again).
    pub fn with_unsettled_recovery(mut self, p: UnsettledRecovery) -> Self {
        self.unsettled = p;
        self
    }
    pub fn unsettled_recovery(&self) -> UnsettledRecovery {
        self.unsettled
    }
    /// [`JobStore::flush_pending_settlements`] on this runner's store.
    pub fn flush_pending_settlements(&self) -> FlushReport {
        self.store.flush_pending_settlements()
    }
    /// Jobs that finished but whose terminal state is not stored ([`JobStore::pending_settlements`]).
    pub fn pending_settlements(&self) -> Vec<UnsettledJob> {
        self.store.pending_settlements()
    }
    /// [`JobStore::shutdown_detailed`]: one last flush, then the jobs that could not be stored.
    /// Call it after the last run ends, before the process exits.
    pub fn shutdown_detailed(&self) -> SettlementShutdown {
        self.store.shutdown_detailed()
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
        // The handler of this job finished and storage has not taken its outcome yet.
        if self.store.has_pending_settlement(&id) {
            match self.unsettled {
                UnsettledRecovery::Settle => return Ok(None),
                UnsettledRecovery::Rerun => {
                    self.store.discard_unclaimed_settlement(&id);
                }
            }
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
        let _pin = self.store.pin_delivery(job_id);
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
        let Some(lock) = self.locks.try_lock(job_id)? else {
            return Ok(RunOutcome::LockedElsewhere(job_id.into()));
        };
        let mut lock = Some(lock);
        let _live = LiveToken(&self.store, job_id);
        // The row stays out of `prune` until this run has announced its terminal state.
        let _pin = self.store.pin_delivery(job_id);
        let job = self.store.get(job_id)?;
        if job.is_terminal() {
            return Ok(RunOutcome::Finished(job));
        }
        // Held: waiting for an operator's `resolve`. Nothing runs, nothing is counted.
        if self.is_held(&job.status) {
            return Ok(RunOutcome::Paused(job));
        }
        // A handler already finished this job and storage has not taken its outcome yet (the
        // journal of an earlier run brought it back): do not run the handler again.
        if self.store.has_pending_settlement(job_id) {
            match self.unsettled {
                UnsettledRecovery::Rerun => {
                    self.store.discard_unclaimed_settlement(job_id);
                }
                UnsettledRecovery::Settle => {
                    self.store.flush_pending_settlements();
                    let now = self.store.get(job_id)?;
                    if now.is_terminal() {
                        // The held write landed (or the outcome was already stored): hand back
                        // the whole row, extension values included, so a caller holding older
                        // state does not write over what was restored.
                        return Ok(RunOutcome::Restored(self.store.record(job_id)?));
                    }
                    if let Some(t) = self.store.pending_transition(job_id) {
                        return Ok(RunOutcome::Unsettled(projected(&now, &t)));
                    }
                }
            }
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
        // A terminal write the handler deferred through `JobControl::settle` is the job's
        // outcome whatever the handler returned: the runner writes nothing and takes the claim.
        if let Some(held) = lock.take() {
            match self.store.adopt_claim(job_id, held, ctl.sink.clone()) {
                Ok(t) => {
                    if self.implicit_events {
                        let status = t.to.as_deref().unwrap_or(FAILED);
                        self.store
                            .set_pending_event(job_id, terminal_event(status, SETTLED_BY_HANDLER));
                    }
                    return Ok(RunOutcome::Unsettled(projected(&job, &t)));
                }
                Err(held) => lock = Some(held),
            }
        }
        match result {
            Ok(HandlerOutcome::Done(value)) => {
                let t = Transition::to(COMPLETE).result(value).error(None);
                let term = Terminal::new(t, "complete", Some(1.0), None, true);
                self.write_terminal(&ctl, owner, &job, term, lock)
            }
            Ok(HandlerOutcome::Settled) => self.handler_settled(&ctl, owner, &job, lock),
            Ok(HandlerOutcome::Pending(reason)) => {
                self.handler_pending(&ctl, owner, &job, &reason, lock)
            }
            // The handler began; it may already have had effects, so a `Never` job cannot pause
            // for a later re-run.
            Err(e) if e.retryable && never => {
                let e = JobError::interrupted_by(job_id, &e);
                let term = self.failure(&e, false);
                self.write_terminal(&ctl, owner, &job, term, lock)
            }
            Err(e) if e.retryable => {
                // The pause is the outcome; the refund, the error text on the row and the fresh
                // read below are bookkeeping. If storage refuses one of them (disk full, database
                // locked) the job must still come back paused and recoverable: turning that into
                // an `Err` made the caller treat a resumable job as a hard failure. Any other
                // error (a lost fence, say) still ends the run.
                let tolerated = |r: JobResult<()>| match r {
                    Err(w) if is_storage_error(&w) => Ok(()),
                    other => other,
                };
                if e.code == HOST_HANDOFF && per_run {
                    tolerated(self.store.refund_attempt_fenced(job_id, owner))?;
                }
                tolerated(
                    self.store
                        .record_error_fenced(job_id, owner, &e)
                        .map(|_| ()),
                )?;
                self.implicit(
                    &ctl,
                    "paused",
                    None,
                    Some(&format!("paused, will resume: {}", e.message)),
                );
                let now = match self.store.get(job_id) {
                    Ok(now) => now,
                    Err(w) if is_storage_error(&w) => job.clone(),
                    Err(w) => return Err(w),
                };
                Ok(RunOutcome::Paused(now))
            }
            Err(e) => {
                let term = self.failure(&e, true);
                self.write_terminal(&ctl, owner, &job, term, lock)
            }
        }
    }

    /// The terminal write of a failure.
    fn failure(&self, e: &JobError, cancelled_event: bool) -> Terminal {
        let t = Transition::to(FAILED).error(Some(
            e.to_json_with(self.store.config().includes_retryable()),
        ));
        Terminal::new(t, "failed", None, Some(e.message.clone()), cancelled_event)
    }

    /// The runner's own terminal write for the job `ctl` runs: one transaction with its
    /// lifecycle event, from data this run holds. If storage refuses it (and
    /// [`Runner::with_settle_later`] is on) the write is held with `lock` and the run is
    /// `Unsettled`; a job already terminal (cancelled meanwhile) is left as it is.
    fn write_terminal(
        &self,
        ctl: &JobControl,
        owner: i64,
        base: &Job,
        term: Terminal,
        lock: Option<JobLock>,
    ) -> JobResult<RunOutcome> {
        let job_id = ctl.job_id.as_str();
        let event = self
            .implicit_events
            .then(|| LifecycleEvent::new(term.stage, term.progress, term.message.as_deref()));
        let settled = self.store.settle_request(SettleRequest {
            job_id: job_id.to_string(),
            fence: Some(owner),
            transition: term.transition.clone(),
            event,
            claim: lock,
            sink: ctl.sink.clone(),
            defer: self.settle_later,
            owner: false,
        })?;
        match settled {
            Settle::Written {
                record: Some(rec),
                event,
            } => {
                if let Some(ev) = &event {
                    ctl.publish(ev);
                }
                Ok(RunOutcome::Finished(rec.job))
            }
            Settle::Written { record: None, .. } => {
                let now = self.store.get(job_id)?;
                if term.cancelled_event && now.status == CANCELLED {
                    self.implicit_final(ctl, &now, "cancelled", None, None);
                }
                Ok(RunOutcome::Finished(now))
            }
            Settle::Deferred => Ok(RunOutcome::Unsettled(projected(base, &term.transition))),
        }
    }

    /// Like `implicit`, for a terminal event of a row this run already holds (see
    /// [`JobControl::lifecycle_terminal`]).
    fn implicit_final(
        &self,
        ctl: &JobControl,
        job: &Job,
        stage: &str,
        progress: Option<f64>,
        msg: Option<&str>,
    ) {
        if self.implicit_events {
            ctl.lifecycle_terminal(job, stage, progress, msg);
        }
    }

    /// [`HandlerOutcome::Settled`]: the row must be terminal; the runner writes nothing.
    fn handler_settled(
        &self,
        ctl: &JobControl,
        owner: i64,
        base: &Job,
        lock: Option<JobLock>,
    ) -> JobResult<RunOutcome> {
        let job_id = ctl.job_id.as_str();
        let now = self.store.get(job_id)?;
        if now.is_terminal() {
            let (stage, progress) = match now.status.as_str() {
                COMPLETE => ("complete", Some(1.0)),
                FAILED => ("failed", None),
                _ => ("cancelled", None),
            };
            self.implicit_final(ctl, &now, stage, progress, Some(SETTLED_BY_HANDLER));
            if let Some(lock) = lock {
                lock.release_removing();
            }
            return Ok(RunOutcome::Finished(now));
        }
        // A held row waits for `resolve`: the handler did hand the outcome on.
        if self.is_held(&now.status) {
            return self.handler_pending(ctl, owner, base, "held for resolve", lock);
        }
        // The handler says it settled the job and the row says it did not. Nobody owns the job:
        // fail it with a typed error instead of leaving it running.
        let e = JobError::handler_unsettled(job_id, &now.status);
        let term = self.failure(&e, true);
        self.write_terminal(ctl, owner, base, term, lock)
    }

    /// [`HandlerOutcome::Pending`]: the runner writes nothing and releases its claim.
    fn handler_pending(
        &self,
        ctl: &JobControl,
        owner: i64,
        base: &Job,
        reason: &str,
        lock: Option<JobLock>,
    ) -> JobResult<RunOutcome> {
        let job_id = ctl.job_id.as_str();
        let now = self.store.get(job_id)?;
        // Settled meanwhile (the callback got there first): that is a settled job.
        if now.is_terminal() {
            return self.handler_settled(ctl, owner, base, lock);
        }
        if !self.is_held(&now.status) && self.store.run_class(job_id)?.0 == Recovery::Never {
            // Recovery would fail it as `interrupted` and lose the pending outcome.
            let e = JobError::handler_pending_unowned(job_id, &now.status);
            let term = self.failure(&e, true);
            return self.write_terminal(ctl, owner, base, term, lock);
        }
        self.implicit(ctl, "paused", None, Some(&format!("pending: {reason}")));
        Ok(RunOutcome::Paused(now))
    }

    /// Run the handler inline, or on a watched worker thread when a stall
    /// timeout is set. A stalled attempt is abandoned: its thread is told to
    /// stop (`abort_reason`), its result is discarded, and the job pauses.
    fn run_handler(&self, job: &Job, ctl: &Arc<JobControl>) -> JobResult<HandlerOutcome> {
        let Some(limit) = self.stall_timeout else {
            return catch_unwind(AssertUnwindSafe(|| self.handler.run_outcome(job, ctl)))
                .unwrap_or_else(|_| Err(JobError::new("panic", "job handler panicked")));
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let (handler, job2, ctl2) = (self.handler.clone(), job.clone(), ctl.clone());
        std::thread::Builder::new()
            .name(format!("rk-job-{}", job.job_id))
            .spawn(move || {
                let r = catch_unwind(AssertUnwindSafe(|| handler.run_outcome(&job2, &ctl2)))
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

    /// Remove the lock files of jobs that are terminal or no longer in the store, and are not
    /// held (see [`JobLocks::sweep`]). A job that is queued, running or held keeps its file.
    /// Returns the job ids whose files were removed.
    pub fn sweep_job_locks(&self) -> JobResult<Vec<String>> {
        let live: std::collections::HashSet<String> =
            self.store.non_terminal_ids()?.into_iter().collect();
        self.locks.sweep(|id| live.contains(id))
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
    /// directories. Pass `root` only when this process is the sole engine for it. The files of
    /// a finished job are not claimed: an app that keeps them until the job is deleted uses
    /// [`Runner::sweep_unclaimed_with`]`(root, `[`ClaimScope::All`]`)`.
    pub fn sweep_unclaimed(&self, root: &Path) -> JobResult<Vec<PathBuf>> {
        self.sweep_unclaimed_with(root, ClaimScope::NonTerminal)
    }

    /// [`Runner::sweep_unclaimed`] with the claim scope chosen: [`ClaimScope::NonTerminal`] is
    /// what `sweep_unclaimed` does; [`ClaimScope::All`] also claims the artifact paths (and, with
    /// a scratch root configured, the `<scratch_root>/<job_id>` directory) recorded on terminal
    /// rows still in the store, so a sweep never removes the media of a finished job that has not
    /// been deleted or pruned.
    pub fn sweep_unclaimed_with(&self, root: &Path, scope: ClaimScope) -> JobResult<Vec<PathBuf>> {
        let mut claimed: Vec<PathBuf> = Vec::new();
        for p in self.store.claimed_paths_with(scope)? {
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

/// A terminal write the runner makes, with the lifecycle event that announces it.
struct Terminal {
    transition: Transition,
    stage: &'static str,
    progress: Option<f64>,
    message: Option<String>,
    /// When the job turns out to be cancelled already, announce that instead.
    cancelled_event: bool,
}

impl Terminal {
    fn new(
        transition: Transition,
        stage: &'static str,
        progress: Option<f64>,
        message: Option<String>,
        cancelled_event: bool,
    ) -> Self {
        Self {
            transition,
            stage,
            progress,
            message,
            cancelled_event,
        }
    }
}

/// The lifecycle event for a job that ended `status`, with `reason`.
fn terminal_event(status: &str, reason: &str) -> LifecycleEvent {
    match status {
        COMPLETE => LifecycleEvent::new("complete", Some(1.0), Some(reason)),
        FAILED => LifecycleEvent::new("failed", None, Some(reason)),
        _ => LifecycleEvent::new("cancelled", None, Some(reason)),
    }
}

/// Drops the job's cancel flag from the store registry when its run ends, however it ends.
struct LiveToken<'a>(&'a JobStore, &'a str);

impl Drop for LiveToken<'_> {
    fn drop(&mut self) {
        self.0.release_cancel_token(self.1);
    }
}

#[cfg(test)]
mod lock_wait_tests {
    use super::*;

    #[test]
    fn a_cancelled_waiter_reports_cancelled_and_leaves_the_queue() {
        let dir = tempfile::tempdir().unwrap();
        let locks = JobLocks::new(dir.path().join("locks"));
        let held = locks.try_lock("job-c").unwrap().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let waiter = {
            let (locks, cancel) = (locks.clone(), Arc::clone(&cancel));
            std::thread::spawn(move || {
                matches!(
                    locks.acquire_cancellable("job-c", Duration::from_secs(30), || {
                        cancel.load(Ordering::SeqCst)
                    }),
                    Ok(LockWait::Cancelled)
                )
            })
        };
        std::thread::sleep(Duration::from_millis(150));
        cancel.store(true, Ordering::SeqCst);
        assert!(waiter.join().unwrap());
        drop(held);
        let Ok(LockWait::Acquired(_held)) =
            locks.acquire_cancellable("job-c", Duration::ZERO, || false)
        else {
            panic!("a free lock is acquired at once");
        };
        assert!(matches!(
            locks.acquire_cancellable("job-c", Duration::ZERO, || false),
            Ok(LockWait::TimedOut)
        ));
    }
}
