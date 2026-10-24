//! Attempt fencing: a stalled attempt's worker thread may outlive its
//! abandonment, but every write it attempts must be rejected once a newer
//! generation owns the job. Generations are durable across restarts.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use rightkit_job_queue::{
    Job, JobControl, JobError, JobHandler, JobLocks, JobResult, JobStore, NewJob, RunOutcome,
    Runner, Transition, COMPLETE, STALE_ATTEMPT,
};
use serde_json::{json, Value};

fn new_job() -> NewJob {
    NewJob {
        request_id: None,
        kind: "fence".into(),
        request: json!({}),
    }
}

fn code<T>(r: JobResult<T>) -> String {
    match r {
        Ok(_) => "ok".into(),
        Err(e) => e.code,
    }
}

type Report = mpsc::Receiver<(i64, Vec<String>)>;

/// Stall limit of the watchdog. Only the first attempt is meant to exceed it (it blocks on the
/// gate below, without a sign of life), so it costs one limit of wall time. Later attempts must
/// beat it under host load: their first store write can take well over the 100 ms this once was,
/// which fenced the second attempt too and paused it with a stale-generation error. 500 ms still
/// fenced it once on a hosted Windows runner (native run 40073029556); 2 s costs 2 s per test.
const STALL: Duration = Duration::from_secs(2);

/// First attempt stalls past the watchdog, then keeps writing through its
/// control (as a zombie worker would). Later attempts complete at once.
struct Zombie {
    calls: AtomicUsize,
    report: Mutex<mpsc::Sender<(i64, Vec<String>)>>,
    /// The zombie waits here until the test releases it (or drops the sender), so the order
    /// "new attempt finished, then the zombie writes" does not depend on a sleep.
    gate: Mutex<mpsc::Receiver<()>>,
}

impl JobHandler for Zombie {
    fn run(&self, _job: &Job, ctl: &JobControl) -> JobResult<Value> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if n == 1 {
            let gen = ctl.generation();
            let _ = self
                .gate
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(30));
            ctl.progress("late", None, Some("zombie progress"));
            let codes = vec![
                code(ctl.transition(&Transition::to(COMPLETE).result(json!({"late": true})))),
                code(ctl.set_ext(&Default::default())),
            ];
            let _ = self.report.lock().unwrap().send((gen, codes));
            return Err(JobError::new("late", "zombie finished"));
        }
        ctl.progress("work", Some(0.5), None);
        Ok(json!({"attempt": n}))
    }
}

/// The extra sender releases the zombie; dropping it releases it too.
fn rig(dir: &Path) -> (Arc<JobStore>, Runner, Report, mpsc::Sender<()>) {
    let store = Arc::new(JobStore::open(dir.join("jobs.db")).unwrap());
    let (tx, rx) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let handler = Arc::new(Zombie {
        calls: AtomicUsize::new(0),
        report: Mutex::new(tx),
        gate: Mutex::new(gate),
    });
    let runner = Runner::new(store.clone(), JobLocks::new(dir.join("locks")), handler)
        .with_stall_timeout(STALL);
    (store, runner, rx, release)
}

#[test]
fn abandoned_worker_writes_are_rejected_and_new_attempt_proceeds() {
    let dir = tempfile::tempdir().unwrap();
    let (store, runner, rx, release) = rig(dir.path());
    let (job, _) = store.submit(&new_job()).unwrap();
    let id = job.job_id.clone();

    let first = runner.run(&id).unwrap();
    assert!(
        matches!(first, RunOutcome::Paused(_)),
        "stall pauses the run"
    );
    assert_eq!(
        store.generation(&id).unwrap(),
        2,
        "the watchdog fences attempt 1 (generation 1 -> 2) before releasing ownership"
    );

    // A new attempt takes ownership and completes.
    match runner.run(&id).unwrap() {
        RunOutcome::Finished(j) => assert_eq!(j.status, COMPLETE),
        other => panic!("second attempt should finish, got {other:?}"),
    }
    assert_eq!(store.get(&id).unwrap().result, Some(json!({"attempt": 2})));

    // The zombie wakes after the new attempt finished: every write is fenced.
    release.send(()).unwrap();
    let (gen, codes) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(gen, 1);
    assert_eq!(codes, vec![STALE_ATTEMPT, STALE_ATTEMPT]);

    let after = store.get(&id).unwrap();
    assert_eq!(after.status, COMPLETE);
    assert_eq!(
        after.result,
        Some(json!({"attempt": 2})),
        "zombie result must not land"
    );
    let stages: Vec<String> = store
        .events_after(&id, 0)
        .unwrap()
        .into_iter()
        .map(|e| e.stage)
        .collect();
    assert!(
        !stages.iter().any(|s| s == "late"),
        "zombie progress rejected: {stages:?}"
    );
}

#[test]
fn stale_generation_rejected_at_store_level_for_every_write_kind() {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::open(dir.path().join("jobs.db")).unwrap();
    let (job, _) = store.submit(&new_job()).unwrap();
    let id = job.job_id.clone();

    let (_, gen1) = store.begin_run_fenced(&id, true).unwrap().unwrap();
    assert_eq!(gen1, 1);
    let gen2 = store.fence(&id, gen1).unwrap();
    assert_eq!(gen2, 2);

    let art = dir.path().join("out.txt");
    std::fs::write(&art, b"x").unwrap();
    let e = JobError::new("x", "x");
    let stale = |r: JobResult<()>| r.unwrap_err().code;
    assert_eq!(
        stale(
            store
                .transition_fenced(&id, gen1, &Transition::to(COMPLETE))
                .map(|_| ())
        ),
        STALE_ATTEMPT
    );
    assert_eq!(
        stale(
            store
                .set_ext_fenced(&id, gen1, &Default::default())
                .map(|_| ())
        ),
        STALE_ATTEMPT
    );
    assert_eq!(
        stale(store.record_error_fenced(&id, gen1, &e)),
        STALE_ATTEMPT
    );
    assert_eq!(stale(store.refund_attempt_fenced(&id, gen1)), STALE_ATTEMPT);
    assert_eq!(
        stale(
            store
                .add_event_fenced(&id, gen1, "s", None, None)
                .map(|_| ())
        ),
        STALE_ATTEMPT
    );
    assert_eq!(
        stale(
            store
                .add_artifact_fenced(&id, gen1, &art, "text/plain")
                .map(|_| ())
        ),
        STALE_ATTEMPT
    );
    assert!(
        store.artifacts(&id).unwrap().is_empty(),
        "stale artifact not indexed"
    );
    assert_eq!(
        store.get(&id).unwrap().status,
        "running",
        "stale complete not applied"
    );

    // The current generation still writes.
    store
        .add_artifact_fenced(&id, gen2, &art, "text/plain")
        .unwrap();
    assert_eq!(store.artifacts(&id).unwrap().len(), 1);
}

#[test]
fn generation_survives_restart_and_keeps_fencing_old_attempts() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("jobs.db");
    let id = {
        let store = JobStore::open(&db).unwrap();
        let (job, _) = store.submit(&new_job()).unwrap();
        let (_, gen) = store.begin_run_fenced(&job.job_id, true).unwrap().unwrap();
        assert_eq!(gen, 1);
        assert_eq!(store.fence(&job.job_id, gen).unwrap(), 2);
        job.job_id
        // store dropped: simulates a crash after the fence committed
    };
    let reopened = JobStore::open(&db).unwrap();
    assert_eq!(
        reopened.generation(&id).unwrap(),
        2,
        "generation is durable"
    );
    let stale = reopened
        .transition_fenced(&id, 1, &Transition::to(COMPLETE))
        .unwrap_err();
    assert_eq!(
        stale.code, STALE_ATTEMPT,
        "pre-restart attempt stays fenced"
    );
    let (_, next) = reopened.begin_run_fenced(&id, true).unwrap().unwrap();
    assert_eq!(
        next, 3,
        "the next run continues from the durable generation"
    );
}

#[test]
fn second_attempt_completes_under_the_current_generation() {
    let dir = tempfile::tempdir().unwrap();
    let (store, runner, _rx, _release) = rig(dir.path());
    // The first attempt stalls and is fenced; the second completes normally.
    let (job, _) = store.submit(&new_job()).unwrap();
    let id = job.job_id.clone();
    let _ = runner.run(&id).unwrap();
    let finished = runner.run(&id).unwrap();
    let RunOutcome::Finished(j) = finished else {
        panic!("expected finished");
    };
    assert_eq!(j.status, COMPLETE);
    assert_eq!(
        store.generation(&id).unwrap(),
        3,
        "run 2 began at 3 after the fence"
    );
    let events = store.events_after(&id, 0).unwrap();
    assert!(
        events.iter().any(|e| e.stage == "work" && e.attempt == 2),
        "current attempt's progress is stored"
    );
}
