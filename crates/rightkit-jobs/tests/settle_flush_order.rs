//! An owner settle and a flush of held writes never interleave: a flush that is committing the
//! runner's held outcome finishes first, and the owner is then told the truth (the row is
//! already terminal) instead of `Deferred` for an outcome the flush would drop. The flush is
//! paused inside its commit with a `StoreHook`.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use rightkit_jobs::rusqlite::Connection;
use rightkit_jobs::{
    Job, JobControl, JobError, JobHandler, JobLocks, JobRecord, JobResult, JobStore, NewJob,
    RunOutcome, Runner, StoreConfig, StoreHook, Transition, CANCELLED, FAILED,
};
use serde_json::{json, Value};

/// Fails terminal writes while `fail` is set; pauses the next one while `pause` is set.
struct Gate {
    fail: AtomicBool,
    pause: AtomicBool,
    entered: Mutex<mpsc::Sender<()>>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl StoreHook for Gate {
    fn on_transition(&self, _c: &Connection, _b: &JobRecord, a: &JobRecord) -> JobResult<()> {
        if !a.job.is_terminal() {
            return Ok(());
        }
        if self.fail.load(Ordering::SeqCst) {
            return Err(JobError::new("db", "database is locked (injected)"));
        }
        if self.pause.swap(false, Ordering::SeqCst) {
            self.entered.lock().unwrap().send(()).unwrap();
            let _ = self
                .release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10));
        }
        Ok(())
    }
}

struct Boom;

impl JobHandler for Boom {
    fn run(&self, _job: &Job, _ctl: &JobControl) -> JobResult<Value> {
        Err(JobError::new("boom", "generic failure"))
    }
}

fn journal_files(dir: &Path) -> usize {
    std::fs::read_dir(dir.join("jobs.db.settlements"))
        .map(|rd| rd.flatten().count())
        .unwrap_or(0)
}

#[test]
fn an_owner_settle_waits_for_a_flush_that_is_committing_the_held_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let gate = Arc::new(Gate {
        fail: AtomicBool::new(true),
        pause: AtomicBool::new(false),
        entered: Mutex::new(entered_tx),
        release: Mutex::new(release_rx),
    });
    let store = Arc::new(
        JobStore::open_configured(
            dir.path().join("jobs.db"),
            StoreConfig::new().hook(gate.clone()),
        )
        .unwrap(),
    );
    let runner = Runner::new(
        store.clone(),
        JobLocks::new(dir.path().join("locks")),
        Arc::new(Boom),
    );
    let id = store
        .submit(&NewJob {
            request_id: None,
            kind: "order".into(),
            request: json!({}),
        })
        .unwrap()
        .0
        .job_id;

    // The runner's FAILED is refused by storage and held.
    let RunOutcome::Unsettled(job) = runner.run(&id).unwrap() else {
        panic!("expected Unsettled");
    };
    assert_eq!(job.status, FAILED);

    // Storage works again; the flush stops inside its commit of the held FAILED.
    gate.fail.store(false, Ordering::SeqCst);
    gate.pause.store(true, Ordering::SeqCst);
    let flushing = {
        let store = store.clone();
        std::thread::spawn(move || store.flush_pending_settlements())
    };
    entered_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the flush reached its commit");

    // The owner settles the same job meanwhile.
    let (done_tx, done_rx) = mpsc::channel();
    let owner = {
        let (store, id) = (store.clone(), id.clone());
        std::thread::spawn(move || {
            let cancelled = Transition::to(CANCELLED).error(Some(
                json!({"code": "cancelled", "message": "by the user", "retryable": false}),
            ));
            let out = store.settle_terminal(&id, &cancelled);
            done_tx.send(()).unwrap();
            out
        })
    };
    assert!(
        done_rx.recv_timeout(Duration::from_millis(200)).is_err(),
        "the owner settle finished while the flush was still committing"
    );

    release_tx.send(()).unwrap();
    let report = flushing.join().unwrap();
    assert_eq!(report.settled, 1);
    let error = owner
        .join()
        .unwrap()
        .expect_err("the row is already failed; a different outcome is refused, not held");
    assert_eq!(error.code, "illegal_transition");
    assert_eq!(store.record(&id).unwrap().job.status, FAILED);
    assert!(store.pending_settlements().is_empty());
    assert_eq!(journal_files(dir.path()), 0);
}
