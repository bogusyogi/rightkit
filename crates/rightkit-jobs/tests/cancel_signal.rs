//! The in-memory cancel token is the fast path; the database flag stays the source of truth.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use rightkit_jobs::{
    Job, JobControl, JobError, JobHandler, JobLocks, JobResult, JobStore, NewJob, RunOutcome,
    Runner, CANCELLED,
};
use serde_json::{json, Value};

fn new_job() -> NewJob {
    NewJob {
        request_id: None,
        kind: "scan".into(),
        request: json!({}),
    }
}

/// Spins on the token only (never reads the database) until it is set or a deadline passes.
struct TokenSpinner {
    entered: Mutex<mpsc::Sender<()>>,
    saw_token: AtomicBool,
}

impl JobHandler for TokenSpinner {
    fn run(&self, _job: &Job, ctl: &JobControl) -> JobResult<Value> {
        let token = ctl.cancel_token();
        assert!(!token.is_cancelled());
        self.entered.lock().unwrap().send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if token.is_cancelled() {
                self.saw_token.store(true, Ordering::SeqCst);
                return Err(JobError::cancelled());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(json!({"timed_out": true}))
    }
}

#[test]
fn in_process_cancel_sets_the_token_the_handler_polls() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(JobStore::open(dir.path().join("j.db")).unwrap());
    let (tx, rx) = mpsc::channel();
    let handler = Arc::new(TokenSpinner {
        entered: Mutex::new(tx),
        saw_token: AtomicBool::new(false),
    });
    let runner = Arc::new(Runner::new(
        store.clone(),
        JobLocks::new(dir.path().join("locks")),
        handler.clone(),
    ));
    let (job, _) = store.submit(&new_job()).unwrap();
    let id = job.job_id;
    let t = {
        let (runner, id) = (runner.clone(), id.clone());
        std::thread::spawn(move || runner.run(&id).unwrap())
    };
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    store.cancel(&id).unwrap();
    let out = t.join().unwrap();
    assert!(handler.saw_token.load(Ordering::SeqCst), "token fired");
    assert!(matches!(out, RunOutcome::Finished(j) if j.status == CANCELLED));
}

/// Records what a handler sees when the cancel arrives through another store handle (another
/// process in production): the token cannot know, the database check does, and then the token follows.
struct CrossHandle {
    other: Arc<JobStore>,
    seen: Mutex<Vec<(bool, bool, bool)>>,
}

impl JobHandler for CrossHandle {
    fn run(&self, job: &Job, ctl: &JobControl) -> JobResult<Value> {
        let token = ctl.cancel_token();
        self.other.cancel(&job.job_id).unwrap();
        let before = token.is_cancelled();
        let db = ctl.is_cancelled();
        let after = token.is_cancelled();
        self.seen.lock().unwrap().push((before, db, after));
        assert!(matches!(ctl.abort_reason(), Some(e) if e.code == "cancelled"));
        Err(JobError::cancelled())
    }
}

#[test]
fn cross_process_cancel_is_found_through_the_database_and_then_sets_the_token() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("j.db");
    let store = Arc::new(JobStore::open(&path).unwrap());
    let other = Arc::new(JobStore::open(&path).unwrap());
    let handler = Arc::new(CrossHandle {
        other,
        seen: Mutex::new(vec![]),
    });
    let runner = Runner::new(
        store.clone(),
        JobLocks::new(dir.path().join("locks")),
        handler.clone(),
    );
    let (job, _) = store.submit(&new_job()).unwrap();
    runner.run(&job.job_id).unwrap();
    assert_eq!(*handler.seen.lock().unwrap(), vec![(false, true, true)]);
    assert_eq!(store.get(&job.job_id).unwrap().status, CANCELLED);
}

#[test]
fn token_is_shared_flag_and_cloneable() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(JobStore::open(dir.path().join("j.db")).unwrap());
    let (job, _) = store.submit(&new_job()).unwrap();
    let a = store.cancel_token(&job.job_id);
    let b = a.clone();
    let flag = a.flag();
    assert!(!b.is_cancelled() && !flag.load(Ordering::SeqCst));
    store.cancel(&job.job_id).unwrap();
    assert!(a.is_cancelled() && b.is_cancelled() && flag.load(Ordering::SeqCst));
}
