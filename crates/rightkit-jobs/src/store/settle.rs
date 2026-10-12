//! Settle-later: a terminal write that fails on storage is kept, not lost.
//!
//! When the runner (or a handler, through [`crate::JobControl::settle`]) writes a job's terminal
//! state and the database refuses with a storage error (`db` or `io`: busy, I/O error, disk
//! full), the intended transition is kept in memory with the job's claim (its per-job lock)
//! still held, so no other runner in this process can start the job again. It is retried when
//! the store next completes a write, and on [`JobStore::flush_pending_settlements`]. There is no
//! timer and no sleep: only store activity and that explicit call drive a retry.
//!
//! Who wins. A held write is the runner's or the owner's. An explicit owner settle
//! ([`JobStore::settle_terminal`], [`crate::JobControl::settle`]) on a job whose held write is not
//! stored yet replaces it, extension values included; the runner's own write never replaces a
//! held one; once the terminal row is stored a conflicting settle is `illegal_transition`.
//!
//! Restart. A held transition is also written to a small journal file, one file per job, in
//! `<database file>.settlements/` next to the database, through `rightkit_fs`'s atomic write
//! (temp file, `fsync`, rename). The journal does not depend on the table that refused the write.
//! Opening the store reads the journal and writes each recorded transition, so a job whose
//! handler finished is settled the way the handler left it instead of being run again. A
//! transition that cannot be written yet stays pending, and a runner does not list or run such a
//! job (see [`crate::UnsettledRecovery`]). A journal file that cannot be read (a permission
//! error, a sharing violation while another process replaces it, an I/O error) is not judged bad:
//! it stays where it is, shows in [`JobStore::pending_settlements`] with an empty status and
//! the read error, and is read again by [`JobStore::flush_pending_settlements`]. Only a file that
//! was read and fails to parse or validate is set aside as `.json.bad`.
//!
//! Limits. The journal needs a writable directory next to the database: when that fails too
//! (disk full), the entry is memory-only ([`UnsettledJob::durable`] is false) and a crash loses
//! it. The set is bounded by [`MAX_PENDING_SETTLEMENTS`] entries and
//! [`MAX_PENDING_SETTLEMENT_BYTES`] bytes; past either bound the write is not held and its error
//! is returned, as before.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

use rightkit_db::write_txn;
use serde_json::{json, Map, Value};

use super::{
    add_event_on, check_fence, now, Event, Job, JobRecord, JobStore, Transition, CANCELLED,
    COMPLETE,
};
use crate::config::StatusModel;
use crate::error::{JobError, JobResult};
use crate::runner::{EventSink, JobLock};

/// Most terminal writes held in memory at once. A write that fails while the set is full is not
/// held: its error is returned.
pub const MAX_PENDING_SETTLEMENTS: usize = 64;
/// Most bytes (results, errors, extension values, ids) held at once, counted from their JSON.
pub const MAX_PENDING_SETTLEMENT_BYTES: usize = 4 * 1024 * 1024;

/// Retries of one entry that run immediately; later ones wait [`SLOW_RETRY`] between tries.
const FAST_RETRIES: u32 = 3;
const SLOW_RETRY: Duration = Duration::from_secs(1);

fn lk<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// A lifecycle event (`complete`, `failed`, ...) written in the same transaction as the terminal
/// state it announces.
#[derive(Debug, Clone)]
pub(crate) struct LifecycleEvent {
    pub(crate) stage: String,
    pub(crate) progress: Option<f64>,
    pub(crate) message: Option<String>,
}

impl LifecycleEvent {
    pub(crate) fn new(stage: &str, progress: Option<f64>, message: Option<&str>) -> Self {
        Self {
            stage: stage.into(),
            progress,
            message: message.map(Into::into),
        }
    }
}

/// One terminal write to make: now, or later if storage refuses it.
pub(crate) struct SettleRequest {
    pub(crate) job_id: String,
    /// Attempt generation the write is fenced to (`None`: unfenced).
    pub(crate) fence: Option<i64>,
    pub(crate) transition: Transition,
    pub(crate) event: Option<LifecycleEvent>,
    /// The job's per-job lock. Kept with the held write, dropped with it, or dropped at once
    /// when the write lands.
    pub(crate) claim: Option<JobLock>,
    /// Receives `event` if the write lands later than this call.
    pub(crate) sink: Option<EventSink>,
    /// Hold a write that fails on storage (otherwise return its error).
    pub(crate) defer: bool,
    /// An explicit settle by the job's owner (`JobStore::settle_terminal`, `JobControl::settle`),
    /// as opposed to the runner's own write. See [`JobStore::settle_terminal`] for the rule.
    pub(crate) owner: bool,
}

#[allow(clippy::large_enum_variant)]
pub(crate) enum Settle {
    /// Committed. `record` is `None` when the job was already terminal (nothing was written).
    Written {
        record: Option<JobRecord>,
        event: Option<Event>,
    },
    /// Storage refused the write; it is held and will be retried.
    Deferred,
}

/// Result of [`JobStore::settle_terminal`] and [`crate::JobControl::settle`].
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum Settlement {
    /// The terminal state is stored. `None`: the job was already terminal and nothing changed.
    Written(Option<JobRecord>),
    /// Storage refused the write (`db` or `io`). It is kept in memory and in the journal, with
    /// the job's claim held once the runner has it, and retried (see
    /// [`JobStore::flush_pending_settlements`]). The job reads as non-terminal until it lands.
    Deferred,
}

/// What a flush did.
#[derive(Debug, Clone, Default)]
pub struct FlushReport {
    /// Held writes that are no longer pending because the job's terminal state is stored: written
    /// by this flush, or already there.
    pub settled: usize,
    /// Held writes given up because they can never apply (the job is gone, a newer attempt
    /// superseded the write, a hook refused it), with the reason. They are removed from the
    /// journal.
    pub dropped: Vec<(String, JobError)>,
    /// Held writes still pending after the flush.
    pub pending: usize,
}

/// A job whose handler finished but whose terminal state is not stored yet.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct UnsettledJob {
    pub job_id: String,
    /// The terminal status the job will get: `complete`, `failed` or `cancelled`. Empty for a
    /// journal file that exists but could not be read at open: its outcome is unknown.
    pub status: String,
    /// The transition is in the journal file, so it survives a crash and is applied at the next
    /// open. False: memory only (the journal could not be written).
    pub durable: bool,
    /// A runner in this process holds the job's lock for it. False: the entry came from the
    /// journal at open.
    pub claimed: bool,
    /// Failed write attempts so far.
    pub attempts: u32,
    /// The last storage error.
    pub last_error: String,
}

/// What [`JobStore::shutdown_detailed`] (also on `Runner` and `StagedPool`) found.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct SettlementShutdown {
    /// Held writes that landed during the shutdown flush.
    pub settled: usize,
    /// Held writes given up (see [`FlushReport::dropped`]).
    pub dropped: Vec<(String, JobError)>,
    /// Jobs that finished but could not be stored: "finished, unsettled". Those with
    /// [`UnsettledJob::durable`] are applied at the next open; the others are lost with the
    /// process, and the next open sees them as unfinished.
    pub unsettled: Vec<UnsettledJob>,
}

impl SettlementShutdown {
    /// Nothing finished is left unstored.
    pub fn is_clean(&self) -> bool {
        self.unsettled.is_empty()
    }
}

struct Entry {
    job_id: String,
    generation: Option<i64>,
    transition: Transition,
    event: Option<LifecycleEvent>,
    claim: Option<JobLock>,
    sink: Option<EventSink>,
    last_error: String,
    attempts: u32,
    last_try: Instant,
    durable: bool,
    bytes: usize,
}

impl Entry {
    fn due(&self, force: bool) -> bool {
        force || self.attempts < FAST_RETRIES || self.last_try.elapsed() >= SLOW_RETRY
    }
}

/// Snapshot of an entry taken to write it without holding the pending lock.
struct Work {
    job_id: String,
    generation: Option<i64>,
    transition: Transition,
    event: Option<LifecycleEvent>,
}

pub(super) struct SettleState {
    pending: Mutex<Vec<Entry>>,
    /// `pending.len()`, readable without the lock: the check every write makes.
    len: AtomicUsize,
    /// Serialises flushes; an opportunistic flush skips when one is running.
    flush: Mutex<()>,
    /// Jobs whose runner is still announcing a terminal state: not prunable meanwhile.
    delivering: Mutex<HashMap<String, usize>>,
    /// `<database file>.settlements/`, or `None` for a database without a file name.
    dir: Option<PathBuf>,
    /// Journal files found at open that could not be read (not "unparseable": those are set
    /// aside). They stay where they are, are reported as unsettled, and are read again on an
    /// explicit flush.
    unread: Mutex<Vec<UnreadJournal>>,
}

/// A journal file that exists but could not be read.
struct UnreadJournal {
    path: PathBuf,
    error: String,
    /// Reads that failed so far.
    attempts: u32,
}

impl SettleState {
    pub(super) fn new(db: &Path) -> Self {
        let dir = db
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| !n.is_empty() && *n != ":memory:" && !n.starts_with("file:"))
            .map(|n| db.with_file_name(format!("{n}.settlements")));
        Self {
            pending: Mutex::new(Vec::new()),
            len: AtomicUsize::new(0),
            flush: Mutex::new(()),
            delivering: Mutex::new(HashMap::new()),
            dir,
            unread: Mutex::new(Vec::new()),
        }
    }
}

/// Keeps a job out of `prune` until dropped: its terminal state is written but its runner has
/// not finished announcing it.
pub(crate) struct DeliveryPin<'a> {
    store: &'a JobStore,
    id: String,
}

impl Drop for DeliveryPin<'_> {
    fn drop(&mut self) {
        let mut m = lk(&self.store.settle.delivering);
        if let Some(n) = m.get_mut(&self.id) {
            *n -= 1;
            if *n == 0 {
                m.remove(&self.id);
            }
        }
    }
}

/// The row as it will read once `t` lands: `base` with the status, error and result of `t`.
pub(crate) fn projected(base: &Job, t: &Transition) -> Job {
    let mut job = base.clone();
    if let Some(to) = &t.to {
        job.status = to.clone();
    }
    if let Some(e) = &t.error {
        job.error = e.clone();
    }
    if let Some(r) = &t.result {
        job.result = Some(r.clone());
    }
    job.updated_at = now();
    job
}

/// The event for an explicit settle that replaces a held write which had one.
fn owner_event(status: Option<&str>) -> LifecycleEvent {
    match status {
        Some(COMPLETE) => LifecycleEvent::new("complete", Some(1.0), Some("settled by owner")),
        Some(CANCELLED) => LifecycleEvent::new("cancelled", None, Some("settled by owner")),
        _ => LifecycleEvent::new("failed", None, Some("settled by owner")),
    }
}

/// A write worth holding: the database or the disk refused it.
pub(crate) fn is_storage_error(e: &JobError) -> bool {
    matches!(e.code.as_str(), "db" | "io")
}

/// File-name-safe job id (the rule `JobLocks` applies).
fn journal_safe(job_id: &str) -> bool {
    !job_id.is_empty() && !job_id.contains(['/', '\\', '.'])
}

fn entry_bytes(job_id: &str, t: &Transition, ev: &Option<LifecycleEvent>) -> usize {
    let mut n = 256 + job_id.len();
    if let Some(Some(e)) = &t.error {
        n += e.to_string().len();
    }
    if let Some(r) = &t.result {
        n += r.to_string().len();
    }
    n += Value::Object(t.ext.clone()).to_string().len();
    if let Some(e) = ev {
        n += e.stage.len() + e.message.as_ref().map_or(0, String::len);
    }
    n
}

fn entry_json(e: &Entry) -> Value {
    let t = &e.transition;
    let mut m = Map::new();
    m.insert("version".into(), json!(1));
    m.insert("job_id".into(), json!(e.job_id));
    m.insert("generation".into(), json!(e.generation));
    m.insert("to".into(), json!(t.to));
    if let Some(err) = &t.error {
        m.insert("error".into(), err.clone().unwrap_or(Value::Null));
    }
    if let Some(r) = &t.result {
        m.insert("result".into(), r.clone());
    }
    if !t.ext.is_empty() {
        m.insert("ext".into(), Value::Object(t.ext.clone()));
    }
    if let Some(ev) = &e.event {
        m.insert(
            "event".into(),
            json!({"stage": ev.stage, "progress": ev.progress, "message": ev.message}),
        );
    }
    m.insert("recorded_at".into(), json!(now()));
    Value::Object(m)
}

type Parsed = (String, Option<i64>, Transition, Option<LifecycleEvent>);

fn parse_entry(v: &Value) -> Option<Parsed> {
    let o = v.as_object()?;
    if o.get("version").and_then(Value::as_i64) != Some(1) {
        return None;
    }
    let job_id = o.get("job_id")?.as_str()?.to_string();
    if !journal_safe(&job_id) {
        return None;
    }
    let to = o.get("to")?.as_str()?.to_string();
    if !StatusModel::is_terminal(&to) {
        return None;
    }
    let mut t = Transition::to(to);
    t.error = match o.get("error") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(e) => Some(Some(e.clone())),
    };
    t.result = o.get("result").cloned();
    if let Some(Value::Object(x)) = o.get("ext") {
        t.ext = x.clone();
    }
    let event = o.get("event").and_then(|e| {
        Some(LifecycleEvent {
            stage: e.get("stage")?.as_str()?.to_string(),
            progress: e.get("progress").and_then(Value::as_f64),
            message: e.get("message").and_then(Value::as_str).map(String::from),
        })
    });
    Some((
        job_id,
        o.get("generation").and_then(Value::as_i64),
        t,
        event,
    ))
}

impl JobStore {
    /// Apply `t` to `job_id` (inside one write transaction with the hook), together with an
    /// optional lifecycle event. `Ok(None)` when the job is already terminal. The shared body of
    /// [`JobStore::transition`] and the settle-later path.
    pub(super) fn transition_event_in(
        &self,
        job_id: &str,
        fence: Option<i64>,
        t: &Transition,
        event: Option<&LifecycleEvent>,
    ) -> JobResult<Option<(JobRecord, Option<Event>)>> {
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
        let ev = match event {
            Some(e) => Some(add_event_on(
                &tx,
                job_id,
                None,
                &e.stage,
                None,
                e.progress,
                e.message.as_deref(),
            )?),
            None => None,
        };
        tx.commit()?;
        drop(conn);
        if after.job.is_terminal() {
            self.settle(job_id, after.job.status == CANCELLED);
        }
        Ok(Some((after, ev)))
    }

    /// Write the terminal transition `t` (`complete`, `failed` or `cancelled`) of `job_id`. If
    /// the database refuses it on storage (`db` or `io`), the transition is kept instead of
    /// lost and `Settlement::Deferred` is returned: it is retried when the store next completes a
    /// write and on [`JobStore::flush_pending_settlements`], and journaled so a restart applies
    /// it (see the module notes). Any other error is returned and nothing is kept.
    ///
    /// The rule when the job already has an outcome:
    /// - **Held, not yet stored** (the row is still non-terminal): this explicit settle
    ///   **replaces** the held write, whether the runner or an earlier settle made it. The new
    ///   transition is kept whole, extension values included, the journal file is rewritten, the
    ///   job's claim and sink stay, and a lifecycle event the held write carried is replaced by
    ///   one for the new status. The write is attempted at once; if storage accepts it the stale
    ///   held write is dropped. The runner's own write never replaces a held one.
    /// - **Already stored** (the row is terminal): nothing is written or merged. The same status
    ///   is idempotent (`Written(None)`); a different one is refused with `illegal_transition`.
    pub fn settle_terminal(&self, job_id: &str, t: &Transition) -> JobResult<Settlement> {
        self.settle_terminal_in(job_id, None, t)
    }

    /// [`JobStore::settle_terminal`] for the attempt holding `generation` (fenced).
    pub fn settle_terminal_fenced(
        &self,
        job_id: &str,
        generation: i64,
        t: &Transition,
    ) -> JobResult<Settlement> {
        self.settle_terminal_in(job_id, Some(generation), t)
    }

    fn settle_terminal_in(
        &self,
        job_id: &str,
        fence: Option<i64>,
        t: &Transition,
    ) -> JobResult<Settlement> {
        match t.to.as_deref() {
            Some(s) if StatusModel::is_terminal(s) => {}
            _ => {
                return Err(JobError::bad_input(
                    "a settlement moves to complete, failed or cancelled",
                ))
            }
        }
        let out = self.settle_request(SettleRequest {
            job_id: job_id.into(),
            fence,
            transition: t.clone(),
            event: None,
            claim: None,
            sink: None,
            defer: true,
            owner: true,
        })?;
        Ok(match out {
            Settle::Written { record, .. } => Settlement::Written(record),
            Settle::Deferred => Settlement::Deferred,
        })
    }

    /// The terminal write of the runner or of the job's owner: commit it, or hold it.
    pub(crate) fn settle_request(&self, req: SettleRequest) -> JobResult<Settle> {
        let SettleRequest {
            job_id,
            fence,
            transition,
            mut event,
            claim,
            sink,
            defer,
            owner,
        } = req;
        // A settle and a flush never interleave, and neither do two settles of one store. A
        // flush copies a held write, commits the copy and then removes the entry by job id; an
        // owner replacing the held outcome in between would be told `Deferred` while the flush
        // stored the replaced outcome and then removed the owner's entry and its journal file.
        // The runner takes the same lock: otherwise an owner whose write storage refused could
        // hold its outcome between the runner's look at the held writes and the runner's commit,
        // and the runner's commit (then the flush after it, finding the row terminal) would
        // drop the owner's accepted outcome. Held from before the look at the held write until
        // this settle has committed or is held; released before any callback and before the
        // opportunistic flush. Order: flush, then pending, then the connection; nothing takes
        // them the other way round. A `StoreHook` runs with the connection locked and must not
        // call back into the store (it would wait for the connection it is running under).
        let flush_guard = lk(&self.settle.flush);
        let mut prior_sink = None;
        let (claim, sink) = if owner {
            // A later explicit outcome replaces a held one; keep its announcement going.
            if let Some((had_event, held_sink)) = self.pending_info(&job_id) {
                if had_event && event.is_none() {
                    event = Some(owner_event(transition.to.as_deref()));
                }
                prior_sink = held_sink;
            }
            (claim, sink)
        } else if defer {
            // An outcome already held for this job stands; the runner never replaces it.
            match self.adopt(&job_id, claim, sink) {
                Ok(_) => return Ok(Settle::Deferred),
                Err(back) => back,
            }
        } else {
            (claim, sink)
        };
        match self.transition_event_in(&job_id, fence, &transition, event.as_ref()) {
            Ok(Some((record, ev))) => {
                if owner {
                    // The stored outcome is this one; the stale held write is moot.
                    self.take_entry(&job_id);
                }
                // A sink may flush; so does `after_write`.
                drop(flush_guard);
                if owner {
                    if let (Some(s), Some(e)) = (&prior_sink, &ev) {
                        let _ = catch_unwind(AssertUnwindSafe(|| s(e)));
                    }
                }
                self.after_write();
                Ok(Settle::Written {
                    record: Some(record),
                    event: ev,
                })
            }
            Ok(None) if owner => {
                // Already terminal: never merged. The same outcome is idempotent.
                let now = self.get(&job_id)?;
                if transition.to.as_deref() != Some(now.status.as_str()) {
                    return Err(JobError::new(
                        "illegal_transition",
                        format!(
                            "job {job_id} is already {}; it cannot be settled as {}",
                            now.status,
                            transition.to.as_deref().unwrap_or("?")
                        ),
                    ));
                }
                self.take_entry(&job_id);
                Ok(Settle::Written {
                    record: None,
                    event: None,
                })
            }
            Ok(None) => Ok(Settle::Written {
                record: None,
                event: None,
            }),
            Err(e) if defer && is_storage_error(&e) => {
                let bytes = entry_bytes(&job_id, &transition, &event);
                let entry = Entry {
                    job_id,
                    generation: fence,
                    transition,
                    event,
                    claim,
                    sink,
                    last_error: e.message.clone(),
                    attempts: 0,
                    last_try: Instant::now(),
                    durable: false,
                    bytes,
                };
                let held = if owner {
                    self.upsert_entry(entry)
                } else {
                    self.push_entry(entry, true)
                };
                if held {
                    Ok(Settle::Deferred)
                } else {
                    Err(e)
                }
            }
            Err(e) => Err(e),
        }
    }

    /// `(carries a lifecycle event, sink)` of the held write of `job_id`, if any.
    fn pending_info(&self, job_id: &str) -> Option<(bool, Option<EventSink>)> {
        if self.settle.len.load(Ordering::Relaxed) == 0 {
            return None;
        }
        lk(&self.settle.pending)
            .iter()
            .find(|e| e.job_id == job_id)
            .map(|e| (e.event.is_some(), e.sink.clone()))
    }

    /// Hold `entry`, replacing the held write of the same job (its claim and sink stay) and
    /// rewriting the journal file. If the rewrite fails the old file is removed: replaying the
    /// replaced outcome would be worse than none.
    ///
    /// Look and insert are one step under the pending lock: were the lock released between
    /// them, a runner could hold its own outcome in the gap and this call would report the
    /// owner's outcome held while the runner's stood.
    fn upsert_entry(&self, entry: Entry) -> bool {
        let mut p = lk(&self.settle.pending);
        if let Some(e) = p.iter_mut().find(|e| e.job_id == entry.job_id) {
            e.generation = entry.generation;
            e.transition = entry.transition;
            e.event = entry.event;
            e.bytes = entry.bytes;
            e.last_error = entry.last_error;
            if e.claim.is_none() {
                e.claim = entry.claim;
            }
            if e.sink.is_none() {
                e.sink = entry.sink;
            }
            let ok = self.write_journal(e);
            if !ok {
                self.remove_journal(&e.job_id);
            }
            e.durable = ok;
            return true;
        }
        self.push_locked(&mut p, entry, true)
    }

    /// Merge `claim` and `sink` into the held write of `job_id` and return its transition. When
    /// there is none, hand both back.
    #[allow(clippy::type_complexity)]
    // The error hands the caller its own claim (and sink) back, so it is as large as they are;
    // boxing it would only add an allocation to a path that is not hot.
    #[allow(clippy::result_large_err)]
    fn adopt(
        &self,
        job_id: &str,
        claim: Option<JobLock>,
        sink: Option<EventSink>,
    ) -> Result<Transition, (Option<JobLock>, Option<EventSink>)> {
        if self.settle.len.load(Ordering::Relaxed) == 0 {
            return Err((claim, sink));
        }
        let mut p = lk(&self.settle.pending);
        match p.iter_mut().find(|e| e.job_id == job_id) {
            Some(e) => {
                if e.claim.is_none() {
                    e.claim = claim;
                }
                if e.sink.is_none() {
                    e.sink = sink;
                }
                Ok(e.transition.clone())
            }
            None => Err((claim, sink)),
        }
    }

    /// The runner finished a handler and found a write held for the job (the handler deferred it
    /// through `JobControl::settle`): the runner writes nothing, takes over the claim, and
    /// returns the held transition. `Err` gives the claim back.
    // The error hands the caller its own claim (and sink) back, so it is as large as they are;
    // boxing it would only add an allocation to a path that is not hot.
    #[allow(clippy::result_large_err)]
    pub(crate) fn adopt_claim(
        &self,
        job_id: &str,
        claim: JobLock,
        sink: Option<EventSink>,
    ) -> Result<Transition, JobLock> {
        match self.adopt(job_id, Some(claim), sink) {
            Ok(t) => Ok(t),
            Err((claim, _)) => Err(claim.expect("the claim was passed in")),
        }
    }

    /// The held transition of `job_id`, if any.
    pub(crate) fn pending_transition(&self, job_id: &str) -> Option<Transition> {
        if self.settle.len.load(Ordering::Relaxed) == 0 {
            return None;
        }
        lk(&self.settle.pending)
            .iter()
            .find(|e| e.job_id == job_id)
            .map(|e| e.transition.clone())
    }

    /// Whether a terminal write is held for `job_id`.
    pub(crate) fn has_pending_settlement(&self, job_id: &str) -> bool {
        self.pending_transition(job_id).is_some()
    }

    /// Attach the lifecycle event that announces the held write of `job_id` when it lands
    /// (and journal it again), unless it already has one.
    pub(crate) fn set_pending_event(&self, job_id: &str, event: LifecycleEvent) {
        let mut p = lk(&self.settle.pending);
        if let Some(e) = p.iter_mut().find(|e| e.job_id == job_id) {
            if e.event.is_none() {
                e.event = Some(event);
                e.bytes = entry_bytes(&e.job_id, &e.transition, &e.event);
                if self.write_journal(e) {
                    e.durable = true;
                }
            }
        }
    }

    /// Give up a held write that came from the journal (no runner holds it) so the job is
    /// recovered as unfinished. True when one was discarded.
    pub(crate) fn discard_unclaimed_settlement(&self, job_id: &str) -> bool {
        // Under the flush lock, like a settle: a flush that had already copied this entry would
        // otherwise commit it after the discard, and an owner settle held in between would have
        // its journal file removed below while its entry said the file exists.
        let _flush = lk(&self.settle.flush);
        let entry = {
            let mut p = lk(&self.settle.pending);
            match p
                .iter()
                .position(|e| e.job_id == job_id && e.claim.is_none())
            {
                Some(i) => {
                    let e = p.remove(i);
                    self.settle.len.store(p.len(), Ordering::Relaxed);
                    Some(e)
                }
                None => None,
            }
        };
        match entry {
            Some(e) => {
                self.remove_journal(&e.job_id);
                true
            }
            None => false,
        }
    }

    /// Insert `entry`, journal it, and keep the count. False when the bounds are reached (a
    /// replayed entry is never refused: it is already on disk).
    fn push_entry(&self, entry: Entry, bounded: bool) -> bool {
        let mut p = lk(&self.settle.pending);
        self.push_locked(&mut p, entry, bounded)
    }

    /// [`Self::push_entry`] for a caller that already holds the pending lock.
    fn push_locked(&self, p: &mut Vec<Entry>, mut entry: Entry, bounded: bool) -> bool {
        if p.iter().any(|e| e.job_id == entry.job_id) {
            return true;
        }
        if bounded {
            let held: usize = p.iter().map(|e| e.bytes).sum();
            if p.len() >= MAX_PENDING_SETTLEMENTS
                || held.saturating_add(entry.bytes) > MAX_PENDING_SETTLEMENT_BYTES
            {
                return false;
            }
        }
        if !entry.durable {
            entry.durable = self.write_journal(&entry);
        }
        p.push(entry);
        self.settle.len.store(p.len(), Ordering::Relaxed);
        true
    }

    fn write_journal(&self, e: &Entry) -> bool {
        let Some(dir) = &self.settle.dir else {
            return false;
        };
        if !journal_safe(&e.job_id) {
            return false;
        }
        let bytes = serde_json::to_vec(&entry_json(e)).unwrap_or_default();
        let options = rightkit_fs::AtomicOptions {
            private: true,
            create_parents: true,
            durable: true,
        };
        rightkit_fs::atomic_write_with(&dir.join(format!("{}.json", e.job_id)), options, |f| {
            f.write_all(&bytes)
        })
        .is_ok()
    }

    fn remove_journal(&self, job_id: &str) {
        if let Some(dir) = &self.settle.dir {
            if journal_safe(job_id) {
                let _ = std::fs::remove_file(dir.join(format!("{job_id}.json")));
            }
        }
    }

    fn take_entry(&self, job_id: &str) -> Option<Entry> {
        let entry = {
            let mut p = lk(&self.settle.pending);
            let i = p.iter().position(|e| e.job_id == job_id)?;
            let e = p.remove(i);
            self.settle.len.store(p.len(), Ordering::Relaxed);
            e
        };
        self.remove_journal(&entry.job_id);
        Some(entry)
    }

    /// Run after a store write completed: retry the held writes that are due. Cheap when nothing
    /// is held. Never called with the connection locked.
    pub(super) fn after_write(&self) {
        if self.settle.len.load(Ordering::Relaxed) == 0 {
            return;
        }
        let _ = self.flush_impl(false);
    }

    /// Try every held terminal write now, regardless of how recently it was tried. Stops at the
    /// first storage error (the rest would meet the same one) and reports what is still
    /// pending. Safe to call at any time and from any thread; a write that lands removes its
    /// journal file, drops the job's claim and delivers its lifecycle event to the runner's sink.
    pub fn flush_pending_settlements(&self) -> FlushReport {
        self.flush_impl(true)
    }

    fn flush_impl(&self, force: bool) -> FlushReport {
        let mut report = FlushReport::default();
        let guard = if force {
            lk(&self.settle.flush)
        } else {
            match self.settle.flush.try_lock() {
                Ok(g) => g,
                Err(TryLockError::Poisoned(p)) => p.into_inner(),
                Err(TryLockError::WouldBlock) => return report,
            }
        };
        if force {
            self.retry_unread_journal();
        }
        let work: Vec<Work> = lk(&self.settle.pending)
            .iter()
            .filter(|e| e.due(force))
            .map(|e| Work {
                job_id: e.job_id.clone(),
                generation: e.generation,
                transition: e.transition.clone(),
                event: e.event.clone(),
            })
            .collect();
        let mut deliveries: Vec<(EventSink, Event, DeliveryPin<'_>)> = Vec::new();
        for w in work {
            let pin = self.pin_delivery(&w.job_id);
            match self.transition_event_in(&w.job_id, w.generation, &w.transition, w.event.as_ref())
            {
                Ok(done) => {
                    let entry = self.take_entry(&w.job_id);
                    if let (Some(Some(sink)), Some((_, Some(ev)))) =
                        (entry.as_ref().map(|e| e.sink.clone()), done)
                    {
                        deliveries.push((sink, ev, pin));
                    }
                    report.settled += 1;
                }
                Err(e) if is_storage_error(&e) => {
                    let mut p = lk(&self.settle.pending);
                    if let Some(entry) = p.iter_mut().find(|x| x.job_id == w.job_id) {
                        entry.attempts += 1;
                        entry.last_try = Instant::now();
                        entry.last_error = e.message.clone();
                    }
                    break;
                }
                Err(e) => {
                    self.take_entry(&w.job_id);
                    report.dropped.push((w.job_id.clone(), e));
                }
            }
        }
        report.pending = lk(&self.settle.pending).len();
        drop(guard);
        for (sink, ev, _pin) in deliveries {
            let _ = catch_unwind(AssertUnwindSafe(|| sink(&ev)));
        }
        report
    }

    /// Jobs whose handler finished but whose terminal state is not stored: "finished,
    /// unsettled", plus any journal file that could not be read (empty `status`). Empty when
    /// nothing is held.
    pub fn pending_settlements(&self) -> Vec<UnsettledJob> {
        let mut out: Vec<UnsettledJob> = lk(&self.settle.pending)
            .iter()
            .map(|e| UnsettledJob {
                job_id: e.job_id.clone(),
                status: e.transition.to.clone().unwrap_or_default(),
                durable: e.durable,
                claimed: e.claim.is_some(),
                attempts: e.attempts,
                last_error: e.last_error.clone(),
            })
            .collect();
        // A journal file that exists but could not be read: its outcome is unknown, so `status`
        // is empty. It is in the journal (`durable`) and is read again on the next explicit flush.
        out.extend(lk(&self.settle.unread).iter().map(|u| {
            UnsettledJob {
                job_id: u
                    .path
                    .file_stem()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                status: String::new(),
                durable: true,
                claimed: false,
                attempts: u.attempts,
                last_error: format!("journal file could not be read: {}", u.error),
            }
        }));
        out
    }

    /// The last chance to store what is held: one flush, then the report of what could not be
    /// stored. Call it when the process is about to exit. Entries that are
    /// [`UnsettledJob::durable`] are applied by the next open; the others are lost with the
    /// process. `Runner::shutdown_detailed` and `StagedPool::shutdown_detailed` call this.
    pub fn shutdown_detailed(&self) -> SettlementShutdown {
        let report = self.flush_pending_settlements();
        SettlementShutdown {
            settled: report.settled,
            dropped: report.dropped,
            unsettled: self.pending_settlements(),
        }
    }

    /// Read one journal file and hold what it records. `Err` only when the file could not be
    /// read; the file is then untouched. A file that was read but is not a valid entry is set
    /// aside as `.json.bad` and is `Ok(false)`: only content that was read and failed to parse
    /// or validate is judged bad.
    fn ingest_journal_file(&self, path: &Path) -> std::io::Result<bool> {
        let bytes = std::fs::read(path)?;
        let parsed = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| parse_entry(&v));
        let Some((job_id, generation, transition, event)) = parsed else {
            // Not ours or not valid: set it aside, never delete it.
            let _ = std::fs::rename(path, path.with_extension("json.bad"));
            return Ok(false);
        };
        let bytes = entry_bytes(&job_id, &transition, &event);
        Ok(self.push_entry(
            Entry {
                job_id,
                generation,
                transition,
                event,
                claim: None,
                sink: None,
                last_error: "recorded by an earlier run".into(),
                attempts: 0,
                last_try: Instant::now(),
                durable: true,
                bytes,
            },
            false,
        ))
    }

    /// Read again the journal files that could not be read at open. Once, with no sleep: a
    /// file that reads now is held like any other; one that is gone is forgotten; one that
    /// still cannot be read stays listed. Lock order is `unread` then `pending`, and nothing
    /// takes them the other way round.
    fn retry_unread_journal(&self) {
        let mut unread = lk(&self.settle.unread);
        unread.retain_mut(|u| match self.ingest_journal_file(&u.path) {
            Ok(_) => false,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => {
                u.attempts += 1;
                u.error = e.to_string();
                true
            }
        });
    }

    /// Read the journal left by an earlier run and write what it recorded. Run at open.
    ///
    /// Only a missing directory means "no journal". A directory that exists and cannot be
    /// listed (permission, I/O) may hold a durable Done or Cancelled for a job whose row still
    /// reads non-terminal; opening the store without it would let recovery run that job again.
    /// So the listing error is returned and the open fails; nothing is recovered until the
    /// journal can be enumerated.
    pub(super) fn replay_journal(&self) -> JobResult<()> {
        let Some(dir) = self.settle.dir.clone() else {
            return Ok(());
        };
        let unlisted = |e: std::io::Error| {
            JobError::new(
                "io",
                format!(
                    "settlement journal {} could not be listed: {e}",
                    dir.display()
                ),
            )
        };
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(unlisted(e)),
        };
        let mut loaded = false;
        let mut unread = Vec::new();
        for item in rd {
            let path = item.map_err(unlisted)?.path();
            // Temp files of an interrupted atomic write carry another extension.
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            match self.ingest_journal_file(&path) {
                Ok(pushed) => loaded |= pushed,
                // Gone between the listing and the read: nothing to settle.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                // Could not be read (permission, a sharing violation while another process
                // replaces it, an I/O error): not proof the entry is bad. Leave the file
                // where it is, report it, and read it again later.
                Err(e) => unread.push(UnreadJournal {
                    path,
                    error: e.to_string(),
                    attempts: 1,
                }),
            }
        }
        if loaded {
            let _ = self.flush_impl(true);
        }
        // Recorded after the flush above, which reads the unread files again: they were just
        // tried, and the next explicit flush is the retry.
        lk(&self.settle.unread).extend(unread);
        Ok(())
    }

    /// Keep `job_id` out of `prune` until the returned pin drops.
    pub(crate) fn pin_delivery(&self, job_id: &str) -> DeliveryPin<'_> {
        *lk(&self.settle.delivering)
            .entry(job_id.to_string())
            .or_insert(0) += 1;
        DeliveryPin {
            store: self,
            id: job_id.to_string(),
        }
    }

    /// Jobs a runner is still announcing (see [`DeliveryPin`]).
    pub(super) fn delivering_ids(&self) -> HashSet<String> {
        lk(&self.settle.delivering).keys().cloned().collect()
    }
}
