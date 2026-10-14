//! Single-flight, latest-wins submit keyed by a caller string.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use rightkit_job_queue::{
    Job, JobControl, JobHandler, JobLocks, JobResult, JobStore, NewJob, RunOutcome, Runner,
    SubmitOptions, CANCELLED, COMPLETE, SUPERSEDED,
};
use serde_json::{json, Value};

fn new_job(tag: &str) -> NewJob {
    NewJob {
        request_id: None,
        kind: "scan".into(),
        request: json!({"tag": tag}),
    }
}

fn flight(key: &str) -> SubmitOptions {
    SubmitOptions::new().single_flight(key)
}

fn open(dir: &std::path::Path) -> Arc<JobStore> {
    Arc::new(JobStore::open(dir.join("j.db")).unwrap())
}

struct Counter(AtomicUsize);

impl JobHandler for Counter {
    fn run(&self, _job: &Job, _ctl: &JobControl) -> JobResult<Value> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(json!({}))
    }
}

#[test]
fn queued_predecessor_is_superseded_and_never_handed_to_a_handler() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let a = store
        .submit_with(&new_job("a"), &Default::default(), &flight("scan:home"))
        .unwrap();
    assert!(a.superseded.is_empty());
    let b = store
        .submit_with(&new_job("b"), &Default::default(), &flight("scan:home"))
        .unwrap();
    let (a_id, b_id) = (a.record.job.job_id, b.record.job.job_id);
    assert_ne!(a_id, b_id);
    assert_eq!(b.superseded, vec![a_id.clone()]);

    let old = store.get(&a_id).unwrap();
    assert_eq!(old.status, CANCELLED);
    let err = old.error.unwrap();
    assert_eq!(err["code"], SUPERSEDED);
    assert!(err["message"].as_str().unwrap().contains(&b_id));
    assert!(!store.get(&b_id).unwrap().is_terminal());

    let counter = Arc::new(Counter(AtomicUsize::new(0)));
    let runner = Runner::new(
        store.clone(),
        JobLocks::new(dir.path().join("locks")),
        counter.clone(),
    );
    assert!(matches!(runner.run(&a_id).unwrap(), RunOutcome::Finished(j) if j.status == CANCELLED));
    assert_eq!(counter.0.load(Ordering::SeqCst), 0);
    assert!(matches!(runner.run(&b_id).unwrap(), RunOutcome::Finished(j) if j.status == COMPLETE));
}

/// Runs until its token fires, then returns a late `Ok` result to prove it is discarded.
struct Late {
    entered: Mutex<mpsc::Sender<()>>,
}

impl JobHandler for Late {
    fn run(&self, _job: &Job, ctl: &JobControl) -> JobResult<Value> {
        let token = ctl.cancel_token();
        self.entered.lock().unwrap().send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !token.is_cancelled() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            token.is_cancelled(),
            "superseded predecessor must be told to stop"
        );
        Ok(json!({"stale": true}))
    }
}

#[test]
fn running_predecessor_is_cancelled_signalled_and_its_late_result_discarded() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let (tx, rx) = mpsc::channel();
    let runner = Arc::new(Runner::new(
        store.clone(),
        JobLocks::new(dir.path().join("locks")),
        Arc::new(Late {
            entered: Mutex::new(tx),
        }),
    ));
    let a = store
        .submit_with(&new_job("a"), &Default::default(), &flight("k"))
        .unwrap()
        .record
        .job
        .job_id;
    let t = {
        let (runner, a) = (runner.clone(), a.clone());
        std::thread::spawn(move || runner.run(&a).unwrap())
    };
    rx.recv_timeout(Duration::from_secs(5)).unwrap();

    let b = store
        .submit_with(&new_job("b"), &Default::default(), &flight("k"))
        .unwrap();
    assert_eq!(b.superseded, vec![a.clone()]);
    // The predecessor is cancelled in the database at once, before its handler has stopped.
    let now = store.get(&a).unwrap();
    assert_eq!(now.status, CANCELLED);
    assert_eq!(now.error.unwrap()["code"], SUPERSEDED);

    let out = t.join().unwrap();
    assert!(matches!(out, RunOutcome::Finished(j) if j.status == CANCELLED && j.result.is_none()));
    assert_eq!(
        store.get(&a).unwrap().result,
        None,
        "the stale result was discarded"
    );
    assert!(!store.get(&b.record.job.job_id).unwrap().is_terminal());
}

#[test]
fn keys_are_independent_and_terminal_jobs_are_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let a = store
        .submit_with(&new_job("a"), &Default::default(), &flight("x"))
        .unwrap()
        .record
        .job
        .job_id;
    let other = store
        .submit_with(&new_job("o"), &Default::default(), &flight("y"))
        .unwrap();
    assert!(other.superseded.is_empty());
    let plain = store.submit(&new_job("p")).unwrap().0;
    store.complete(&a, &json!({"done": true})).unwrap();
    let b = store
        .submit_with(&new_job("b"), &Default::default(), &flight("x"))
        .unwrap();
    assert!(b.superseded.is_empty(), "a finished job is not superseded");
    assert_eq!(store.get(&a).unwrap().status, COMPLETE);
    assert!(!store.get(&other.record.job.job_id).unwrap().is_terminal());
    assert!(!store.get(&plain.job_id).unwrap().is_terminal());
}

#[test]
fn chain_leaves_only_the_latest_alive() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let ids: Vec<String> = (0..4)
        .map(|i| {
            store
                .submit_with(&new_job(&i.to_string()), &Default::default(), &flight("k"))
                .unwrap()
                .record
                .job
                .job_id
        })
        .collect();
    assert_eq!(store.non_terminal_ids().unwrap(), vec![ids[3].clone()]);
}

#[test]
fn deduplicated_submit_supersedes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let mut n = new_job("a");
    n.request_id = Some("req-1".into());
    let first = store
        .submit_with(&n, &Default::default(), &flight("k"))
        .unwrap();
    let again = store
        .submit_with(&n, &Default::default(), &flight("k"))
        .unwrap();
    assert!(again.deduplicated && again.superseded.is_empty());
    assert_eq!(again.record.job.job_id, first.record.job.job_id);
    assert!(!store.get(&first.record.job.job_id).unwrap().is_terminal());
}
