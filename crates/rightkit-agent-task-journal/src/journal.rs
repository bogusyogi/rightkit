//! The durable task journal: a task that survives UI close and restart.
//!
//! Store. The journal is a `rightkit_job_queue::JobStore` (SQLite, full durability) at
//! `<state>/journal/tasks.db`. One row per task: the job id is the task id (the engine mints it
//! at ACCEPTANCE), the submitter's idempotency key is the store's unique `request_id`, and the
//! submitted envelope, kind and params are the row's request. The journal keeps three more things in
//! the same database, written in the same transactions as the task rows:
//!
//! * `pulse_task_events`, the lifecycle event outbox (`task.accepted`, `task.running`, ...);
//! * `pulse_journal_meta`, the schema version, the owner epoch, the admission switch and the
//!   publish cursor;
//! * the `jobs` extension columns `owner_epoch`, `utterance_id`, `utterance_generation`,
//!   `device_id`, `deadline_ms` and `note`.
//!
//! Only the job store's queue and fencing primitives are used. Its `Runner` is not: a task
//! has ten statuses, an owner epoch and an `uncertain` state the runner's pause-and-resume model
//! does not have, and `Recovery::Resume` would re-run a lost job blindly.
//!
//! ACCEPTANCE is the commit of the `jobs` row (with its `task.accepted` event) by
//! `JobStore::submit_with`: one SQLite transaction that also resolves a repeated idempotency key
//! to the original task. `task.submit` returns only after that commit. A second dedupe, by
//! (utterance id, generation), is checked in the same critical section.
//!
//! Device-link replay. `pulse_link_requests` (schema 2, `link.rs`) remembers each device-link
//! request id. An accepted request's row is written by the store hook in the same transaction as
//! the task row (`submit_linked`), so a redelivery gets the original task and a crash leaves both or
//! neither. `link_row` is the read-only lookup the product's replay store uses; rows are pruned past their
//! expiry plus the clock-skew margin by the idle tick of the general worker lane.
//!
//! Status moves. Every status change goes through the job store's transition inside one
//! transaction, and the store hook (`schema::JournalHook`) applies `rightkit_agent_task_contract::transition`, so a
//! move the C0 table forbids is refused and nothing is written.
//!
//! Lease and fencing. Two counters fence old owners. The owner epoch is per engine process: it is
//! incremented and committed at every open, before anything is leased, and a task row records the
//! epoch of the process that leased it. The job store's `attempt_gen` is per task: every lease,
//! takeover, retry, recovery and pre-execution cancel first bumps it (`JobStore::fence`), and
//! every write the worker makes carries the generation it holds, so a write from a superseded
//! holder is rejected (`stale_attempt`) and changes nothing.
//!
//! Restart. At open, a task found `running` from an older epoch becomes `uncertain` (the effect
//! may have happened), never silently re-run. A read-only kind is then retried by the worker
//! (`uncertain` to `running`, a new attempt, at most `MAX_ATTEMPTS` in all). A kind that is
//! `effect_free` but not read-only (`agent.request`: tools off, no writes) is never retried; its
//! lost run goes `uncertain` and is then settled `failed` (`interrupted`) at once, as a second
//! event, because no effect was possible. Any kind that is neither would stay `uncertain` until a
//! receipt query resolves it (none exists). A `leased` task of an older epoch had started no
//! effect and is leased again by the new owner. A `cancel-requested` task of an older epoch is
//! `cancelled` when its kind is effect-free (nothing outside the journal can be in flight) and
//! `uncertain` otherwise.
//!
//! Lanes. Two workers share the one store and the one gate. The general lane runs every kind
//! no other lane claims; the agent lane runs only the kinds the host puts on it (`agent.request`), one at a time, so a run of up
//! to 180 s never delays `storage.snapshot`. The store has no claim-by-kind call (a task is
//! leased by `non_terminal_ids` plus `fenced`), so each worker skips the kinds of the other lane
//! before it writes anything; the kind sets are disjoint and every lease is taken under the gate
//! at a fresh generation, so the fencing is the same as with one worker. A stop that cannot end
//! the agent run in time settles it itself (`JournalService::shutdown`): `uncertain`,
//! then `failed` (`interrupted`), at a fresh generation, so it is never left `running`.
//!
//! Events. Every lifecycle event is a row committed with the status change that caused it.
//! After a commit the journal publishes the rows past its persisted cursor to the host's
//! log and event bus (`JournalHost::log`, `JournalHost::emit`, topics `task.*`), then advances the
//! cursor. A crash between publishing and advancing publishes the row again at the next start:
//! delivery is at-least-once, and each payload carries its `seq` so a consumer can drop repeats.
//! An event is never published before its commit.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use rightkit_agent_task_contract::task::TASK_ENVELOPE_SCHEMA_VERSION;
use rightkit_agent_task_contract::{IntentSource, TaskEnvelope, TaskStatus};
use rightkit_job_queue::rusqlite::OptionalExtension;
use rightkit_job_queue::{
    Ext, JobError, JobRecord, JobResult, JobStore, NewJob, PrunePolicy, SubmitOptions, Transition,
};
use serde_json::{json, Value};

use crate::host::{JournalHost, KindFacts, Lane, LogLevel};
use crate::link::{self, LinkEntry, LinkOutcome, LinkRecord, LinkSubmit};
use crate::schema::{
    self, ext_i64, meta_get, meta_set, now_ms, parse_status, pulse_name, store_name,
    JOURNAL_SCHEMA_VERSION, META_ADMISSION, META_PUBLISHED,
};

mod worker;

pub use worker::JournalService;

/// What the executor receives for one task.
pub struct TaskInput<'a> {
    pub kind: &'a str,
    pub params: &'a Value,
    /// The submitted envelope (its quote, origin and device), as stored at ACCEPTANCE.
    pub envelope: &'a TaskEnvelope,
    /// The running task's handle on the journal: its state (a cancel arrives here) and the
    /// `running` / `waiting-user` moves of a kind that asks the owner during its run.
    pub ctl: &'a RunCtl<'a>,
}

/// A typed failure. The journal records `{code, message, retryable: false}`; the message carries
/// no request text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskError {
    pub code: String,
    pub message: String,
}

impl TaskError {
    pub fn new(code: &str, message: impl Into<String>) -> TaskError {
        TaskError {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

/// What the engine runs for a task.
pub type Executor = Arc<dyn Fn(&TaskInput<'_>) -> Result<Value, TaskError> + Send + Sync>;

/// The lifecycle topics the journal publishes on the host event bus, one per status.
pub const TOPICS: &[&str] = &[
    "task.accepted",
    "task.leased",
    "task.running",
    "task.waiting-user",
    "task.waiting-device",
    "task.cancel-requested",
    "task.succeeded",
    "task.failed",
    "task.cancelled",
    "task.uncertain",
];

/// Most runs a task gets, counting the first. A read-only task that keeps ending `uncertain`
/// (the engine dies while it runs) stops being retried after this many and stays `uncertain`
/// for a person to cancel, so a task that crashes the engine cannot loop across restarts.
pub const MAX_ATTEMPTS: i64 = 3;

/// How long a finished task (and its lifecycle events) stays in the journal.
const FINISHED_RETENTION: std::time::Duration = std::time::Duration::from_secs(30 * 24 * 3600);
/// Least time between two retention sweeps of finished tasks (the sweep scans every terminal row).
const FINISHED_SWEEP_EVERY_MS: i64 = 3_600_000;
/// When the last retention sweep ran, in `now_ms` time; 0 before the first.
static LAST_FINISHED_SWEEP_MS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// Events one publish pass reads.
const PUBLISH_BATCH: i64 = 256;
/// Events one `task.events` reply carries.
const EVENTS_REPLY: i64 = 200;

pub struct Journal {
    store: Arc<JobStore>,
    /// Serialises every read-decide-write sequence on the journal (submit, lease, finish,
    /// cancel, recovery). The store's own transactions are atomic; this keeps a decision made on
    /// a read from going stale before its write.
    gate: Mutex<()>,
    epoch: u64,
    local_device: Option<String>,
    host: Arc<dyn JournalHost>,
    exec: Executor,
    closing: AtomicBool,
    publish: Mutex<()>,
    /// The device-link request the acceptance in progress belongs to (see `link`).
    pending_link: link::PendingLink,
    /// Worker wake-ups, one per lane (`worker::Lane::index`): set by submit and cancel. Separate
    /// flags so one lane's worker cannot consume the other's wake.
    wake: [(Mutex<bool>, Condvar); 4],
}

/// The transition `to` with its reason (null when none). Always sets `note`, so the hook never
/// reads a reason left by an earlier transition.
fn tr(to: TaskStatus, note: Option<&str>) -> Transition {
    Transition::to(store_name(to)).set("note", note.map_or(Value::Null, Value::from))
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Where a running task stands, as its executor sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    Running,
    /// `waiting-user`: the executor asked the owner and is waiting for the answer.
    Waiting,
    /// The run must stop acting: a cancel was requested, another holder fenced the task, it is
    /// terminal, or the engine is stopping.
    Stopped,
}

/// The handle a running task's executor holds on the journal (see `TaskInput::ctl`). Every move it
/// makes carries the generation the worker holds, so a task that was cancelled or taken over
/// rejects it (`stale_attempt`) and the executor learns it must stop.
pub struct RunCtl<'a> {
    journal: &'a Journal,
    id: &'a str,
    generation: i64,
}

impl RunCtl<'_> {
    pub fn task_id(&self) -> &str {
        self.id
    }

    /// The task's state now, read from the journal. `Stopped` also when the engine is stopping or
    /// the store cannot be read: an executor that cannot see its task must not keep acting.
    pub fn state(&self) -> RunState {
        if self.journal.closing.load(Ordering::SeqCst) {
            return RunState::Stopped;
        }
        let Ok(generation) = self.journal.store.generation(self.id) else {
            return RunState::Stopped;
        };
        if generation != self.generation {
            return RunState::Stopped;
        }
        match self
            .journal
            .load(self.id)
            .and_then(|rec| Journal::status_of(&rec))
        {
            Ok(TaskStatus::Running) => RunState::Running,
            Ok(TaskStatus::WaitingUser) => RunState::Waiting,
            _ => RunState::Stopped,
        }
    }

    /// `running` to `waiting-user`, with the reason as the event's note. `false`: the move was
    /// refused (a cancel, a takeover, the engine stopping), and the executor must stop.
    pub fn wait_for_user(&self, note: &str) -> bool {
        self.journal
            .ctl_move(self.id, self.generation, TaskStatus::WaitingUser, note)
    }

    /// The lease heartbeat (AG-8): records that the executor is alive, as a same-status move with a note, which fires no hook and
    /// makes no event. It moves nothing else: a task that is `waiting-user` or `waiting-device` is left as it is (a run that waits for
    /// the owner has nothing to beat and must not be resumed by it). `false`: the task is no longer this run's (a cancel, a takeover,
    /// the engine stopping), and the executor must stop.
    pub fn beat(&self) -> bool {
        self.journal.ctl_beat(self.id, self.generation)
    }

    /// `waiting-user` back to `running`. `false` as for `wait_for_user`.
    pub fn resume(&self) -> bool {
        self.journal.ctl_move(
            self.id,
            self.generation,
            TaskStatus::Running,
            "the owner answered",
        )
    }
}

impl Journal {
    /// Opens (creating) the journal at `path`, checks its schema version, takes the next owner
    /// epoch, applies the restart rules above, and publishes events an earlier process
    /// committed but had not published. The worker is started separately (`JournalService`).
    ///
    /// A corrupt or unreadable database is an error, never quarantined: the journal is the
    /// evidence of what may already have happened.
    pub fn open(
        path: &Path,
        host: Arc<dyn JournalHost>,
        exec: Executor,
        local_device: Option<String>,
    ) -> JobResult<Arc<Journal>> {
        let pending_link: link::PendingLink = Arc::default();
        // Before the store opens the file: opening creates and alters tables.
        schema::refuse_newer(path)?;
        let store = Arc::new(JobStore::open_configured(
            path,
            schema::store_config(pending_link.clone(), host.clone()),
        )?);
        store.read(|conn| schema::migrate_journal(path, conn))?;
        let epoch = store.write(schema::bump_epoch)?;
        let journal = Arc::new(Journal {
            store,
            gate: Mutex::new(()),
            epoch,
            local_device,
            host,
            exec,
            closing: AtomicBool::new(false),
            publish: Mutex::new(()),
            pending_link,
            wake: [
                (Mutex::new(false), Condvar::new()),
                (Mutex::new(false), Condvar::new()),
                (Mutex::new(false), Condvar::new()),
                (Mutex::new(false), Condvar::new()),
            ],
        });
        journal.recover()?;
        journal.prune_link_requests();
        journal.publish_pending();
        journal.log(
            LogLevel::Info,
            "journal.open",
            json!({ "schema_version": JOURNAL_SCHEMA_VERSION, "owner_epoch": epoch }),
        );
        Ok(journal)
    }

    pub fn owner_epoch(&self) -> u64 {
        self.epoch
    }

    fn log(&self, level: LogLevel, event: &str, fields: Value) {
        self.host.log(level, event, fields);
    }

    fn facts(&self, kind: &str) -> Option<KindFacts> {
        self.host.kind(kind)
    }

    fn is_read_only(&self, kind: &str) -> bool {
        self.facts(kind).is_some_and(|k| k.read_only)
    }

    /// A lost run of this kind cannot have had an effect (read-only kinds included).
    fn is_effect_free(&self, kind: &str) -> bool {
        self.facts(kind).is_some_and(|k| k.effect_free)
    }

    /// A lost run of this kind is settled `failed` (`interrupted`) instead of left `uncertain`:
    /// effect-free and not retried.
    fn settles_when_lost(&self, kind: &str) -> bool {
        self.facts(kind)
            .is_some_and(|k| k.effect_free && !k.read_only)
    }

    /// The kind keeps an audit that is its receipt query (`KindFacts::audited`).
    fn is_audited(&self, kind: &str) -> bool {
        self.facts(kind).is_some_and(|k| k.audited)
    }

    /// The task is bound to another computer (`KindFacts::needs_device`).
    fn needs_device(&self, kind: &str) -> bool {
        self.facts(kind).is_some_and(|k| k.needs_device)
    }

    /// The lane a kind runs on; a kind the host does not name runs on the general lane.
    fn lane_of(&self, kind: &str) -> Lane {
        self.facts(kind).map_or(Lane::General, |k| k.lane)
    }

    /// Read-only: one `pulse_link_requests` row (the product's replay store looks requests up here).
    pub fn link_row(&self, request_id: &str) -> JobResult<Option<LinkRecord>> {
        self.store.read(|conn| link::read_row(conn, request_id))
    }

    fn load(&self, id: &str) -> JobResult<JobRecord> {
        self.store.record(id).map_err(|e| {
            if e.code == "job_not_found" {
                JobError::new("task_not_found", format!("no such task: {id}"))
            } else {
                e
            }
        })
    }

    fn status_of(rec: &JobRecord) -> JobResult<TaskStatus> {
        parse_status(&rec.job.status).ok_or_else(|| {
            JobError::new(
                "journal_schema_newer",
                format!(
                    "task {} has a status this engine does not know",
                    rec.job.job_id
                ),
            )
        })
    }

    /// A write at a fresh generation (`bump`: supersede the current holder first) or at the
    /// current one. Callers hold the gate. `Ok(None)`: the task is already terminal. Returns the
    /// generation the write carried.
    fn fenced(&self, id: &str, t: Transition, bump: bool) -> JobResult<Option<(JobRecord, i64)>> {
        let mut generation = self.store.generation(id)?;
        if bump {
            generation = self.store.fence(id, generation)?;
        }
        Ok(self
            .store
            .transition_fenced(id, generation, &t)?
            .map(|record| (record, generation)))
    }

    /// A move an executor asks for while it runs (`RunCtl`): at the generation it holds, under the
    /// gate, refused while the journal is closing. The event is published after the commit.
    /// `RunCtl::beat`: under the gate, a `running` task of this generation gets the heartbeat note; a waiting one is left alone (and
    /// reported as still the run's); anything else is not this run's any more.
    fn ctl_beat(&self, id: &str, generation: i64) -> bool {
        let moved = {
            let _gate = lock(&self.gate);
            if self.closing.load(Ordering::SeqCst) {
                false
            } else {
                match self.load(id).and_then(|rec| Self::status_of(&rec)) {
                    Ok(TaskStatus::Running) => matches!(
                        self.store.transition_fenced(
                            id,
                            generation,
                            &tr(
                                TaskStatus::Running,
                                Some(&format!("heartbeat {}", now_ms()))
                            ),
                        ),
                        Ok(Some(_))
                    ),
                    Ok(TaskStatus::WaitingUser | TaskStatus::WaitingDevice) => true,
                    _ => false,
                }
            }
        };
        if moved {
            self.publish_pending();
        }
        moved
    }

    fn ctl_move(&self, id: &str, generation: i64, to: TaskStatus, note: &str) -> bool {
        let moved = {
            let _gate = lock(&self.gate);
            !self.closing.load(Ordering::SeqCst)
                && matches!(
                    self.store
                        .transition_fenced(id, generation, &tr(to, Some(note))),
                    Ok(Some(_))
                )
        };
        if moved {
            self.publish_pending();
        }
        moved
    }

    // ---- ACCEPTANCE ------------------------------------------------------------------------

    /// `task.submit`. `body` is a C0 `TaskEnvelope` (schema version 2, whose `kind` is the job kind,
    /// required) plus one member the envelope has no field for: `params` (an object, default `{}`).
    pub fn submit(&self, body: &Value) -> JobResult<Value> {
        match self.submit_inner(body, None)? {
            LinkSubmit::Accepted(receipt) => Ok(receipt),
            // Only a link submit produces these.
            LinkSubmit::Duplicate { .. } | LinkSubmit::Mismatch => Err(JobError::new(
                "idempotency_conflict",
                "unexpected replay outcome for a local submit",
            )),
        }
    }

    /// `task.submit` for a device-link request the authorizer judged fresh
    /// (a `LinkEntry`). `body` is built by the caller from the authorized
    /// request (its envelope's origin is the paired-device principal). The replay row commits
    /// in the same transaction as the task. A redelivery, or a delivery that loses the race to
    /// another, returns [`LinkSubmit::Duplicate`] with the first one's outcome; the same id
    /// with different content returns [`LinkSubmit::Mismatch`].
    pub fn submit_linked(&self, body: &Value, entry: &LinkEntry) -> JobResult<LinkSubmit> {
        self.submit_inner(body, Some(entry))
    }

    /// Records a request refused after authorization (for example a missing owner approval), so
    /// a redelivery gets the same refusal. `reason` is the stable code, never content.
    pub fn record_linked_refusal(&self, entry: &LinkEntry, reason: &str) -> JobResult<LinkSubmit> {
        let _gate = lock(&self.gate);
        if self
            .store
            .write(|conn| link::insert_refused(conn, entry, reason))?
        {
            return Ok(LinkSubmit::Duplicate {
                outcome: LinkOutcome::Refused {
                    reason: reason.to_string(),
                },
                receipt: None,
            });
        }
        self.link_resolved(entry, None)?
            .ok_or_else(|| JobError::new("journal_unavailable", "the link request row vanished"))
    }

    /// Drops link request rows past their expiry plus the clock-skew margin. Best effort; run at
    /// open and by the general worker lane's idle tick.
    pub(crate) fn prune_link_requests(&self) {
        let margin = self.host.link_prune_margin_ms();
        match self.store.write(|conn| link::prune(conn, now_ms(), margin)) {
            Ok(0) => {}
            Ok(count) => self.log(
                LogLevel::Info,
                "journal.link_requests_pruned",
                json!({ "count": count }),
            ),
            Err(error) => self.log(
                LogLevel::Warn,
                "journal.link_prune_failed",
                json!({ "code": error.code }),
            ),
        }
        self.prune_finished_tasks();
    }

    /// Retention for finished tasks: terminal `jobs` rows older than `FINISHED_RETENTION` leave with
    /// the store's own event, artifact and submission rows (`JobStore::prune`, which never touches
    /// an active task), then the `pulse_task_events` rows of tasks that no longer exist go, but only
    /// those the publisher has already sent (`seq` at or below the persisted cursor). Best effort.
    pub(crate) fn prune_finished_tasks(&self) {
        let now = now_ms();
        let last = LAST_FINISHED_SWEEP_MS.load(Ordering::SeqCst);
        if now.saturating_sub(last) < FINISHED_SWEEP_EVERY_MS
            || LAST_FINISHED_SWEEP_MS
                .compare_exchange(last, now, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
        {
            return;
        }
        let policy = PrunePolicy::new().older_than(FINISHED_RETENTION);
        let jobs = match self.store.prune(&policy) {
            Ok(report) => report.jobs.len(),
            Err(error) => {
                self.log(
                    LogLevel::Warn,
                    "journal.task_prune_failed",
                    json!({ "code": error.code }),
                );
                return;
            }
        };
        let events = self.store.write(|conn| {
            let cursor: i64 = meta_get(conn, META_PUBLISHED)?
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            Ok(conn.execute(
                "DELETE FROM pulse_task_events WHERE seq<=?1 AND task_id NOT IN (SELECT job_id FROM jobs)",
                [cursor],
            )?)
        });
        match events {
            Ok(events) if jobs > 0 || events > 0 => self.log(
                LogLevel::Info,
                "journal.tasks_pruned",
                json!({ "jobs": jobs, "events": events }),
            ),
            Ok(_) => {}
            Err(error) => self.log(
                LogLevel::Warn,
                "journal.event_prune_failed",
                json!({ "code": error.code }),
            ),
        }
    }

    /// What a stored row says about `entry`: the original outcome (with the task's receipt when
    /// accepted) for the same content, a mismatch otherwise. `None` when the id has no row.
    /// `envelope` builds the receipt when the caller has parsed it.
    fn link_resolved(
        &self,
        entry: &LinkEntry,
        envelope: Option<&TaskEnvelope>,
    ) -> JobResult<Option<LinkSubmit>> {
        let Some(stored) = self.link_row(&entry.request_id)? else {
            return Ok(None);
        };
        if stored.entry.body_sha256 != entry.body_sha256 {
            return Ok(Some(LinkSubmit::Mismatch));
        }
        let receipt = match (&stored.outcome, envelope) {
            (LinkOutcome::Accepted { task_id }, Some(envelope)) => {
                let rec = self.load(task_id)?;
                Some(self.receipt(&rec, true, envelope))
            }
            _ => None,
        };
        Ok(Some(LinkSubmit::Duplicate {
            outcome: stored.outcome,
            receipt,
        }))
    }

    fn submit_inner(&self, body: &Value, link: Option<&LinkEntry>) -> JobResult<LinkSubmit> {
        let mut envelope_value = body.clone();
        if let Value::Object(map) = &mut envelope_value {
            map.remove("params");
        }
        let envelope: TaskEnvelope = serde_json::from_value(envelope_value)
            .map_err(|e| JobError::bad_input(format!("invalid task envelope: {e}")))?;
        // The envelope's own `kind` is the job kind. An empty one names none (a voice hand-off
        // carries an empty kind until the submitter supplies one).
        if envelope.kind.is_empty() {
            return Err(JobError::bad_input("missing param `kind`"));
        }
        let kind = envelope.kind.clone();
        if self.facts(&kind).is_none() {
            return Err(JobError::new(
                "unsupported_kind",
                format!("this engine runs no task kind {kind}"),
            ));
        }
        let params = match body.get("params") {
            None | Some(Value::Null) => json!({}),
            Some(p @ Value::Object(_)) => p.clone(),
            Some(_) => return Err(JobError::bad_input("`params` must be an object")),
        };
        let (generation, deadline) = validate(&envelope)?;
        let request = json!({
            "kind": kind,
            "params": params,
            "envelope": serde_json::to_value(&envelope)
                .map_err(|e| JobError::bad_input(e.to_string()))?,
        });

        let gate = lock(&self.gate);

        // A link request id seen before gets its original outcome, before anything else is
        // judged (admission, deadline and the kind's checks may have changed since).
        if let Some(entry) = link {
            if let Some(done) = self.link_resolved(entry, Some(&envelope))? {
                return Ok(done);
            }
        }

        // A retry resolves to the original task before anything else is judged: the same
        // idempotency key, or the same utterance and generation under any key. It is answered
        // even while admission is closed or after the deadline has passed. A different request
        // under a known key or utterance is a conflict, never a new task.
        let by_key = self.find_by_key(&envelope.idempotency_key)?;
        let existing = match by_key {
            Some(id) => Some(id),
            None => self.find_by_utterance(&envelope.utterance.utterance_id, generation)?,
        };
        if let Some(id) = existing {
            let rec = self.load(&id)?;
            if same_work(&rec, &request) {
                let receipt = self.receipt(&rec, true, &envelope);
                if let Some(entry) = link {
                    // The idempotency key already resolved to a task: remember this request id
                    // against it. No task is created, so this is its own write.
                    let task_id = rec.job.job_id.clone();
                    if !self
                        .store
                        .write(|conn| link::insert_accepted_existing(conn, entry, &task_id))?
                    {
                        if let Some(done) = self.link_resolved(entry, Some(&envelope))? {
                            return Ok(done);
                        }
                    }
                    return Ok(LinkSubmit::Duplicate {
                        outcome: LinkOutcome::Accepted { task_id },
                        receipt: Some(receipt),
                    });
                }
                return Ok(LinkSubmit::Accepted(receipt));
            }
            return Err(JobError::new(
                "idempotency_conflict",
                "this idempotency key or utterance already belongs to a different request",
            ));
        }

        if self.closing.load(Ordering::SeqCst) {
            return Err(JobError::new(
                "admission_closed",
                "the engine is shutting down and admits no new tasks",
            ));
        }
        if !self.admission_open()? {
            return Err(JobError::new(
                "admission_closed",
                "admission is closed (drain); the task was not accepted",
            ));
        }
        if deadline.is_some_and(|d| d <= now_ms()) {
            return Err(JobError::new(
                "deadline_passed",
                "deadline_ms is already past",
            ));
        }

        // Checks of this kind alone (the agent switch, params, origin, device). A retry was
        // answered above, so a switch turned off later does not hide an accepted task.
        self.host
            .check_submit(&kind, &params, &envelope, self.local_device.as_deref())?;
        // At most the host's `queue_limit` tasks of one kind from one origin queued or running. Counted under
        // the gate, after a retry was answered above, so a resend of a queued task is never refused by its own count.
        if let Some(limit) = self.host.queue_limit(&kind) {
            if self.live_of(&kind, &envelope.origin.subject)? >= limit {
                return Err(self.host.queue_full(&kind));
            }
        }

        let mut ext = Ext::new();
        ext.insert("owner_epoch".into(), json!(0));
        ext.insert(
            "utterance_id".into(),
            json!(envelope.utterance.utterance_id),
        );
        ext.insert("utterance_generation".into(), json!(generation));
        ext.insert("device_id".into(), json!(envelope.target_device.device_id));
        ext.insert(
            "deadline_ms".into(),
            deadline.map_or(Value::Null, Value::from),
        );
        ext.insert("note".into(), Value::Null);
        let new = NewJob {
            request_id: Some(envelope.idempotency_key.clone()),
            kind: kind.clone(),
            request: request.clone(),
        };
        // ACCEPTANCE: this returns after the row and its `task.accepted` event are committed.
        // For a link request the hook inserts the replay row in this same transaction.
        *lock(&self.pending_link) = link.cloned();
        let submitted = self.store.submit_with(&new, &ext, &SubmitOptions::new());
        *lock(&self.pending_link) = None;
        let submitted = match submitted {
            Ok(done) => done,
            Err(error) if error.code == link::CONFLICT => {
                // Another delivery committed this request id first; the acceptance rolled back.
                return match link {
                    Some(entry) => self.link_resolved(entry, Some(&envelope))?.ok_or(error),
                    None => Err(error),
                };
            }
            Err(error) => return Err(error),
        };
        if submitted.deduplicated && !same_work(&submitted.record, &request) {
            return Err(JobError::new(
                "idempotency_conflict",
                "this idempotency key already belongs to a different request",
            ));
        }
        let receipt = self.receipt(&submitted.record, submitted.deduplicated, &envelope);
        drop(gate);
        self.publish_pending();
        self.wake();
        Ok(LinkSubmit::Accepted(receipt))
    }

    /// How many tasks of `kind` are queued or running whose origin is `origin_subject` (the principal's
    /// subject, set by the product and never from the caller's JSON). A task held `uncertain`
    /// is not counted: it is not running and waits for the owner.
    fn live_of(&self, kind: &str, origin_subject: &str) -> JobResult<usize> {
        let live = [
            TaskStatus::Accepted,
            TaskStatus::Leased,
            TaskStatus::Running,
            TaskStatus::WaitingUser,
            TaskStatus::WaitingDevice,
            TaskStatus::CancelRequested,
        ]
        .map(|s| format!("'{}'", store_name(s)))
        .join(",");
        let sql = format!("SELECT request_json FROM jobs WHERE kind=?1 AND status IN ({live})");
        let requests = self.store.read(|conn| {
            let mut st = conn.prepare(&sql)?;
            let rows = st
                .query_map([kind], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })?;
        Ok(requests
            .iter()
            .filter_map(|text| serde_json::from_str::<Value>(text).ok())
            .filter_map(|request| {
                serde_json::from_value::<TaskEnvelope>(request["envelope"].clone()).ok()
            })
            .filter(|envelope| envelope.origin.subject == origin_subject)
            .count())
    }

    fn receipt(&self, rec: &JobRecord, deduplicated: bool, envelope: &TaskEnvelope) -> Value {
        let verified = self
            .local_device
            .as_ref()
            .map(|local| *local == envelope.target_device.device_id);
        json!({
            "receipt": "accepted",
            "task_id": rec.job.job_id,
            "idempotency_key": rec.job.request_id,
            "task_status": Self::status_of(rec).map_or("unknown", pulse_name),
            "deduplicated": deduplicated,
            "journal_epoch": self.epoch,
            // Whether the named device is this computer. `null`: this engine does not know its
            // own device id yet (pairing is the slice 3b gate), so the task runs here unchecked.
            "target_device_verified": verified,
        })
    }

    fn find_by_key(&self, key: &str) -> JobResult<Option<String>> {
        self.store.read(|conn| {
            Ok(conn
                .query_row("SELECT job_id FROM jobs WHERE request_id=?1", [key], |r| {
                    r.get::<_, String>(0)
                })
                .optional()?)
        })
    }

    /// The task a submitter's idempotency key resolved to, if any: the same lookup `task.submit` dedupes on. A phone's
    /// `ask.cancel {clientMsgId}` names its question this way (`phone_assistant.rs`).
    pub fn task_for_key(&self, key: &str) -> JobResult<Option<String>> {
        self.find_by_key(key)
    }

    fn find_by_utterance(&self, utterance_id: &str, generation: i64) -> JobResult<Option<String>> {
        self.store.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT job_id FROM jobs WHERE utterance_id=?1 AND utterance_generation=?2 \
                     ORDER BY created_at, rowid LIMIT 1",
                    rightkit_job_queue::rusqlite::params![utterance_id, generation],
                    |r| r.get::<_, String>(0),
                )
                .optional()?)
        })
    }

    fn admission_open(&self) -> JobResult<bool> {
        self.store
            .read(|conn| Ok(meta_get(conn, META_ADMISSION)?.as_deref() != Some("closed")))
    }

    /// `task.admission`: opens or closes admission (the drain switch a cutover uses before it
    /// replaces the engine or the host). It is persisted, so a restart keeps it. Closing
    /// refuses new tasks and leaves accepted ones running; it never interrupts one.
    pub fn set_admission(&self, open: bool) -> JobResult<Value> {
        self.store
            .write(|conn| meta_set(conn, META_ADMISSION, if open { "open" } else { "closed" }))?;
        self.log(LogLevel::Info, "task.admission", json!({ "open": open }));
        if open {
            self.wake();
        }
        self.info()
    }

    // ---- reads -----------------------------------------------------------------------------

    fn view(&self, rec: &JobRecord) -> JobResult<Value> {
        let status = Self::status_of(rec)?;
        let id = &rec.job.job_id;
        let cursor = self.store.read(|conn| {
            Ok(conn.query_row(
                "SELECT COALESCE(MAX(seq), 0) FROM pulse_task_events WHERE task_id=?1",
                [id],
                |r| r.get::<_, i64>(0),
            )?)
        })?;
        Ok(json!({
            "task_id": id,
            "kind": rec.job.kind,
            "status": pulse_name(status),
            "terminal": status.is_terminal(),
            "owner_epoch": ext_i64(rec, "owner_epoch"),
            "attempts": rec.job.attempts,
            "idempotency_key": rec.job.request_id,
            "target_device": rec.ext.get("device_id").cloned().unwrap_or(Value::Null),
            "deadline_ms": rec.ext.get("deadline_ms").cloned().unwrap_or(Value::Null),
            "error": rec.job.error,
            "created_at": rec.job.created_at,
            "updated_at": rec.job.updated_at,
            "event_cursor": cursor,
        }))
    }

    /// `task.status`.
    pub fn status(&self, id: &str) -> JobResult<Value> {
        self.view(&self.load(id)?)
    }

    /// `task.result`: the status, and the result once the task is `succeeded`. Any other
    /// status answers `finished: false` (or `true` with the error, for `failed` and
    /// `cancelled`) and `result: null`.
    pub fn result(&self, id: &str) -> JobResult<Value> {
        let rec = self.load(id)?;
        let status = Self::status_of(&rec)?;
        let result = if status == TaskStatus::Succeeded {
            rec.job.result.clone()
        } else {
            None
        };
        Ok(json!({
            "task_id": id,
            "status": pulse_name(status),
            "finished": status.is_terminal(),
            "result": result,
            "error": rec.job.error,
        }))
    }

    /// `task.events`: the task's lifecycle events with `seq` greater than `after`, oldest first,
    /// and `next`, the cursor to pass on the next call.
    pub fn events(&self, id: &str, after: i64) -> JobResult<Value> {
        self.load(id)?;
        let rows = self.store.read(|conn| {
            let mut st = conn.prepare(
                "SELECT seq, topic, at_ms, status, owner_epoch, attempt, detail \
                 FROM pulse_task_events WHERE task_id=?1 AND seq>?2 ORDER BY seq LIMIT ?3",
            )?;
            let rows = st
                .query_map(
                    rightkit_job_queue::rusqlite::params![id, after, EVENTS_REPLY],
                    event_row,
                )?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })?;
        let next = rows
            .last()
            .and_then(|e| e.get("seq").and_then(Value::as_i64))
            .unwrap_or(after);
        Ok(json!({ "task_id": id, "events": rows, "next": next }))
    }

    /// `task.journal`: what a cutover records and checks: the journal schema version, this
    /// process's owner epoch, the admission switch, and how many tasks sit in each status.
    pub fn info(&self) -> JobResult<Value> {
        let (admission, counts) = self.store.read(|conn| {
            let admission = meta_get(conn, META_ADMISSION)?.unwrap_or_else(|| "open".into());
            let mut st = conn.prepare("SELECT status, COUNT(*) FROM jobs GROUP BY status")?;
            let mut counts = serde_json::Map::new();
            let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            for row in rows {
                let (status, n) = row?;
                let name =
                    parse_status(&status).map_or(status.clone(), |s| pulse_name(s).to_string());
                counts.insert(name, json!(n));
            }
            Ok((admission, counts))
        })?;
        let in_flight: i64 = [
            "accepted",
            "leased",
            "running",
            "waiting-user",
            "waiting-device",
            "cancel-requested",
        ]
        .iter()
        .map(|s| counts.get(*s).and_then(Value::as_i64).unwrap_or(0))
        .sum();
        Ok(json!({
            "schema_version": JOURNAL_SCHEMA_VERSION,
            "owner_epoch": self.epoch,
            "admission": admission,
            "closing": self.closing.load(Ordering::SeqCst),
            "counts": counts,
            "in_flight": in_flight,
            "kinds": self.host.kinds().iter().map(|k| json!({ "name": k.name, "read_only": k.read_only, "effect_free": k.effect_free })).collect::<Vec<_>>(),
        }))
    }

    // ---- cancel ----------------------------------------------------------------------------

    /// `task.cancel`. Before any effect can have started (`accepted`, `leased`, `waiting-*`) the
    /// task becomes `cancelled` at once and the holder is fenced. A `running` task becomes
    /// `cancel-requested`, which is not terminal: the worker ends it `cancelled` if the request
    /// is still pending when the work finishes. An `uncertain` task of an effect-free kind
    /// (read-only, or `agent.request`) is `cancelled`; a kind that is neither goes to
    /// `cancel-requested`, which waits for a receipt query. A terminal task, or one already
    /// `cancel-requested`, is returned as it is. Cancel is not undo.
    pub fn cancel(&self, id: &str) -> JobResult<Value> {
        {
            let _gate = lock(&self.gate);
            let rec = self.load(id)?;
            match Self::status_of(&rec)? {
                s if s.is_terminal() => {}
                TaskStatus::CancelRequested => {}
                TaskStatus::Running => {
                    self.store.transition(
                        id,
                        &tr(TaskStatus::CancelRequested, Some("cancel requested")),
                    )?;
                }
                // An audited kind's lost run: nothing can reach a tool any more, so the owner's
                // cancel is final. It does not undo what the audit lists.
                TaskStatus::Uncertain if self.is_audited(&rec.job.kind) => {
                    self.fenced(
                        id,
                        tr(
                            TaskStatus::Cancelled,
                            Some(
                                "cancelled; effects of the lost run are not undone (see the audit)",
                            ),
                        )
                        .error(None),
                        true,
                    )?;
                }
                // Waiting for the owner: the holder is fenced, so its next move fails and it
                // stops. Earlier calls of the run, if any, stay in the audit.
                TaskStatus::WaitingUser if self.is_audited(&rec.job.kind) => {
                    self.fenced(
                        id,
                        tr(
                            TaskStatus::Cancelled,
                            Some("cancelled while waiting for the owner; earlier calls are in the audit"),
                        )
                        .error(None),
                        true,
                    )?;
                }
                TaskStatus::Uncertain if !self.is_effect_free(&rec.job.kind) => {
                    self.store.transition(
                        id,
                        &tr(TaskStatus::CancelRequested, Some("cancel requested")),
                    )?;
                }
                TaskStatus::Accepted
                | TaskStatus::Leased
                | TaskStatus::WaitingUser
                | TaskStatus::WaitingDevice
                | TaskStatus::Uncertain => {
                    self.fenced(
                        id,
                        tr(TaskStatus::Cancelled, Some("cancelled before any effect")).error(None),
                        true,
                    )?;
                }
                TaskStatus::Succeeded | TaskStatus::Failed | TaskStatus::Cancelled => {}
            }
        }
        self.publish_pending();
        self.wake();
        self.status(id)
    }

    // ---- restart ---------------------------------------------------------------------------

    fn recover(&self) -> JobResult<()> {
        let _gate = lock(&self.gate);
        for id in self.store.non_terminal_ids()? {
            let result = (|| -> JobResult<()> {
                let rec = self.load(&id)?;
                if ext_i64(&rec, "owner_epoch") >= i64::try_from(self.epoch).unwrap_or(i64::MAX) {
                    return Ok(());
                }
                match Self::status_of(&rec)? {
                    TaskStatus::Running => {
                        self.lose_run(&id, &rec.job.kind, TaskStatus::Running, "owner epoch lost")?;
                    }
                    // An audited kind that was waiting for the owner lost its run the same way: its
                    // executor and its approval record (engine memory) are gone.
                    TaskStatus::WaitingUser if self.is_audited(&rec.job.kind) => {
                        self.lose_run(
                            &id,
                            &rec.job.kind,
                            TaskStatus::WaitingUser,
                            "owner epoch lost",
                        )?;
                    }
                    // A non-retried kind found `uncertain` (a settle that did not commit before
                    // the last stop) is settled the same way when its receipt shows no effect.
                    TaskStatus::Uncertain if self.host.lost_run_is_clean(&rec.job.kind, &id) => {
                        self.settle_interrupted(&id)?;
                    }
                    // Nothing can be in flight: an effect-free kind has no effects, and an audited
                    // kind's effects all pass a broker that died with the old engine.
                    TaskStatus::CancelRequested
                        if self.is_effect_free(&rec.job.kind) || self.is_audited(&rec.job.kind) =>
                    {
                        self.fenced(
                            &id,
                            tr(
                                TaskStatus::Cancelled,
                                Some(
                                    "cancel requested; owner epoch lost; nothing can run any more",
                                ),
                            )
                            .error(None),
                            true,
                        )?;
                    }
                    TaskStatus::CancelRequested => {
                        self.fenced(
                            &id,
                            tr(TaskStatus::Uncertain, Some("owner epoch lost")),
                            true,
                        )?;
                    }
                    // `leased`: no effect had started; the worker leases it again. Everything
                    // else waits as it is.
                    _ => {}
                }
                Ok(())
            })();
            if let Err(error) = result {
                self.log(
                    LogLevel::Error,
                    "journal.recover_failed",
                    json!({ "task_id": id, "code": error.code }),
                );
            }
        }
        Ok(())
    }

    /// A run whose executor is gone (an older epoch, or an engine that could not wait for it):
    /// `uncertain`, and `failed` (`interrupted`) at once when the kind's receipt shows nothing
    /// outside the journal can have changed. A `waiting-user` run goes through `running` first
    /// (the only edge the table has to `uncertain`). Callers hold the gate.
    fn lose_run(&self, id: &str, kind: &str, from: TaskStatus, note: &str) -> JobResult<()> {
        if from == TaskStatus::WaitingUser {
            self.fenced(
                id,
                tr(TaskStatus::Running, Some("approval wait abandoned")),
                true,
            )?;
        }
        self.fenced(id, tr(TaskStatus::Uncertain, Some(note)), true)?;
        if self.host.lost_run_is_clean(kind, id) {
            self.settle_interrupted(id)?;
        }
        Ok(())
    }

    /// The settle step of a non-retried kind whose receipt shows no effect (`agent.request`:
    /// none possible; `agent.act`: none in its audit): the lost run is `failed` (`interrupted`),
    /// not left `uncertain`. Callers hold the gate.
    fn settle_interrupted(&self, id: &str) -> JobResult<()> {
        self.fenced(
            id,
            tr(TaskStatus::Failed, Some("no external effect possible")).error(Some(json!({
                "code": "interrupted",
                "message": "the engine stopped while the request was running; ask again",
                "retryable": false,
            }))),
            true,
        )?;
        Ok(())
    }

    // ---- events out ------------------------------------------------------------------------

    /// Publishes event rows past the persisted cursor to `hub.jsonl` and the engine event bus,
    /// then advances the cursor. Best effort: a failure is logged and the next call retries.
    pub(crate) fn publish_pending(&self) {
        let _publishing = lock(&self.publish);
        loop {
            let batch = self.store.read(|conn| {
                let cursor: i64 = meta_get(conn, META_PUBLISHED)?
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let mut st = conn.prepare(
                    "SELECT seq, topic, at_ms, status, owner_epoch, attempt, detail, task_id \
                     FROM pulse_task_events WHERE seq>?1 ORDER BY seq LIMIT ?2",
                )?;
                let rows = st
                    .query_map(
                        rightkit_job_queue::rusqlite::params![cursor, PUBLISH_BATCH],
                        |r| {
                            let mut v = event_row(r)?;
                            v["task_id"] = json!(r.get::<_, String>(7)?);
                            Ok(v)
                        },
                    )?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            });
            let batch = match batch {
                Ok(b) => b,
                Err(error) => {
                    self.log(
                        LogLevel::Warn,
                        "journal.publish_failed",
                        json!({ "code": error.code }),
                    );
                    return;
                }
            };
            let Some(last) = batch
                .last()
                .and_then(|e| e.get("seq").and_then(Value::as_i64))
            else {
                return;
            };
            for event in &batch {
                let topic = event
                    .get("topic")
                    .and_then(Value::as_str)
                    .unwrap_or("task.unknown");
                self.log(LogLevel::Info, topic, event.clone());
                self.host.emit(topic, event.clone());
            }
            if let Err(error) = self
                .store
                .write(|conn| meta_set(conn, META_PUBLISHED, &last.to_string()))
            {
                self.log(
                    LogLevel::Warn,
                    "journal.publish_failed",
                    json!({ "code": error.code }),
                );
                return;
            }
        }
    }

    pub(crate) fn wake(&self) {
        for (flag, signal) in &self.wake {
            *lock(flag) = true;
            signal.notify_all();
        }
    }
}

/// One `pulse_task_events` row (`seq, topic, at_ms, status, owner_epoch, attempt, detail`) as
/// the JSON the commands and the event bus carry. The status is the C0 name.
fn event_row(
    r: &rightkit_job_queue::rusqlite::Row<'_>,
) -> rightkit_job_queue::rusqlite::Result<Value> {
    let status: String = r.get(3)?;
    let status = parse_status(&status).map_or(status, |s| pulse_name(s).to_string());
    let detail: Option<String> = r.get(6)?;
    Ok(json!({
        "seq": r.get::<_, i64>(0)?,
        "topic": r.get::<_, String>(1)?,
        "at_ms": r.get::<_, i64>(2)?,
        "status": status,
        "owner_epoch": r.get::<_, i64>(4)?,
        "attempt": r.get::<_, i64>(5)?,
        "detail": detail.and_then(|d| serde_json::from_str::<Value>(&d).ok()).unwrap_or(Value::Null),
    }))
}

/// Whether a stored task is the same request as `request` (kind, params and envelope, with the
/// envelope's engine-assigned fields at their submitted values). A submitter's `seal`
/// (`params.submitter.seal`) is left out: it is the engine process's HMAC, so a resend after
/// a restart carries a different one for the same request. Who submitted (`requester`) still counts,
/// so the same key from another submitter stays a conflict.
fn same_work(rec: &JobRecord, request: &Value) -> bool {
    fn unsealed(request: &Value) -> Value {
        let mut request = request.clone();
        if let Some(submitter) = request
            .get_mut("params")
            .and_then(|params| params.get_mut("submitter"))
            .and_then(Value::as_object_mut)
        {
            submitter.remove("seal");
        }
        request
    }
    unsealed(&rec.job.request) == unsealed(request)
}

/// Checks a submitted envelope. Returns the utterance generation and the deadline as the
/// integers the journal stores.
fn validate(envelope: &TaskEnvelope) -> JobResult<(i64, Option<i64>)> {
    let bad = |m: &str| JobError::bad_input(m.to_string());
    if envelope.schema_version != TASK_ENVELOPE_SCHEMA_VERSION {
        return Err(bad(&format!(
            "unsupported task envelope schema version {}",
            envelope.schema_version
        )));
    }
    if !envelope.task_id.is_empty() {
        return Err(bad(
            "task_id must be empty: the engine mints it at ACCEPTANCE",
        ));
    }
    if envelope.idempotency_key.trim().is_empty() {
        return Err(bad("idempotency_key is empty"));
    }
    if envelope.utterance.utterance_id.trim().is_empty() {
        return Err(bad("utterance.utterance_id is empty"));
    }
    if envelope.target_device.device_id.trim().is_empty() {
        return Err(bad("target_device.device_id is empty"));
    }
    if envelope.status != TaskStatus::Accepted {
        return Err(bad("status must be accepted on submission"));
    }
    if envelope.owner_epoch != 0 {
        return Err(bad(
            "owner_epoch must be 0 on submission: the engine assigns it at lease",
        ));
    }
    if envelope.checkpoint_ref.is_some() || !envelope.output_refs.is_empty() {
        return Err(bad(
            "checkpoint_ref and output_refs must be empty on submission",
        ));
    }
    if matches!(
        envelope.intent.source,
        IntentSource::Revoked { .. } | IntentSource::OutOfScope { .. }
    ) {
        return Err(JobError::new(
            "not_covered",
            "not covered by the relayed request",
        ));
    }
    let generation = i64::try_from(envelope.utterance.generation)
        .map_err(|_| bad("utterance.generation is out of range"))?;
    let deadline = match envelope.deadline_ms {
        None => None,
        Some(d) => Some(i64::try_from(d).map_err(|_| bad("deadline_ms is out of range"))?),
    };
    Ok((generation, deadline))
}
