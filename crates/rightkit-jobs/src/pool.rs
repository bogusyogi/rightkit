//! A staged worker pool over [`Runner`]: parallel, admission-limited *prepare* work feeding
//! one thread-affine *compute* lane through a bounded hand-off.
//!
//! # Threading model
//!
//! - One dispatcher thread admits queued job ids in FIFO order while fewer than `limit()`
//!   jobs hold an admission slot. The limit closure is called on every admission attempt, so
//!   a raised limit is seen within one 50 ms re-check.
//! - Each admitted job runs `Runner::run` on its own thread (lock, fencing and lifecycle are
//!   the runner's). The adapter calls `prepare`, waits for its FIFO turn, then pushes the work
//!   into a `sync_channel(handoff)`. A full buffer blocks that thread, which keeps its slot
//!   until the send lands; every wait polls every 50 ms to heartbeat and check
//!   [`JobControl::abort_reason`].
//! - One lane thread creates the lane value (`new_lane`) and uses it for its whole life; only
//!   it calls `compute` and `idle`. It claims each item (an atomic) before computing, so an
//!   item a waiter already withdrew (cancelled, stalled, shutdown) is skipped, never run.
//! - After the send the thread waits for the lane's reply, heartbeating until compute starts.
//!
//! # Shutdown
//!
//! [`StagedPool::shutdown`] stops admission, then joins the dispatcher, every job thread and
//! the lane. Jobs still queued were never started and stay `queued` (recoverable). Jobs
//! buffered or waiting for a turn are withdrawn with the retryable host-handoff error: the
//! runner pauses them, refunds the attempt, and they stay recoverable. A `prepare` or
//! `compute` call already executing is never interrupted; shutdown waits for it to return
//! (plus at most one 50 ms poll). The bound is therefore the longest single `prepare` or
//! `compute` call in flight, and a handler call that never returns blocks shutdown.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::Value;

use crate::error::{JobError, JobResult};
use crate::runner::{JobControl, JobHandler, JobLocks, RunOutcome, Runner};
use crate::store::{Job, JobStore};

/// How often a waiting thread heartbeats and checks for cancel, stall or shutdown.
const POLL: Duration = Duration::from_millis(50);

const PENDING: u8 = 0;
const COMPUTING: u8 = 1;
const WITHDRAWN: u8 = 2;

/// What `prepare` decided for one job.
pub enum Staged<P> {
    /// Finished without the lane; this is the job's result.
    Done(Value),
    /// Needs the lane; `P` is handed to `compute`.
    Compute(P),
}

/// Product code for a staged pool. `prepare` runs on many threads (up to the limit);
/// `compute` and `idle` run on the single lane thread that built the lane value.
pub trait StagedHandler: Send + Sync + 'static {
    type Prepared: Send + 'static;
    /// Not `Send`: it is created, used and dropped on the lane thread.
    type Lane: 'static;
    fn new_lane(&self) -> Self::Lane;
    fn prepare(&self, job: &Job, ctl: &JobControl) -> JobResult<Staged<Self::Prepared>>;
    fn compute(
        &self,
        lane: &mut Self::Lane,
        job: &Job,
        work: Self::Prepared,
        ctl: &JobControl,
    ) -> JobResult<Value>;
    /// Called once per idle period, after `idle_after` with no work, on the lane thread.
    fn idle(&self, _lane: &mut Self::Lane) {}
}

type LimitFn = Arc<dyn Fn() -> usize + Send + Sync>;
/// Called with the job id and the result of its `Runner::run`, after its slot is released.
pub type OutcomeFn = Arc<dyn Fn(&str, JobResult<RunOutcome>) + Send + Sync>;

/// Pool settings.
pub struct PoolConfig {
    limit: LimitFn,
    handoff: usize,
    idle_after: Option<Duration>,
    on_outcome: Option<OutcomeFn>,
}

impl PoolConfig {
    /// `limit` is read on every admission attempt: the number of jobs that may be in
    /// `prepare` or waiting to hand off at once. A value of 0 is treated as 1.
    pub fn new(limit: impl Fn() -> usize + Send + Sync + 'static) -> Self {
        Self {
            limit: Arc::new(limit),
            handoff: 1,
            idle_after: None,
            on_outcome: None,
        }
    }
    /// Hand-off buffer size (default 1, minimum 1).
    pub fn handoff(mut self, n: usize) -> Self {
        self.handoff = n.max(1);
        self
    }
    /// Call `StagedHandler::idle` once the lane has had no work for `d`. Default: never.
    pub fn idle_after(mut self, d: Duration) -> Self {
        self.idle_after = Some(d);
        self
    }
    /// Observe the end of each pool-started run.
    pub fn on_outcome(mut self, f: OutcomeFn) -> Self {
        self.on_outcome = Some(f);
        self
    }
}

fn lk<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// One admission: its FIFO position and whether it still holds a slot / its turn.
struct Ticket {
    seq: u64,
    slot_held: AtomicBool,
    turn_passed: AtomicBool,
}

#[derive(Default)]
struct State {
    queue: VecDeque<String>,
    /// Ids queued or running in this pool (dedupe).
    pending: HashSet<String>,
    active: usize,
    next_seq: u64,
    /// Next sequence number allowed to send into the hand-off.
    next_turn: u64,
    passed: BTreeSet<u64>,
    /// Count of items the lane has taken off the buffer.
    drained: u64,
    tickets: HashMap<String, Arc<Ticket>>,
}

struct Shared {
    state: Mutex<State>,
    cv: Condvar,
    stopping: AtomicBool,
    limit: LimitFn,
    on_outcome: Option<OutcomeFn>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl Shared {
    fn stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// Wait until `done(state)` or 50 ms, whichever is first.
    fn park(&self, done: impl Fn(&State) -> bool) {
        let st = lk(&self.state);
        if done(&st) {
            return;
        }
        let _ = self.cv.wait_timeout(st, POLL);
    }

    fn ticket(&self, job_id: &str) -> Option<Arc<Ticket>> {
        lk(&self.state).tickets.get(job_id).cloned()
    }

    fn release_slot(&self, t: &Ticket) {
        if t.slot_held.swap(false, Ordering::SeqCst) {
            lk(&self.state).active -= 1;
            self.cv.notify_all();
        }
    }

    fn pass_turn(&self, t: &Ticket) {
        if !t.turn_passed.swap(true, Ordering::SeqCst) {
            let mut st = lk(&self.state);
            st.passed.insert(t.seq);
            loop {
                let next = st.next_turn;
                if !st.passed.remove(&next) {
                    break;
                }
                st.next_turn += 1;
            }
            drop(st);
            self.cv.notify_all();
        }
    }

    fn finish(&self, id: &str, t: &Ticket) {
        self.release_slot(t);
        self.pass_turn(t);
        let mut st = lk(&self.state);
        st.tickets.remove(id);
        st.pending.remove(id);
        drop(st);
        self.cv.notify_all();
    }
}

/// Releases a ticket's slot and turn however the run ends (error, panic, early return).
struct TicketGuard {
    shared: Arc<Shared>,
    ticket: Option<Arc<Ticket>>,
}

impl Drop for TicketGuard {
    fn drop(&mut self) {
        if let Some(t) = &self.ticket {
            self.shared.release_slot(t);
            self.shared.pass_turn(t);
        }
    }
}

struct Item<P> {
    job: Job,
    work: P,
    ctl: Arc<JobControl>,
    claim: Arc<AtomicU8>,
    reply: mpsc::Sender<JobResult<Value>>,
}

// `Work` is the hot path; boxing it would add an allocation per job to shrink a rare `Stop`.
#[allow(clippy::large_enum_variant)]
enum Msg<P> {
    Work(Item<P>),
    Stop,
}

fn lane_lost() -> JobError {
    JobError::retryable("pool_lane_lost", "the compute lane is not running; retry")
}

/// The `JobHandler` the runner sees: prepare here, compute on the lane.
struct Adapter<H: StagedHandler> {
    handler: Arc<H>,
    shared: Arc<Shared>,
    tx: SyncSender<Msg<H::Prepared>>,
}

impl<H: StagedHandler> Adapter<H> {
    /// Heartbeat, then the reason to stop waiting, if any.
    fn abort(&self, ctl: &JobControl) -> Option<JobError> {
        ctl.heartbeat();
        if let Some(e) = ctl.abort_reason() {
            return Some(e);
        }
        if self.shared.stopping() {
            return Some(JobError::handoff());
        }
        None
    }

    fn wait_turn(&self, seq: u64, ctl: &JobControl) -> JobResult<()> {
        loop {
            if let Some(e) = self.abort(ctl) {
                return Err(e);
            }
            self.shared.park(|st| st.next_turn == seq);
            if lk(&self.shared.state).next_turn == seq {
                return Ok(());
            }
        }
    }
}

impl<H: StagedHandler> JobHandler for Adapter<H> {
    fn run(&self, _job: &Job, _ctl: &JobControl) -> JobResult<Value> {
        Err(JobError::new(
            "pool_misuse",
            "a staged pool handler needs a runner that calls run_shared",
        ))
    }

    fn run_shared(&self, job: &Job, ctl: &Arc<JobControl>) -> JobResult<Value> {
        let sh = &self.shared;
        let ticket = sh.ticket(&job.job_id);
        let _guard = TicketGuard {
            shared: sh.clone(),
            ticket: ticket.clone(),
        };
        if sh.stopping() {
            return Err(JobError::handoff());
        }
        let work = match self.handler.prepare(job, ctl)? {
            Staged::Done(v) => return Ok(v),
            Staged::Compute(p) => p,
        };
        match &ticket {
            Some(t) => self.wait_turn(t.seq, ctl)?,
            None => {
                if let Some(e) = self.abort(ctl) {
                    return Err(e);
                }
            }
        }
        let claim = Arc::new(AtomicU8::new(PENDING));
        let (rtx, rrx) = mpsc::channel();
        let mut msg = Msg::Work(Item {
            job: job.clone(),
            work,
            ctl: ctl.clone(),
            claim: claim.clone(),
            reply: rtx,
        });
        loop {
            let seen = lk(&sh.state).drained;
            match self.tx.try_send(msg) {
                Ok(()) => break,
                Err(TrySendError::Full(m)) => {
                    msg = m;
                    if let Some(e) = self.abort(ctl) {
                        return Err(e);
                    }
                    sh.park(|st| st.drained != seen);
                }
                Err(TrySendError::Disconnected(_)) => return Err(lane_lost()),
            }
        }
        if let Some(t) = &ticket {
            sh.release_slot(t);
            sh.pass_turn(t);
        }
        loop {
            match rrx.recv_timeout(POLL) {
                Ok(r) => return r,
                Err(RecvTimeoutError::Timeout) => {
                    if claim.load(Ordering::SeqCst) == COMPUTING {
                        // Compute heartbeats through `ctl`; just wait for the result.
                        return rrx.recv().unwrap_or_else(|_| Err(lane_lost()));
                    }
                    if let Some(e) = self.abort(ctl) {
                        let withdrawn = claim
                            .compare_exchange(
                                PENDING,
                                WITHDRAWN,
                                Ordering::SeqCst,
                                Ordering::SeqCst,
                            )
                            .is_ok();
                        if withdrawn {
                            return Err(e);
                        }
                        // The lane claimed it first: compute is running or about to.
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return Err(lane_lost()),
            }
        }
    }
}

fn lane_main<H: StagedHandler>(
    h: Arc<H>,
    rx: Receiver<Msg<H::Prepared>>,
    shared: Arc<Shared>,
    idle_after: Option<Duration>,
) {
    let mut lane = h.new_lane();
    let mut idle_pending = false;
    loop {
        let msg = if idle_pending {
            match idle_after {
                Some(d) => match rx.recv_timeout(d) {
                    Ok(m) => m,
                    Err(RecvTimeoutError::Timeout) => {
                        idle_pending = false;
                        let _ = catch_unwind(AssertUnwindSafe(|| h.idle(&mut lane)));
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                },
                None => {
                    idle_pending = false;
                    continue;
                }
            }
        } else {
            match rx.recv() {
                Ok(m) => m,
                Err(_) => break,
            }
        };
        {
            lk(&shared.state).drained += 1;
            shared.cv.notify_all();
        }
        let item = match msg {
            Msg::Stop => break,
            Msg::Work(item) => item,
        };
        let Item {
            job,
            work,
            ctl,
            claim,
            reply,
        } = item;
        if claim
            .compare_exchange(PENDING, COMPUTING, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            continue; // withdrawn by its waiter
        }
        ctl.heartbeat();
        let abort = if shared.stopping() {
            Some(JobError::handoff())
        } else {
            ctl.abort_reason()
        };
        let result = match abort {
            Some(e) => Err(e),
            None => catch_unwind(AssertUnwindSafe(|| h.compute(&mut lane, &job, work, &ctl)))
                .unwrap_or_else(|_| {
                    // The lane may be inconsistent after a panic: rebuild it on this thread.
                    let old = std::mem::replace(
                        &mut lane,
                        // `new_lane` may itself panic; that ends the thread and waiters see it.
                        h.new_lane(),
                    );
                    let _ = catch_unwind(AssertUnwindSafe(|| drop(old)));
                    Err(JobError::new("panic", "staged compute panicked"))
                }),
        };
        let _ = reply.send(result);
        idle_pending = true;
    }
}

fn job_main(shared: Arc<Shared>, runner: Arc<Runner>, id: String, ticket: Arc<Ticket>) {
    let res = catch_unwind(AssertUnwindSafe(|| runner.run(&id)))
        .unwrap_or_else(|_| Err(JobError::new("panic", "pool job thread panicked")));
    shared.finish(&id, &ticket);
    if let Some(f) = &shared.on_outcome {
        let _ = catch_unwind(AssertUnwindSafe(|| f(&id, res)));
    }
}

fn reap(workers: &mut Vec<JoinHandle<()>>) {
    let mut live = Vec::with_capacity(workers.len());
    for h in workers.drain(..) {
        if h.is_finished() {
            let _ = h.join();
        } else {
            live.push(h);
        }
    }
    *workers = live;
}

fn dispatch(shared: Arc<Shared>, runner: Arc<Runner>) {
    loop {
        {
            let mut st = lk(&shared.state);
            while st.queue.is_empty() && !shared.stopping() {
                st = shared.cv.wait(st).unwrap_or_else(|e| e.into_inner());
            }
            if shared.stopping() {
                return;
            }
        }
        // Called outside the state lock so the closure may do anything.
        let limit = (shared.limit)().max(1);
        let admitted = {
            let mut st = lk(&shared.state);
            if shared.stopping() {
                return;
            }
            if st.active < limit && !st.queue.is_empty() {
                let id = st.queue.pop_front().expect("queue checked non-empty");
                let seq = st.next_seq;
                st.next_seq += 1;
                st.active += 1;
                let ticket = Arc::new(Ticket {
                    seq,
                    slot_held: AtomicBool::new(true),
                    turn_passed: AtomicBool::new(false),
                });
                st.tickets.insert(id.clone(), ticket.clone());
                Some((id, ticket))
            } else {
                // Woken by a release or enqueue, or after 50 ms to re-read the limit.
                let _ = shared.cv.wait_timeout(st, POLL);
                None
            }
        };
        let Some((id, ticket)) = admitted else {
            continue;
        };
        let (s2, r2, id2, t2) = (shared.clone(), runner.clone(), id.clone(), ticket.clone());
        match std::thread::Builder::new()
            .name(format!("rk-pool-{id}"))
            .spawn(move || job_main(s2, r2, id2, t2))
        {
            Ok(h) => {
                let mut w = lk(&shared.workers);
                reap(&mut w);
                w.push(h);
            }
            Err(e) => {
                // Never started: the job stays queued in the store, so it is recoverable.
                shared.finish(&id, &ticket);
                if let Some(f) = &shared.on_outcome {
                    let _ = catch_unwind(AssertUnwindSafe(|| f(&id, Err(JobError::from(e)))));
                }
            }
        }
    }
}

/// A running staged pool. Dropping it shuts it down.
pub struct StagedPool {
    runner: Arc<Runner>,
    shared: Arc<Shared>,
    dispatcher: Option<JoinHandle<()>>,
    lane: Option<JoinHandle<()>>,
    stop_lane: Option<Box<dyn FnOnce() + Send>>,
}

impl StagedPool {
    /// Build the runner around the pool's adapter (`configure` may set stall timeout, sink,
    /// attempts and so on; it must not replace the handler) and start the dispatcher and lane.
    pub fn start<H: StagedHandler>(
        store: Arc<JobStore>,
        locks: JobLocks,
        handler: Arc<H>,
        cfg: PoolConfig,
        configure: impl FnOnce(Runner) -> Runner,
    ) -> JobResult<StagedPool> {
        let (tx, rx) = mpsc::sync_channel::<Msg<H::Prepared>>(cfg.handoff.max(1));
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            cv: Condvar::new(),
            stopping: AtomicBool::new(false),
            limit: cfg.limit,
            on_outcome: cfg.on_outcome,
            workers: Mutex::new(Vec::new()),
        });
        let adapter = Arc::new(Adapter {
            handler: handler.clone(),
            shared: shared.clone(),
            tx: tx.clone(),
        });
        let runner = Arc::new(configure(Runner::new(store, locks, adapter)));
        let lane = {
            let (h, s, idle) = (handler, shared.clone(), cfg.idle_after);
            std::thread::Builder::new()
                .name("rk-pool-lane".into())
                .spawn(move || lane_main(h, rx, s, idle))
                .map_err(JobError::from)?
        };
        let mut pool = StagedPool {
            runner: runner.clone(),
            shared: shared.clone(),
            dispatcher: None,
            lane: Some(lane),
            stop_lane: Some(Box::new(move || {
                let _ = tx.send(Msg::Stop);
            })),
        };
        // On failure `pool` drops and stops the lane.
        pool.dispatcher = Some(
            std::thread::Builder::new()
                .name("rk-pool-dispatch".into())
                .spawn(move || dispatch(shared, runner))
                .map_err(JobError::from)?,
        );
        Ok(pool)
    }

    /// Queue one job for admission. Queued ids run in call order; an id already queued or
    /// running in this pool is ignored. Errors if the job does not exist or the pool stopped.
    pub fn enqueue(&self, job_id: &str) -> JobResult<()> {
        self.runner.store().get(job_id)?;
        self.enqueue_all(&[job_id.to_string()]).map(|_| ())
    }

    /// Enqueue every recoverable job, oldest first, and return the ids newly queued.
    pub fn resume_recoverable(&self) -> JobResult<Vec<String>> {
        let ids = self.runner.recoverable_ids()?;
        self.enqueue_all(&ids)
    }

    fn enqueue_all(&self, ids: &[String]) -> JobResult<Vec<String>> {
        if self.shared.stopping() {
            return Err(JobError::new(
                "pool_stopped",
                "the staged pool is shut down",
            ));
        }
        let mut st = lk(&self.shared.state);
        let mut out = Vec::new();
        for id in ids {
            if st.pending.insert(id.clone()) {
                st.queue.push_back(id.clone());
                out.push(id.clone());
            }
        }
        drop(st);
        self.shared.cv.notify_all();
        Ok(out)
    }

    /// The pool's runner. Running a job on it directly (not through `enqueue`) bypasses
    /// admission and ordering but still uses the lane.
    pub fn runner(&self) -> &Arc<Runner> {
        &self.runner
    }

    /// Stop admission, withdraw buffered work (left recoverable), and join every thread the
    /// pool started. Waits for any `prepare` or `compute` call already running; see the
    /// module docs for the bound.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        {
            let _g = lk(&self.shared.state);
            self.shared.stopping.store(true, Ordering::SeqCst);
            self.shared.cv.notify_all();
        }
        if let Some(h) = self.dispatcher.take() {
            let _ = h.join();
        }
        let workers = std::mem::take(&mut *lk(&self.shared.workers));
        for h in workers {
            let _ = h.join();
        }
        if let Some(stop) = self.stop_lane.take() {
            stop();
        }
        if let Some(h) = self.lane.take() {
            let _ = h.join();
        }
    }
}

impl Drop for StagedPool {
    fn drop(&mut self) {
        self.stop();
    }
}
