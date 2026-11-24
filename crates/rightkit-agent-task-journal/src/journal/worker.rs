//! The journal's workers: leases accepted tasks under the current owner epoch, runs them one at a
//! time per lane, and settles them. Two lanes: the general worker runs every kind but
//! `agent.request`, the agent worker runs `agent.request` alone (so its long run never blocks the
//! others). Also the service wrapper that opens the journal, starts the workers and stops them
//! with a bounded wait.
//!
//! Every store write here carries the generation the worker holds (`JobStore::*_fenced`), so a
//! write that arrives after another holder fenced the task (a cancel before execution, a
//! takeover) is rejected as `stale_attempt` and the worker drops the task silently.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rightkit_agent_task_contract::{TaskEnvelope, TaskStatus};
use rightkit_job_queue::{JobError, JobResult, Transition};
use serde_json::{json, Value};

use super::{lock, tr, Executor, Journal, RunCtl, TaskError, TaskInput, MAX_ATTEMPTS};
use crate::host::{FinishedTask, JournalHost, Lane, LogLevel};
use crate::schema::{self, add_route, ext_i64, now_ms, pulse_name, store_name, topic_for};

/// How long an idle worker sleeps before it looks at the journal again. A submit or cancel wakes
/// it at once, so this only covers rows another path wrote.
const IDLE: Duration = Duration::from_secs(30);
/// How long the worker waits after an error before it tries again.
const AFTER_ERROR: Duration = Duration::from_secs(5);
/// How long `shutdown` waits, in all, for the tasks in progress.
const STOP_WAIT: Duration = Duration::from_secs(5);

/// Every lane, in the order their workers start and their wake flags are indexed.
const LANES: [Lane; 4] = [Lane::General, Lane::Agent, Lane::Act, Lane::Relay];

fn lane_index(lane: Lane) -> usize {
    match lane {
        Lane::General => 0,
        Lane::Agent => 1,
        Lane::Act => 2,
        Lane::Relay => 3,
    }
}

fn lane_thread_suffix(lane: Lane) -> &'static str {
    match lane {
        Lane::General => "worker",
        Lane::Agent => "agent-worker",
        Lane::Act => "act-worker",
        Lane::Relay => "relay-worker",
    }
}

struct Claim {
    id: String,
    kind: String,
    params: Value,
    /// The envelope stored at ACCEPTANCE; `None` only if the stored record no longer parses.
    envelope: Option<TaskEnvelope>,
    /// The generation the worker's writes carry.
    generation: i64,
    /// The task is `leased` and still has to move to `running`. A retry from `uncertain` is
    /// already `running`.
    needs_start: bool,
}

enum Entry {
    Fresh,
    Takeover,
    Retry,
    /// A `waiting-device` task whose computer is reachable again: leased anew (a new owner epoch).
    Woken,
}

/// The relay lane looks for tasks to park and wake this often.
const RELAY_IDLE: Duration = Duration::from_secs(5);

enum Step {
    Done,
    /// Another holder fenced the task, or it is already terminal: stop without writing.
    Lost,
    Failed(JobError),
}

enum Outcome {
    Value(Value),
    Error(TaskError),
    Panic,
}

impl Journal {
    /// Picks the next task of `lane` this process may run and takes its lease. `None`: nothing to
    /// run. A task of the other lane is skipped before anything is written.
    fn claim_next(&self, lane: Lane) -> JobResult<Option<Claim>> {
        let gate = lock(&self.gate);
        if self.closing.load(Ordering::SeqCst) {
            return Ok(None);
        }
        let epoch = i64::try_from(self.epoch).unwrap_or(i64::MAX);
        for id in self.store.non_terminal_ids()? {
            let rec = self.load(&id)?;
            if self.lane_of(&rec.job.kind) != lane {
                continue;
            }
            let Ok(status) = Self::status_of(&rec) else {
                continue;
            };
            let entry = match status {
                TaskStatus::Accepted => Entry::Fresh,
                // A task bound to another computer that was parked there; woken below when the computer is reachable.
                TaskStatus::WaitingDevice if self.needs_device(&rec.job.kind) => Entry::Woken,
                // A lease of an older owner: it began no effect, so the new owner takes it.
                TaskStatus::Leased if ext_i64(&rec, "owner_epoch") < epoch => Entry::Takeover,
                // The effect may have happened. Only a read-only kind is run again, a bounded
                // number of times. An effect-free kind that is not read-only (`agent.request`)
                // is never run again; recovery settles it `failed` (`interrupted`).
                TaskStatus::Uncertain
                    if self.is_read_only(&rec.job.kind) && rec.job.attempts < MAX_ATTEMPTS =>
                {
                    Entry::Retry
                }
                _ => continue,
            };
            // A task bound to another computer (`agent.relay`) is leased only while that computer is reachable: otherwise it
            // is parked `waiting-device` (from `accepted`, the move the transition table allows) and left there, and one whose
            // signature expired while it waited fails and is never run late.
            if self.needs_device(&rec.job.kind) && matches!(entry, Entry::Fresh | Entry::Woken) {
                let params = rec
                    .job
                    .request
                    .get("params")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                if self.host.device_task_expired(&params) {
                    self.fenced(
                        &id,
                        tr(
                            TaskStatus::Failed,
                            Some("the owner's signature expired before the computer was reachable"),
                        )
                        .error(Some(json!({
                            "code": "relay_expired",
                            "message": "the phone's signature expired before the other computer could be reached; ask again",
                            "retryable": false,
                        }))),
                        true,
                    )?;
                    continue;
                }
                let target = rec
                    .job
                    .request
                    .pointer("/envelope/target_device/device_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if !self.host.device_ready(target) {
                    if matches!(entry, Entry::Fresh) {
                        self.fenced(
                            &id,
                            tr(
                                TaskStatus::WaitingDevice,
                                Some("waiting for the computer to report in"),
                            ),
                            true,
                        )?;
                    }
                    continue;
                }
            }
            if let Some(deadline) = rec.ext.get("deadline_ms").and_then(Value::as_i64) {
                if deadline <= now_ms() {
                    self.fenced(
                        &id,
                        tr(
                            TaskStatus::Failed,
                            Some("deadline passed before the task started"),
                        )
                        .error(Some(json!({
                            "code": "deadline_exceeded",
                            "message": "the task's deadline passed before it started",
                            "retryable": false,
                        }))),
                        true,
                    )?;
                    continue;
                }
            }
            let (t, needs_start) = match entry {
                Entry::Fresh => (tr(TaskStatus::Leased, None), true),
                Entry::Takeover => (
                    tr(
                        TaskStatus::Leased,
                        Some("lease taken over from an older owner epoch"),
                    ),
                    true,
                ),
                Entry::Retry => (
                    tr(TaskStatus::Running, Some("read-only retry after uncertain")),
                    false,
                ),
                Entry::Woken => (
                    tr(TaskStatus::Leased, Some("the computer is reachable again")),
                    true,
                ),
            };
            let t = t.count_attempt().set("owner_epoch", epoch);
            let Some((after, generation)) = self.fenced(&id, t, true)? else {
                continue;
            };
            if matches!(entry, Entry::Takeover) {
                // A same-status move fires no hook; record the takeover as its own event.
                self.store.write(|conn| {
                    let mut detail = json!({
                        "kind": after.job.kind,
                        "from": pulse_name(TaskStatus::Leased),
                        "note": "lease taken over from an older owner epoch",
                        "error": Value::Null,
                    });
                    add_route(self.host.as_ref(), &mut detail, &after.job.kind);
                    schema::insert_event(
                        conn,
                        &id,
                        topic_for(TaskStatus::Leased),
                        store_name(TaskStatus::Leased),
                        epoch,
                        after.job.attempts,
                        &detail,
                    )
                })?;
            }
            drop(gate);
            return Ok(Some(Claim {
                id,
                kind: after.job.kind.clone(),
                params: after
                    .job
                    .request
                    .get("params")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
                envelope: after
                    .job
                    .request
                    .get("envelope")
                    .and_then(|v| serde_json::from_value(v.clone()).ok()),
                generation,
                needs_start,
            }));
        }
        Ok(None)
    }

    /// The lease-to-running move. Writes nothing once the journal is closing, checked under the
    /// gate.
    fn step_unless_closing(&self, id: &str, generation: i64, t: Transition) -> Step {
        let gate = lock(&self.gate);
        if self.closing.load(Ordering::SeqCst) {
            return Step::Lost;
        }
        let result = self.store.transition_fenced(id, generation, &t);
        drop(gate);
        match result {
            Ok(Some(_)) => Step::Done,
            Ok(None) => Step::Lost,
            Err(e) if e.code == "stale_attempt" => Step::Lost,
            Err(e) => Step::Failed(e),
        }
    }

    /// The reaper (AG-8): a running task of a kind that `beats` whose last heartbeat is older than `STALL_AFTER_MS` has stopped (its
    /// thread is stuck, say). It is ended the way a lost run is (`lose_run`: `uncertain`, then `failed` `interrupted` for an
    /// effect-free kind), at a fresh generation, so the stuck executor's late write is rejected as `stale_attempt`. Only a run of this
    /// epoch that has written at least one heartbeat is judged, so a task is never reaped for what it has not yet had the chance to
    /// do. Runs on the general lane's idle tick, which no run of a beating kind occupies.
    fn reap_stalled(&self) {
        let gate = lock(&self.gate);
        let epoch = i64::try_from(self.epoch).unwrap_or(i64::MAX);
        let Ok(ids) = self.store.non_terminal_ids() else {
            return;
        };
        let now = now_ms();
        for id in ids {
            let result = (|| -> JobResult<()> {
                let rec = self.load(&id)?;
                if !self.facts(&rec.job.kind).is_some_and(|k| k.beats)
                    || ext_i64(&rec, "owner_epoch") != epoch
                    || Self::status_of(&rec)? != TaskStatus::Running
                {
                    return Ok(());
                }
                let beat = rec
                    .ext
                    .get("note")
                    .and_then(Value::as_str)
                    .and_then(|note| note.strip_prefix("heartbeat "))
                    .and_then(|ms| ms.parse::<i64>().ok());
                if beat.is_some_and(|at| now.saturating_sub(at) > self.host.stall_after_ms()) {
                    self.lose_run(
                        &id,
                        &rec.job.kind,
                        TaskStatus::Running,
                        "the run stopped beating; ended by the journal",
                    )?;
                }
                Ok(())
            })();
            if let Err(error) = result {
                self.log(
                    LogLevel::Error,
                    "journal.reap_failed",
                    json!({ "task_id": id, "code": error.code }),
                );
            }
        }
        drop(gate);
        self.publish_pending();
    }

    /// A stop that could not wait for `lane`'s task: settles what this process left in progress
    /// so nothing stays `running` under a live lease after the engine exits. Only kinds that
    /// `settles_when_lost` (effect-free, not retried) are touched; this is the same sequence
    /// `recover` would run at the next start (`running` to `uncertain` to `failed`
    /// `interrupted`; `cancel-requested` to `cancelled`), taken now at a fresh generation. The
    /// abandoned worker's late write is then rejected as `stale_attempt`. A read-only kind is
    /// left for `recover` and its bounded retry.
    fn abandon_running(&self, lane: Lane) {
        let gate = lock(&self.gate);
        let epoch = i64::try_from(self.epoch).unwrap_or(i64::MAX);
        let ids = match self.store.non_terminal_ids() {
            Ok(ids) => ids,
            Err(error) => {
                self.log(
                    LogLevel::Error,
                    "journal.abandon_failed",
                    json!({ "code": error.code }),
                );
                return;
            }
        };
        for id in ids {
            let result = (|| -> JobResult<()> {
                let rec = self.load(&id)?;
                if self.lane_of(&rec.job.kind) != lane
                    || !(self.settles_when_lost(&rec.job.kind) || self.is_audited(&rec.job.kind))
                    || ext_i64(&rec, "owner_epoch") != epoch
                {
                    return Ok(());
                }
                match Self::status_of(&rec)? {
                    TaskStatus::Running => {
                        self.lose_run(
                            &id,
                            &rec.job.kind,
                            TaskStatus::Running,
                            "engine stopping; run abandoned",
                        )?;
                    }
                    TaskStatus::WaitingUser if self.is_audited(&rec.job.kind) => {
                        self.lose_run(
                            &id,
                            &rec.job.kind,
                            TaskStatus::WaitingUser,
                            "engine stopping; run abandoned",
                        )?;
                    }
                    TaskStatus::CancelRequested => {
                        self.fenced(
                            &id,
                            tr(
                                TaskStatus::Cancelled,
                                Some("cancel requested; engine stopping; nothing can run any more"),
                            )
                            .error(None),
                            true,
                        )?;
                    }
                    _ => {}
                }
                Ok(())
            })();
            if let Err(error) = result {
                self.log(
                    LogLevel::Error,
                    "journal.abandon_failed",
                    json!({ "task_id": id, "code": error.code }),
                );
            }
        }
        drop(gate);
        self.publish_pending();
    }

    fn run_claim(&self, claim: Claim) {
        if claim.needs_start {
            // The start is refused while the engine is stopping, decided under the gate: either
            // it commits before `abandon_running` looks (which then settles it), or it never
            // happens. The lease stays `leased` and the next owner takes it over.
            match self.step_unless_closing(
                &claim.id,
                claim.generation,
                tr(TaskStatus::Running, None),
            ) {
                Step::Done => self.publish_pending(),
                Step::Lost => return,
                Step::Failed(error) => {
                    self.log(
                        LogLevel::Error,
                        "journal.worker_error",
                        json!({ "code": error.code }),
                    );
                    return;
                }
            }
        }
        let exec: Executor = self.exec.clone();
        let outcome = match &claim.envelope {
            None => Outcome::Error(TaskError::new(
                "executor_failed",
                "the stored task envelope could not be read",
            )),
            Some(envelope) => {
                let ctl = RunCtl {
                    journal: self,
                    id: &claim.id,
                    generation: claim.generation,
                };
                let input = TaskInput {
                    kind: &claim.kind,
                    params: &claim.params,
                    envelope,
                    ctl: &ctl,
                };
                match catch_unwind(AssertUnwindSafe(|| exec(&input))) {
                    Ok(Ok(value)) => Outcome::Value(value),
                    Ok(Err(error)) => Outcome::Error(error),
                    Err(_) => Outcome::Panic,
                }
            }
        };
        self.finish(&claim, outcome);
        self.publish_pending();
    }

    /// Settles the task from the executor's outcome. A cancel request that is still pending when
    /// the work ends wins over a result; completion wins only over a cancel that was not asked.
    fn finish(&self, claim: &Claim, outcome: Outcome) {
        let gate = lock(&self.gate);
        // Set only where the run's value is committed as `succeeded` (not failed, cancelled or uncertain).
        let mut succeeded = false;
        // Likewise for a run that ended in an error or a panic.
        let mut failed = false;
        let result: JobResult<Option<Transition>> = (|| {
            let rec = self.load(&claim.id)?;
            let status = Self::status_of(&rec)?;
            if !matches!(
                status,
                TaskStatus::Running | TaskStatus::CancelRequested | TaskStatus::WaitingUser
            ) {
                return Ok(None);
            }
            Ok(Some(match outcome {
                // An executor that returns a result while the task still waits for the owner
                // lost its move back to `running`; the table has no `waiting-user` to
                // `succeeded`, so the run is failed rather than left waiting.
                Outcome::Value(_) if status == TaskStatus::WaitingUser => tr(
                    TaskStatus::Failed,
                    Some("ended while waiting for the owner"),
                )
                .error(Some(json!({
                    "code": "interrupted",
                    "message": "the run ended while it was waiting for the owner",
                    "retryable": false,
                }))),
                Outcome::Value(_) if status == TaskStatus::CancelRequested => tr(
                    TaskStatus::Cancelled,
                    Some("cancel requested before the result was committed"),
                )
                .error(None),
                Outcome::Value(value) => {
                    succeeded = true;
                    tr(TaskStatus::Succeeded, None).result(value).error(None)
                }
                Outcome::Error(error) => {
                    failed = true;
                    tr(TaskStatus::Failed, None).error(Some(json!({
                        "code": error.code,
                        "message": error.message,
                        "retryable": false,
                    })))
                }
                Outcome::Panic => {
                    failed = true;
                    tr(TaskStatus::Failed, None).error(Some(json!({
                        "code": "executor_panic",
                        "message": "the task's executor panicked",
                        "retryable": false,
                    })))
                }
            }))
        })();
        let written = match result {
            Ok(Some(t)) => self
                .store
                .transition_fenced(&claim.id, claim.generation, &t),
            Ok(None) => Ok(None),
            Err(e) => Err(e),
        };
        drop(gate);
        match written {
            // The status is committed: the host settles what hangs off a finished task (an Inbox item, an alert, an
            // occurrence queued behind it). It runs after the gate is released and never changes the task.
            Ok(Some(_)) => {
                if succeeded || failed {
                    self.host.task_finished(&FinishedTask {
                        id: &claim.id,
                        kind: &claim.kind,
                        params: &claim.params,
                        envelope: claim.envelope.as_ref(),
                        succeeded,
                    });
                }
            }
            Ok(None) => {}
            Err(e) if e.code == "stale_attempt" => {}
            // The task stays `running`; the next start finds it from an older epoch and makes
            // it `uncertain`.
            Err(e) => self.log(
                LogLevel::Error,
                "journal.worker_error",
                json!({ "code": e.code }),
            ),
        }
    }

    fn work_loop(&self, lane: Lane) {
        loop {
            if self.closing.load(Ordering::SeqCst) {
                return;
            }
            let mut wait = if lane == Lane::Relay {
                RELAY_IDLE
            } else {
                IDLE
            };
            match self.claim_next(lane) {
                Ok(Some(claim)) => {
                    self.publish_pending();
                    self.run_claim(claim);
                    continue;
                }
                Ok(None) => {
                    // The idle tick is the journal's sweep: link request rows past expiry.
                    if lane == Lane::General {
                        self.prune_link_requests();
                        self.reap_stalled();
                    }
                }
                Err(error) => {
                    self.log(
                        LogLevel::Error,
                        "journal.worker_error",
                        json!({ "code": error.code }),
                    );
                    wait = AFTER_ERROR;
                }
            }
            let (flag, signal) = &self.wake[lane_index(lane)];
            let mut woken = lock(flag);
            if !*woken && !self.closing.load(Ordering::SeqCst) {
                woken = signal
                    .wait_timeout(woken, wait)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
            *woken = false;
        }
    }
}

/// One worker thread of a lane, and the channel that says it ended.
type Worker = (Lane, JoinHandle<()>, Receiver<()>);

/// The journal and its worker threads, one per lane.
pub struct JournalService {
    journal: Arc<Journal>,
    workers: Mutex<Vec<Worker>>,
}

fn spawn_worker(journal: &Arc<Journal>, lane: Lane) -> JobResult<Worker> {
    let (done, finished) = channel();
    let runner = journal.clone();
    let name = format!(
        "{}-{}",
        journal.host.thread_prefix(),
        lane_thread_suffix(lane)
    );
    let handle = std::thread::Builder::new()
        .name(name)
        .spawn(move || {
            runner.work_loop(lane);
            let _ = done.send(());
        })
        .map_err(|e| JobError::new("io", format!("cannot start the journal worker: {e}")))?;
    Ok((lane, handle, finished))
}

impl JournalService {
    /// Opens the journal at `path` and starts the workers, one per lane. The folder of `path` must exist
    /// (the product creates it, owner-only where the platform has that).
    pub fn start(
        path: &std::path::Path,
        host: Arc<dyn JournalHost>,
        exec: Executor,
        local_device: Option<String>,
    ) -> JobResult<JournalService> {
        let journal = Journal::open(path, host, exec, local_device)?;
        let service = JournalService {
            journal,
            workers: Mutex::new(Vec::new()),
        };
        for lane in LANES {
            match spawn_worker(&service.journal, lane) {
                Ok(worker) => lock(&service.workers).push(worker),
                Err(error) => {
                    // A journal with one lane only must not keep running; stop what started.
                    service.shutdown();
                    return Err(error);
                }
            }
        }
        Ok(service)
    }

    pub fn journal(&self) -> &Arc<Journal> {
        &self.journal
    }

    /// Closes admission for this process, lets the tasks in progress finish (at most five seconds
    /// in all, one deadline for both lanes), and stops the workers. A task still running when the
    /// wait ends is not left to the next start when it is an effect-free, non-retried kind
    /// (`agent.request`): the journal settles it now (`uncertain`, then `failed` `interrupted`)
    /// at a fresh generation, so it is never `running` under a live lease after the engine
    /// exits, and the abandoned run's late write is rejected as `stale_attempt`. Any other kind
    /// still running is left `running` and becomes `uncertain` at the next start (`recover`).
    pub fn shutdown(&self) {
        self.journal.closing.store(true, Ordering::SeqCst);
        self.journal.wake();
        let workers = std::mem::take(&mut *lock(&self.workers));
        let deadline = Instant::now() + STOP_WAIT;
        let mut abandoned = Vec::new();
        for (lane, handle, finished) in workers {
            let left = deadline.saturating_duration_since(Instant::now());
            match finished.recv_timeout(left) {
                Err(RecvTimeoutError::Timeout) => abandoned.push(lane),
                // Ended (a panic also closes the channel): nothing is running on it.
                Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                    let _ = handle.join();
                }
            }
        }
        for lane in abandoned {
            self.journal.abandon_running(lane);
        }
    }
}
