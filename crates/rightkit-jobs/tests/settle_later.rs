//! Settle-later: a terminal write that storage refuses is held (with the job's claim), retried
//! on later store activity and on an explicit flush, journaled for a restart, and reported at
//! shutdown. Storage failures are injected through a `StoreHook` that fails terminal
//! transitions with a `db` error, which rolls the transaction back exactly as a busy or failing
//! database would.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rightkit_jobs::rusqlite::Connection;
use rightkit_jobs::{
    Event, EventSink, HandlerOutcome, Job, JobControl, JobError, JobHandler, JobLocks, JobRecord,
    JobResult, JobStore, NewJob, RunOutcome, Runner, Settlement, StoreConfig, StoreHook,
    Transition, UnsettledRecovery, COMPLETE, MAX_PENDING_SETTLEMENTS, RUNNING,
};
use serde_json::{json, Value};

fn new_job(mode: &str) -> NewJob {
    NewJob {
        request_id: None,
        kind: "settle".into(),
        request: json!({"mode": mode}),
    }
}

/// Fails every terminal transition with `code` while `fail` is on.
struct Flaky {
    fail: AtomicBool,
    code: &'static str,
}

impl Flaky {
    fn new(fail: bool) -> Arc<Self> {
        Self::with_code(fail, "db")
    }
    fn with_code(fail: bool, code: &'static str) -> Arc<Self> {
        Arc::new(Self {
            fail: AtomicBool::new(fail),
            code,
        })
    }
    fn set(&self, fail: bool) {
        self.fail.store(fail, Ordering::SeqCst);
    }
}

impl StoreHook for Flaky {
    fn on_transition(&self, _c: &Connection, _b: &JobRecord, a: &JobRecord) -> JobResult<()> {
        if self.fail.load(Ordering::SeqCst) && a.job.is_terminal() {
            return Err(JobError::new(self.code, "database is locked (injected)"));
        }
        Ok(())
    }
}

/// Counts handler calls and completes (or settles through the control) as the request says.
#[derive(Default)]
struct Work {
    calls: AtomicUsize,
}

impl JobHandler for Work {
    fn run(&self, _job: &Job, _ctl: &JobControl) -> JobResult<Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"n": 1}))
    }

    fn run_outcome(&self, job: &Job, ctl: &Arc<JobControl>) -> JobResult<HandlerOutcome> {
        if job.request["mode"] == "settles_itself" {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let t = Transition::to(COMPLETE)
                .result(json!({"by": "handler"}))
                .error(None);
            // Written, or held by the store when storage refuses it: either way the handler is done.
            match ctl.settle(&t)? {
                Settlement::Written(_) | Settlement::Deferred => Ok(HandlerOutcome::Settled),
            }
        } else {
            self.run_shared(job, ctl).map(HandlerOutcome::Done)
        }
    }
}

struct Rig {
    dir: tempfile::TempDir,
    flaky: Arc<Flaky>,
    store: Arc<JobStore>,
    locks: JobLocks,
    work: Arc<Work>,
    events: Arc<Mutex<Vec<Event>>>,
    sink: EventSink,
}

fn open(dir: &Path, flaky: &Arc<Flaky>) -> Arc<JobStore> {
    let cfg = StoreConfig::new().hook(flaky.clone());
    Arc::new(JobStore::open_configured(dir.join("jobs.db"), cfg).unwrap())
}

fn rig(fail: bool) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let flaky = Flaky::new(fail);
    let store = open(dir.path(), &flaky);
    let locks = JobLocks::new(dir.path().join("locks"));
    let events: Arc<Mutex<Vec<Event>>> = Arc::default();
    let seen = events.clone();
    let sink: EventSink = Arc::new(move |ev: &Event| seen.lock().unwrap().push(ev.clone()));
    Rig {
        dir,
        flaky,
        store,
        locks,
        work: Arc::new(Work::default()),
        events,
        sink,
    }
}

impl Rig {
    fn runner(&self) -> Runner {
        Runner::new(self.store.clone(), self.locks.clone(), self.work.clone())
            .with_sink(self.sink.clone())
    }
    fn submit(&self, mode: &str) -> String {
        self.store.submit(&new_job(mode)).unwrap().0.job_id
    }
    fn complete_events(&self, id: &str) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.job_id == id && e.stage == "complete")
            .count()
    }
    fn journal(&self) -> std::path::PathBuf {
        self.dir.path().join("jobs.db.settlements")
    }
}

fn journal_files(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
                .count()
        })
        .unwrap_or(0)
}

#[test]
fn a_failed_terminal_write_is_held_with_its_claim_and_lands_on_flush() {
    let rig = rig(true);
    let id = rig.submit("work");
    let runner = rig.runner();

    // The write is refused: the outcome is not lost and not turned into another state.
    let RunOutcome::Unsettled(job) = runner.run(&id).unwrap() else {
        panic!("expected Unsettled");
    };
    assert_eq!(job.status, COMPLETE);
    assert_eq!(job.result, Some(json!({"n": 1})));
    assert_eq!(
        rig.store.get(&id).unwrap().status,
        RUNNING,
        "not stored yet"
    );
    let held = runner.pending_settlements();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].job_id, id);
    assert_eq!(held[0].status, COMPLETE);
    assert!(held[0].claimed && held[0].durable, "{held:?}");
    assert_eq!(held[0].attempts, 0);
    assert_eq!(journal_files(&rig.journal()), 1);

    // The claim is held: nobody runs the finished job again.
    assert!(rig.locks.try_lock(&id).unwrap().is_none());
    assert!(matches!(
        runner.run(&id).unwrap(),
        RunOutcome::LockedElsewhere(_)
    ));
    assert!(runner.recoverable_ids().unwrap().is_empty());
    assert!(runner.resume_recoverable().unwrap().is_empty());
    assert_eq!(rig.work.calls.load(Ordering::SeqCst), 1);

    // Still refused: still pending, the attempt counted.
    let report = runner.flush_pending_settlements();
    assert_eq!((report.settled, report.pending), (0, 1));
    assert_eq!(runner.pending_settlements()[0].attempts, 1);
    assert_eq!(rig.complete_events(&id), 0);

    // Storage recovers: one explicit flush lands the write exactly as the runner meant it.
    rig.flaky.set(false);
    let report = runner.flush_pending_settlements();
    assert_eq!((report.settled, report.pending), (1, 0));
    assert!(report.dropped.is_empty());
    let row = rig.store.get(&id).unwrap();
    assert_eq!(row.status, COMPLETE);
    assert_eq!(row.result, Some(json!({"n": 1})));
    // The terminal event was written with the state, and reaches the sink once, now.
    let stages: Vec<String> = rig
        .store
        .events_after(&id, 0)
        .unwrap()
        .into_iter()
        .map(|e| e.stage)
        .collect();
    assert_eq!(stages, vec!["started", "complete"]);
    assert_eq!(rig.complete_events(&id), 1);
    // Claim released, journal cleaned, nothing pending.
    assert!(rig.locks.try_lock(&id).unwrap().is_some());
    assert_eq!(journal_files(&rig.journal()), 0);
    assert!(runner.pending_settlements().is_empty());
    assert_eq!(rig.work.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn the_next_successful_store_write_retries_a_held_settlement() {
    let rig = rig(true);
    let id = rig.submit("work");
    let runner = rig.runner();
    assert!(matches!(runner.run(&id).unwrap(), RunOutcome::Unsettled(_)));
    assert_eq!(rig.store.get(&id).unwrap().status, RUNNING);

    // No flush call, no timer: an ordinary successful write is the retry.
    rig.flaky.set(false);
    rig.submit("other");
    assert_eq!(rig.store.get(&id).unwrap().status, COMPLETE);
    assert!(runner.pending_settlements().is_empty());
    assert_eq!(rig.complete_events(&id), 1);
}

#[test]
fn shutdown_persists_what_it_can_and_reports_the_rest() {
    let rig = rig(true);
    let id = rig.submit("work");
    let runner = rig.runner();
    assert!(matches!(runner.run(&id).unwrap(), RunOutcome::Unsettled(_)));

    // Storage is still down at shutdown: the job is reported as finished and unsettled.
    let down = runner.shutdown_detailed();
    assert_eq!(down.settled, 0);
    assert!(!down.is_clean());
    assert_eq!(down.unsettled.len(), 1);
    assert_eq!(down.unsettled[0].job_id, id);
    assert_eq!(down.unsettled[0].status, COMPLETE);
    assert!(
        down.unsettled[0].durable,
        "the journal holds it for the next open"
    );

    // Storage is back: the shutdown flush stores it and the report is clean.
    rig.flaky.set(false);
    let up = runner.shutdown_detailed();
    assert_eq!(up.settled, 1);
    assert!(up.is_clean());
    assert_eq!(rig.store.get(&id).unwrap().status, COMPLETE);
}

#[test]
fn a_restart_applies_the_journal_instead_of_running_the_finished_job_again() {
    let rig = rig(true);
    let id = rig.submit("work");
    let runner = rig.runner();
    assert!(matches!(runner.run(&id).unwrap(), RunOutcome::Unsettled(_)));
    assert_eq!(rig.work.calls.load(Ordering::SeqCst), 1);
    assert_eq!(rig.store.get(&id).unwrap().status, RUNNING);

    // The process goes away with the write still pending.
    drop(runner);
    let Rig {
        dir, work, store, ..
    } = rig;
    drop(store);

    // The next open reads the journal and writes the outcome the handler reached.
    let healthy = Flaky::new(false);
    let reopened = open(dir.path(), &healthy);
    let row = reopened.get(&id).unwrap();
    assert_eq!(row.status, COMPLETE);
    assert_eq!(row.result, Some(json!({"n": 1})));
    assert_eq!(journal_files(&dir.path().join("jobs.db.settlements")), 0);

    // Recovery has nothing to run: the handler is not called a second time.
    let runner = Runner::new(
        reopened.clone(),
        JobLocks::new(dir.path().join("locks")),
        work.clone(),
    );
    assert!(runner.recoverable_ids().unwrap().is_empty());
    assert!(runner.resume_recoverable().unwrap().is_empty());
    assert_eq!(work.calls.load(Ordering::SeqCst), 1);
    let stages: Vec<String> = reopened
        .events_after(&id, 0)
        .unwrap()
        .into_iter()
        .map(|e| e.stage)
        .collect();
    assert_eq!(stages, vec!["started", "complete"]);
}

#[test]
fn a_journaled_outcome_that_still_cannot_be_written_parks_the_job_by_default() {
    let rig = rig(true);
    let id = rig.submit("work");
    let runner = rig.runner();
    assert!(matches!(runner.run(&id).unwrap(), RunOutcome::Unsettled(_)));
    drop(runner);
    let Rig {
        dir, work, store, ..
    } = rig;
    drop(store);

    // The database is still refusing at the next open: the entry comes back unclaimed.
    let down = Flaky::new(true);
    let reopened = open(dir.path(), &down);
    assert_eq!(reopened.get(&id).unwrap().status, RUNNING);
    let held = reopened.pending_settlements();
    assert_eq!(held.len(), 1);
    assert!(!held[0].claimed && held[0].durable);

    // Default: never run a handler that reported success again.
    let locks = JobLocks::new(dir.path().join("locks"));
    let runner = Runner::new(reopened.clone(), locks.clone(), work.clone());
    assert!(runner.recoverable_ids().unwrap().is_empty());
    assert!(runner.resume_recoverable().unwrap().is_empty());
    assert!(matches!(
        runner.run(&id).unwrap(),
        RunOutcome::Unsettled(j) if j.status == COMPLETE
    ));
    assert_eq!(work.calls.load(Ordering::SeqCst), 1);

    // Configured the other way, the held outcome is discarded and the job is recovered.
    down.set(false);
    let rerun = Runner::new(reopened.clone(), locks, work.clone())
        .with_unsettled_recovery(UnsettledRecovery::Rerun);
    assert_eq!(rerun.recoverable_ids().unwrap(), vec![id.clone()]);
    assert!(rerun.pending_settlements().is_empty());
    assert_eq!(journal_files(&dir.path().join("jobs.db.settlements")), 0);
    let outcomes = rerun.resume_recoverable().unwrap();
    assert!(matches!(&outcomes[0], RunOutcome::Finished(j) if j.status == COMPLETE));
    assert_eq!(work.calls.load(Ordering::SeqCst), 2);
}

#[test]
fn a_handler_can_defer_its_own_terminal_write() {
    let rig = rig(true);
    let id = rig.submit("settles_itself");
    let runner = rig.runner();

    // The handler's write was refused and held; it returned Settled. The runner takes the claim.
    let RunOutcome::Unsettled(job) = runner.run(&id).unwrap() else {
        panic!("expected Unsettled");
    };
    assert_eq!(job.status, COMPLETE);
    assert_eq!(job.result, Some(json!({"by": "handler"})));
    let held = runner.pending_settlements();
    assert_eq!(held.len(), 1);
    assert!(held[0].claimed);
    assert!(rig.locks.try_lock(&id).unwrap().is_none());
    assert!(runner.recoverable_ids().unwrap().is_empty());

    rig.flaky.set(false);
    assert_eq!(runner.flush_pending_settlements().settled, 1);
    let row = rig.store.get(&id).unwrap();
    assert_eq!(row.status, COMPLETE);
    assert_eq!(row.result, Some(json!({"by": "handler"})));
    // The runner's own announcement of a handler-settled job follows the write.
    let stored: Vec<(String, Option<String>)> = rig
        .store
        .events_after(&id, 0)
        .unwrap()
        .into_iter()
        .map(|e| (e.stage, e.message))
        .collect();
    assert_eq!(
        stored,
        vec![
            ("started".to_string(), None),
            (
                "complete".to_string(),
                Some("settled by handler".to_string())
            )
        ]
    );
    assert_eq!(rig.work.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn errors_that_are_not_storage_failures_are_not_held() {
    // A hook refusal (a spend cap, say) is the app's decision: it is returned, never retried.
    let rig = {
        let dir = tempfile::tempdir().unwrap();
        let flaky = Flaky::with_code(true, "spend_cap");
        let store = open(dir.path(), &flaky);
        let locks = JobLocks::new(dir.path().join("locks"));
        Rig {
            dir,
            flaky,
            store,
            locks,
            work: Arc::new(Work::default()),
            events: Arc::default(),
            sink: Arc::new(|_: &Event| {}),
        }
    };
    let id = rig.submit("work");
    let runner = rig.runner();
    assert_eq!(runner.run(&id).unwrap_err().code, "spend_cap");
    assert!(runner.pending_settlements().is_empty());
    assert_eq!(journal_files(&rig.journal()), 0);
    assert_eq!(rig.store.get(&id).unwrap().status, RUNNING);
}

#[test]
fn settle_later_can_be_switched_off() {
    let rig = rig(true);
    let id = rig.submit("work");
    let runner = rig.runner().with_settle_later(false);
    assert!(!runner.settle_later());

    // 0.2.9 behaviour: the error is returned, the job stays running and its lock is released.
    assert_eq!(runner.run(&id).unwrap_err().code, "db");
    assert!(runner.pending_settlements().is_empty());
    assert_eq!(journal_files(&rig.journal()), 0);
    assert!(rig.locks.try_lock(&id).unwrap().is_some());
    assert_eq!(runner.recoverable_ids().unwrap(), vec![id]);
}

#[test]
fn the_held_set_is_bounded() {
    let rig = rig(true);
    let runner = rig.runner();
    let ids: Vec<String> = (0..MAX_PENDING_SETTLEMENTS + 1)
        .map(|i| rig.submit(&format!("work{i}")))
        .collect();
    let mut held = 0;
    let mut refused = 0;
    for id in &ids {
        match runner.run(id) {
            Ok(RunOutcome::Unsettled(_)) => held += 1,
            Err(e) => {
                assert_eq!(e.code, "db");
                refused += 1;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!((held, refused), (MAX_PENDING_SETTLEMENTS, 1));
    assert_eq!(runner.pending_settlements().len(), MAX_PENDING_SETTLEMENTS);
}
