//! `StagedPool`: admission limit, bounded hand-off, one thread-affine lane, FIFO order,
//! cancellation while buffered, heartbeats while waiting, and shutdown.
//!
//! Coordination is by channels: a recording handler reports every `prepare`, `compute` and
//! `idle` call as an event, and gates (channels of permits) hold prepare or compute open.
//! The few negative checks ("nothing else happens") use a bounded `recv_timeout`; every
//! positive wait has a generous deadline instead of a tuned sleep.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

use rightkit_jobs::{
    Job, JobControl, JobLocks, JobResult, JobStore, NewJob, PoolConfig, RunOutcome, Runner, Staged,
    StagedHandler, StagedPool, CANCELLED, COMPLETE,
};
use serde_json::{json, Value};

const WAIT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq)]
enum Ev {
    NewLane(ThreadId),
    Prepare(String),
    Compute(String, ThreadId),
    Idle(ThreadId),
}

/// Not `Send`: proves the pool never moves the lane between threads.
struct Lane {
    id: ThreadId,
    _pin: Rc<()>,
}

struct Rec {
    ev: Mutex<Sender<Ev>>,
    prep_gate: Mutex<Receiver<()>>,
    prep_gated: AtomicBool,
    gate: Mutex<Receiver<()>>,
    gated: AtomicBool,
    in_prepare: AtomicUsize,
    max_prepare: AtomicUsize,
}

struct Chans {
    ev: Receiver<Ev>,
    gate: Sender<()>,
    prep: Sender<()>,
}

fn rec(gated: bool, prep_gated: bool) -> (Arc<Rec>, Chans) {
    let (ev_tx, ev_rx) = mpsc::channel();
    let (gate_tx, gate_rx) = mpsc::channel();
    let (prep_tx, prep_rx) = mpsc::channel();
    let r = Arc::new(Rec {
        ev: Mutex::new(ev_tx),
        prep_gate: Mutex::new(prep_rx),
        prep_gated: AtomicBool::new(prep_gated),
        gate: Mutex::new(gate_rx),
        gated: AtomicBool::new(gated),
        in_prepare: AtomicUsize::new(0),
        max_prepare: AtomicUsize::new(0),
    });
    (
        r,
        Chans {
            ev: ev_rx,
            gate: gate_tx,
            prep: prep_tx,
        },
    )
}

impl Rec {
    fn send(&self, e: Ev) {
        let _ = self.ev.lock().unwrap().send(e);
    }
}

/// Wait for one permit, heartbeating so a stall timeout never fires while a test holds a gate.
fn permit(rx: &Mutex<Receiver<()>>, ctl: &JobControl) {
    let deadline = Instant::now() + WAIT;
    loop {
        let r = rx.lock().unwrap().recv_timeout(Duration::from_millis(20));
        match r {
            Ok(()) => return,
            Err(_) => {
                ctl.heartbeat();
                assert!(Instant::now() < deadline, "gate never opened");
            }
        }
    }
}

impl StagedHandler for Rec {
    type Prepared = String;
    type Lane = Lane;

    fn new_lane(&self) -> Lane {
        let id = thread::current().id();
        self.send(Ev::NewLane(id));
        Lane {
            id,
            _pin: Rc::new(()),
        }
    }

    fn prepare(&self, job: &Job, ctl: &JobControl) -> JobResult<Staged<String>> {
        let n = self.in_prepare.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_prepare.fetch_max(n, Ordering::SeqCst);
        self.send(Ev::Prepare(job.job_id.clone()));
        if self.prep_gated.load(Ordering::SeqCst) {
            permit(&self.prep_gate, ctl);
        }
        if let Some(ms) = job.request["delay_ms"].as_u64() {
            thread::sleep(Duration::from_millis(ms));
            ctl.heartbeat();
        }
        self.in_prepare.fetch_sub(1, Ordering::SeqCst);
        if job.request["done"] == json!(true) {
            Ok(Staged::Done(json!({"done": true})))
        } else {
            Ok(Staged::Compute(job.job_id.clone()))
        }
    }

    fn compute(
        &self,
        lane: &mut Lane,
        _job: &Job,
        work: String,
        ctl: &JobControl,
    ) -> JobResult<Value> {
        assert_eq!(
            thread::current().id(),
            lane.id,
            "compute left the lane thread"
        );
        self.send(Ev::Compute(work.clone(), lane.id));
        if self.gated.load(Ordering::SeqCst) {
            permit(&self.gate, ctl);
        }
        Ok(json!({"id": work}))
    }

    fn idle(&self, lane: &mut Lane) {
        assert_eq!(thread::current().id(), lane.id, "idle left the lane thread");
        self.send(Ev::Idle(lane.id));
    }
}

type Outcome = (String, JobResult<RunOutcome>);

struct Outs {
    rx: Receiver<Outcome>,
    got: HashMap<String, JobResult<RunOutcome>>,
}

impl Outs {
    fn wait(&mut self, id: &str) -> RunOutcome {
        loop {
            if let Some(r) = self.got.remove(id) {
                return r.unwrap();
            }
            let (k, r) = self.rx.recv_timeout(WAIT).expect("no outcome in time");
            self.got.insert(k, r);
        }
    }
    fn finished(&mut self, id: &str) -> Job {
        match self.wait(id) {
            RunOutcome::Finished(j) => j,
            other => panic!("{id}: expected Finished, got {other:?}"),
        }
    }
}

struct Env {
    dir: tempfile::TempDir,
    store: Arc<JobStore>,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(JobStore::open(dir.path().join("j.db")).unwrap());
    Env { dir, store }
}

fn submit(store: &JobStore, request: Value) -> String {
    store
        .submit(&NewJob {
            request_id: None,
            kind: "staged".into(),
            request,
        })
        .unwrap()
        .0
        .job_id
}

fn start_pool(
    env: &Env,
    rec: &Arc<Rec>,
    limit: impl Fn() -> usize + Send + Sync + 'static,
    tweak: impl FnOnce(PoolConfig) -> PoolConfig,
    configure: impl FnOnce(Runner) -> Runner,
) -> (StagedPool, Outs) {
    let (otx, orx) = mpsc::channel::<Outcome>();
    let otx = Mutex::new(otx);
    let cfg = tweak(PoolConfig::new(limit).on_outcome(Arc::new(
        move |id: &str, r: JobResult<RunOutcome>| {
            let _ = otx.lock().unwrap().send((id.to_string(), r));
        },
    )));
    let pool = StagedPool::start(
        env.store.clone(),
        JobLocks::new(env.dir.path().join("locks")),
        rec.clone(),
        cfg,
        configure,
    )
    .unwrap();
    (
        pool,
        Outs {
            rx: orx,
            got: HashMap::new(),
        },
    )
}

/// A limit closure that reads a shared number, so a test can change it mid-run.
fn reads(a: &Arc<AtomicUsize>) -> impl Fn() -> usize + Send + Sync + 'static {
    let a = a.clone();
    move || a.load(Ordering::SeqCst)
}

/// Next event matching `f`, discarding others.
fn expect(ev: &Receiver<Ev>, f: impl Fn(&Ev) -> bool) -> Ev {
    let deadline = Instant::now() + WAIT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match ev.recv_timeout(left) {
            Ok(e) if f(&e) => return e,
            Ok(_) => {}
            Err(_) => panic!("expected event never arrived"),
        }
    }
}

fn prepared(id: &str) -> impl Fn(&Ev) -> bool + '_ {
    move |e| matches!(e, Ev::Prepare(p) if p == id)
}

fn computing(id: &str) -> impl Fn(&Ev) -> bool + '_ {
    move |e| matches!(e, Ev::Compute(p, _) if p == id)
}

/// Wait until every pattern has been seen, in any order (events from different threads race).
fn expect_all(ev: &Receiver<Ev>, pats: &[(bool, &str)]) {
    // (true, id) = a compute of id; (false, id) = a prepare of id.
    let mut seen = vec![false; pats.len()];
    let deadline = Instant::now() + WAIT;
    while !seen.iter().all(|s| *s) {
        let left = deadline.saturating_duration_since(Instant::now());
        let e = ev
            .recv_timeout(left)
            .expect("expected events never arrived");
        for (i, (is_compute, id)) in pats.iter().enumerate() {
            let hit = if *is_compute {
                computing(id)(&e)
            } else {
                prepared(id)(&e)
            };
            seen[i] |= hit;
        }
    }
}

fn silent_for(ev: &Receiver<Ev>, d: Duration) {
    match ev.recv_timeout(d) {
        Err(RecvTimeoutError::Timeout) => {}
        other => panic!("expected silence, got {other:?}"),
    }
}

fn release(tx: &Sender<()>, n: usize) {
    for _ in 0..n {
        tx.send(()).unwrap();
    }
}

#[test]
fn limit_one_never_overlaps() {
    let env = env();
    let (r, _ch) = rec(false, false);
    let calls = Arc::new(AtomicUsize::new(0));
    let c2 = calls.clone();
    let (pool, mut outs) = start_pool(
        &env,
        &r,
        move || {
            c2.fetch_add(1, Ordering::SeqCst);
            1
        },
        |c| c.handoff(1),
        |x| x,
    );
    let ids: Vec<String> = (0..6)
        .map(|_| submit(&env.store, json!({"delay_ms": 2})))
        .collect();
    for id in &ids {
        pool.enqueue(id).unwrap();
    }
    for id in &ids {
        assert_eq!(outs.finished(id).status, COMPLETE);
    }
    assert_eq!(
        r.max_prepare.load(Ordering::SeqCst),
        1,
        "prepare calls overlapped"
    );
    assert!(
        calls.load(Ordering::SeqCst) >= ids.len(),
        "the limit closure is called on every admission"
    );
    pool.shutdown();
}

#[test]
fn done_from_prepare_skips_the_lane() {
    let env = env();
    let (r, ch) = rec(false, false);
    let (pool, mut outs) = start_pool(&env, &r, || 2, |c| c, |x| x);
    let id = submit(&env.store, json!({"done": true}));
    pool.enqueue(&id).unwrap();
    let j = outs.finished(&id);
    assert_eq!(j.status, COMPLETE);
    assert_eq!(j.result, Some(json!({"done": true})));
    pool.shutdown();
    let seen: Vec<Ev> = ch.ev.try_iter().collect();
    assert!(
        !seen.iter().any(|e| matches!(e, Ev::Compute(..))),
        "{seen:?}"
    );
}

#[test]
fn raised_limit_seen_next_admission() {
    let env = env();
    let (r, ch) = rec(false, true);
    let limit = Arc::new(AtomicUsize::new(1));
    let (pool, mut outs) = start_pool(&env, &r, reads(&limit), |c| c, |x| x);
    let a = submit(&env.store, json!({}));
    let b = submit(&env.store, json!({}));
    pool.enqueue(&a).unwrap();
    pool.enqueue(&b).unwrap();
    expect(&ch.ev, prepared(&a));
    // `a` holds the only slot (its prepare is gated), so `b` is not admitted.
    silent_for(&ch.ev, Duration::from_millis(300));
    limit.store(2, Ordering::SeqCst);
    // No new enqueue and no slot release: only the closure's new value can admit `b`.
    expect(&ch.ev, prepared(&b));
    release(&ch.prep, 2);
    assert_eq!(outs.finished(&a).status, COMPLETE);
    assert_eq!(outs.finished(&b).status, COMPLETE);
    pool.shutdown();
}

#[test]
fn full_handoff_blocks_prepare() {
    let env = env();
    let (r, ch) = rec(true, false);
    let (pool, mut outs) = start_pool(&env, &r, || 1, |c| c.handoff(1), |x| x);
    let ids: Vec<String> = (0..4).map(|_| submit(&env.store, json!({}))).collect();
    for id in &ids {
        pool.enqueue(id).unwrap();
    }
    // 0 is in the lane (gated); 1 fills the buffer and frees its slot; 2 is admitted
    // and its send blocks on the full buffer, so it keeps the only slot.
    expect_all(
        &ch.ev,
        &[
            (true, ids[0].as_str()),
            (false, ids[1].as_str()),
            (false, ids[2].as_str()),
        ],
    );
    // 3 cannot be admitted: no further prepare starts while 2 is blocked.
    silent_for(&ch.ev, Duration::from_millis(400));
    release(&ch.gate, 4);
    expect(&ch.ev, prepared(&ids[3]));
    for id in &ids {
        assert_eq!(outs.finished(id).status, COMPLETE);
    }
    pool.shutdown();
}

#[test]
fn compute_runs_on_one_thread() {
    let env = env();
    let (r, ch) = rec(false, false);
    let (pool, mut outs) = start_pool(&env, &r, || 3, |c| c.handoff(2), |x| x);
    let ids: Vec<String> = (0..8)
        .map(|_| submit(&env.store, json!({"delay_ms": 1})))
        .collect();
    for id in &ids {
        pool.enqueue(id).unwrap();
    }
    for id in &ids {
        assert_eq!(outs.finished(id).status, COMPLETE);
    }
    pool.shutdown();
    let events: Vec<Ev> = ch.ev.try_iter().collect();
    let lanes: Vec<ThreadId> = events
        .iter()
        .filter_map(|e| match e {
            Ev::NewLane(t) => Some(*t),
            _ => None,
        })
        .collect();
    assert_eq!(lanes.len(), 1, "one lane value for the pool's life");
    assert_ne!(lanes[0], thread::current().id());
    let computes: Vec<ThreadId> = events
        .iter()
        .filter_map(|e| match e {
            Ev::Compute(_, t) => Some(*t),
            _ => None,
        })
        .collect();
    assert_eq!(computes.len(), ids.len());
    assert!(computes.iter().all(|t| *t == lanes[0]));
}

#[test]
fn idle_fires_once_not_between_back_to_back() {
    let env = env();
    let (r, ch) = rec(true, false);
    let (pool, mut outs) = start_pool(
        &env,
        &r,
        || 1,
        |c| c.handoff(1).idle_after(Duration::from_millis(200)),
        |x| x,
    );
    let ids: Vec<String> = (0..3).map(|_| submit(&env.store, json!({}))).collect();
    for id in &ids {
        pool.enqueue(id).unwrap();
    }
    // 0 is in the lane (gated), 1 is buffered (2 was admitted only after 1's send landed).
    expect_all(&ch.ev, &[(true, ids[0].as_str()), (false, ids[2].as_str())]);
    release(&ch.gate, 3);
    // Everything up to the first idle call: 1 and 2 run, and nothing idles between them.
    let mut before_idle = Vec::new();
    loop {
        match ch.ev.recv_timeout(WAIT).expect("idle never fired") {
            Ev::Idle(_) => break,
            e @ Ev::Compute(..) => before_idle.push(e),
            _ => {}
        }
    }
    assert!(computing(&ids[1])(&before_idle[0]) && computing(&ids[2])(&before_idle[1]));
    assert_eq!(before_idle.len(), 2, "{before_idle:?}");
    for id in &ids {
        assert_eq!(outs.finished(id).status, COMPLETE);
    }
    // Once per idle period: no second call while the lane stays idle.
    silent_for(&ch.ev, Duration::from_millis(800));
    // New work starts a new period, which idles once more.
    let again = submit(&env.store, json!({}));
    pool.enqueue(&again).unwrap();
    release(&ch.gate, 1);
    expect(&ch.ev, computing(&again));
    expect(&ch.ev, |e| matches!(e, Ev::Idle(_)));
    pool.shutdown();
}

fn compute_order(ev: &Receiver<Ev>, n: usize) -> Vec<String> {
    let mut out = Vec::new();
    while out.len() < n {
        if let Ev::Compute(id, _) = expect(ev, |e| matches!(e, Ev::Compute(..))) {
            out.push(id);
        }
    }
    out
}

#[test]
fn recovered_order_equals_submission_order() {
    let env = env();
    // Earlier jobs prepare slowest, so without ordering they would reach the lane last.
    let ids: Vec<String> = [80, 60, 40, 20, 0]
        .iter()
        .map(|ms| submit(&env.store, json!({"delay_ms": ms})))
        .collect();
    let (r, ch) = rec(false, false);
    let (pool, mut outs) = start_pool(&env, &r, || 3, |c| c.handoff(1), |x| x);
    let queued = pool.resume_recoverable().unwrap();
    assert_eq!(queued, ids);
    assert_eq!(compute_order(&ch.ev, ids.len()), ids);
    for id in &ids {
        assert_eq!(outs.finished(id).status, COMPLETE);
    }
    pool.shutdown();
}

#[test]
fn live_order_is_fifo() {
    let env = env();
    let (r, ch) = rec(false, false);
    let (pool, mut outs) = start_pool(&env, &r, || 3, |c| c.handoff(1), |x| x);
    let ids: Vec<String> = [80, 60, 40, 20, 0]
        .iter()
        .map(|ms| submit(&env.store, json!({"delay_ms": ms})))
        .collect();
    for id in &ids {
        pool.enqueue(id).unwrap();
    }
    assert_eq!(compute_order(&ch.ev, ids.len()), ids);
    for id in &ids {
        assert_eq!(outs.finished(id).status, COMPLETE);
    }
    pool.shutdown();
}

#[test]
fn cancel_while_buffered_ends_cancelled() {
    let env = env();
    let (r, ch) = rec(true, false);
    let (pool, mut outs) = start_pool(&env, &r, || 1, |c| c.handoff(1), |x| x);
    let ids: Vec<String> = (0..3).map(|_| submit(&env.store, json!({}))).collect();
    for id in &ids {
        pool.enqueue(id).unwrap();
    }
    // With limit 1, job 2 being admitted proves job 1's send landed: it is buffered.
    expect_all(&ch.ev, &[(true, ids[0].as_str()), (false, ids[2].as_str())]);
    env.store.cancel(&ids[1]).unwrap();
    let j = outs.finished(&ids[1]);
    assert_eq!(j.status, CANCELLED);
    // Job 0 is still computing, so the cancelled job ended without reaching the lane.
    release(&ch.gate, 2);
    assert_eq!(outs.finished(&ids[0]).status, COMPLETE);
    assert_eq!(outs.finished(&ids[2]).status, COMPLETE);
    pool.shutdown();
    let computed: Vec<Ev> = ch.ev.try_iter().filter(computing(&ids[1])).collect();
    assert!(
        computed.is_empty(),
        "cancelled job was computed: {computed:?}"
    );
    assert_eq!(env.store.get(&ids[1]).unwrap().status, CANCELLED);
}

#[test]
fn stall_timeout_does_not_trip_while_waiting() {
    let env = env();
    let (r, ch) = rec(true, false);
    let (pool, mut outs) = start_pool(
        &env,
        &r,
        || 2,
        |c| c.handoff(1),
        |x| x.with_stall_timeout(Duration::from_millis(400)),
    );
    let a = submit(&env.store, json!({}));
    let b = submit(&env.store, json!({}));
    pool.enqueue(&a).unwrap();
    pool.enqueue(&b).unwrap();
    expect_all(&ch.ev, &[(true, a.as_str()), (false, b.as_str())]);
    // `b` now waits for the busy lane for four stall timeouts; only its heartbeats keep it alive.
    silent_for(&ch.ev, Duration::from_millis(1600));
    release(&ch.gate, 2);
    for id in [&a, &b] {
        let j = outs.finished(id);
        assert_eq!(j.status, COMPLETE, "{id}: {:?}", j.error);
    }
    pool.shutdown();
}

#[test]
fn shutdown_joins_and_leaves_buffered_jobs_recoverable() {
    let env = env();
    let (r, ch) = rec(true, false);
    let (pool, mut outs) = start_pool(&env, &r, || 1, |c| c.handoff(1), |x| x);
    let ids: Vec<String> = (0..4).map(|_| submit(&env.store, json!({}))).collect();
    for id in &ids {
        pool.enqueue(id).unwrap();
    }
    // 1 is buffered, 2 is blocked sending, 3 is still queued.
    expect_all(&ch.ev, &[(true, ids[0].as_str()), (false, ids[2].as_str())]);
    // Shutdown waits for the compute in flight, so open the gate once it has begun.
    let t = thread::spawn(move || pool.shutdown());
    release(&ch.gate, 1);
    t.join().unwrap();
    assert_eq!(outs.finished(&ids[0]).status, COMPLETE);
    for id in &ids[1..] {
        let j = env.store.get(id).unwrap();
        assert!(
            !j.is_terminal(),
            "{id} must stay recoverable, is {}",
            j.status
        );
    }
    let (r2, ch2) = rec(false, false);
    let (pool2, mut outs2) = start_pool(&env, &r2, || 1, |c| c.handoff(1), |x| x);
    let queued = pool2.resume_recoverable().unwrap();
    assert_eq!(queued, ids[1..].to_vec());
    assert_eq!(compute_order(&ch2.ev, 3), ids[1..].to_vec());
    for id in &ids[1..] {
        assert_eq!(outs2.finished(id).status, COMPLETE);
    }
    pool2.shutdown();
}
