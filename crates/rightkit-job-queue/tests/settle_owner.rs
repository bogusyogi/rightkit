//! An explicit owner settle is not discarded in favour of an earlier held outcome, and a journal
//! replay inside a run hands back the restored row. Storage failure is injected with a
//! `StoreHook` that fails terminal transitions with a `db` error.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rightkit_job_queue::rusqlite::Connection;
use rightkit_job_queue::{
    Event, EventSink, ExtColumn, HandlerOutcome, Job, JobControl, JobError, JobHandler, JobLocks,
    JobRecord, JobResult, JobStore, NewJob, RunOutcome, Runner, Settlement, StoreConfig, StoreHook,
    Transition, CANCELLED, COMPLETE, FAILED, RUNNING,
};
use serde_json::{json, Value};

struct Flaky {
    fail: AtomicBool,
}

impl StoreHook for Flaky {
    fn on_transition(&self, _c: &Connection, _b: &JobRecord, a: &JobRecord) -> JobResult<()> {
        if self.fail.load(Ordering::SeqCst) && a.job.is_terminal() {
            return Err(JobError::new("db", "database is locked (injected)"));
        }
        Ok(())
    }
}

fn flaky(fail: bool) -> Arc<Flaky> {
    Arc::new(Flaky {
        fail: AtomicBool::new(fail),
    })
}

fn open(dir: &Path, hook: &Arc<Flaky>) -> Arc<JobStore> {
    let cfg = StoreConfig::new()
        .column(ExtColumn::json("info"))
        .hook(hook.clone());
    Arc::new(JobStore::open_configured(dir.join("jobs.db"), cfg).unwrap())
}

/// Fails with a generic error, or settles itself with extension values, as the request says.
struct Handler;

impl JobHandler for Handler {
    fn run(&self, job: &Job, _ctl: &JobControl) -> JobResult<Value> {
        match job.request["mode"].as_str() {
            Some("boom") => Err(JobError::new("boom", "generic failure")),
            _ => Ok(json!({})),
        }
    }

    fn run_outcome(&self, job: &Job, ctl: &Arc<JobControl>) -> JobResult<HandlerOutcome> {
        if job.request["mode"] == "settles_itself" {
            let t = Transition::to(COMPLETE)
                .result(json!({"by": "handler"}))
                .error(None)
                .set("info", json!({"restored": true}));
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
    hook: Arc<Flaky>,
    store: Arc<JobStore>,
    events: Arc<Mutex<Vec<Event>>>,
    runner: Runner,
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let hook = flaky(true);
    let store = open(dir.path(), &hook);
    let events: Arc<Mutex<Vec<Event>>> = Arc::default();
    let seen = events.clone();
    let sink: EventSink = Arc::new(move |ev: &Event| seen.lock().unwrap().push(ev.clone()));
    let runner = Runner::new(
        store.clone(),
        JobLocks::new(dir.path().join("locks")),
        Arc::new(Handler),
    )
    .with_sink(sink);
    Rig {
        dir,
        hook,
        store,
        events,
        runner,
    }
}

fn submit(store: &JobStore, mode: &str) -> String {
    store
        .submit(&NewJob {
            request_id: None,
            kind: "owner".into(),
            request: json!({"mode": mode}),
        })
        .unwrap()
        .0
        .job_id
}

fn cancelled_with_info() -> Transition {
    Transition::to(CANCELLED)
        .error(Some(
            json!({"code": "cancelled", "message": "by the user", "retryable": false}),
        ))
        .set("info", json!({"why": "user", "files": 3}))
}

fn journal_files(dir: &Path) -> usize {
    std::fs::read_dir(dir.join("jobs.db.settlements"))
        .map(|rd| rd.flatten().count())
        .unwrap_or(0)
}

#[test]
fn a_later_owner_settle_replaces_the_runner_journalled_outcome() {
    let rig = rig();
    let id = submit(&rig.store, "boom");

    // The runner's generic FAILED is refused by storage and journalled.
    let RunOutcome::Unsettled(job) = rig.runner.run(&id).unwrap() else {
        panic!("expected Unsettled");
    };
    assert_eq!(job.status, FAILED);

    // The owner settles the same job with its more specific outcome.
    let out = rig
        .store
        .settle_terminal(&id, &cancelled_with_info())
        .unwrap();
    assert!(matches!(out, Settlement::Deferred));
    let held = rig.store.pending_settlements();
    assert_eq!(held.len(), 1, "replaced, not added");
    assert_eq!(held[0].status, CANCELLED);
    assert!(
        held[0].claimed && held[0].durable,
        "claim kept, journal rewritten"
    );

    rig.hook.fail.store(false, Ordering::SeqCst);
    assert_eq!(rig.store.flush_pending_settlements().settled, 1);
    let rec = rig.store.record(&id).unwrap();
    assert_eq!(rec.job.status, CANCELLED);
    assert_eq!(rec.job.error.as_ref().unwrap()["code"], "cancelled");
    assert_eq!(rec.ext["info"], json!({"why": "user", "files": 3}));
    // One terminal event, for the status that was stored, delivered once.
    let stages: Vec<String> = rig
        .store
        .events_after(&id, 0)
        .unwrap()
        .into_iter()
        .map(|e| e.stage)
        .collect();
    assert_eq!(stages, vec!["started", "cancelled"]);
    let sunk: Vec<String> = rig
        .events
        .lock()
        .unwrap()
        .iter()
        .map(|e| e.stage.clone())
        .collect();
    assert_eq!(sunk, vec!["started", "cancelled"]);
    assert_eq!(journal_files(rig.dir.path()), 0);
}

#[test]
fn the_replaced_outcome_is_what_a_restart_applies() {
    let rig = rig();
    let id = submit(&rig.store, "boom");
    assert!(matches!(
        rig.runner.run(&id).unwrap(),
        RunOutcome::Unsettled(_)
    ));
    rig.store
        .settle_terminal(&id, &cancelled_with_info())
        .unwrap();
    let Rig {
        dir, store, runner, ..
    } = rig;
    drop(runner);
    drop(store);

    let reopened = open(dir.path(), &flaky(false));
    let rec = reopened.record(&id).unwrap();
    assert_eq!(rec.job.status, CANCELLED);
    assert_eq!(rec.ext["info"], json!({"why": "user", "files": 3}));
}

#[test]
fn an_owner_settle_with_healthy_storage_replaces_and_writes_at_once() {
    let rig = rig();
    let id = submit(&rig.store, "boom");
    assert!(matches!(
        rig.runner.run(&id).unwrap(),
        RunOutcome::Unsettled(_)
    ));
    rig.hook.fail.store(false, Ordering::SeqCst);

    let out = rig
        .store
        .settle_terminal(&id, &cancelled_with_info())
        .unwrap();
    let Settlement::Written(Some(rec)) = out else {
        panic!("expected Written");
    };
    assert_eq!(rec.job.status, CANCELLED);
    assert_eq!(rec.ext["info"], json!({"why": "user", "files": 3}));
    assert!(
        rig.store.pending_settlements().is_empty(),
        "stale write dropped"
    );
    assert_eq!(journal_files(rig.dir.path()), 0);
}

#[test]
fn once_the_terminal_row_is_stored_a_conflicting_settle_is_refused() {
    let rig = rig();
    let id = submit(&rig.store, "boom");
    assert!(matches!(
        rig.runner.run(&id).unwrap(),
        RunOutcome::Unsettled(_)
    ));
    rig.hook.fail.store(false, Ordering::SeqCst);
    rig.store.flush_pending_settlements();
    assert_eq!(rig.store.get(&id).unwrap().status, FAILED);

    // A different status is refused and nothing is merged.
    let e = rig
        .store
        .settle_terminal(&id, &cancelled_with_info())
        .unwrap_err();
    assert_eq!(e.code, "illegal_transition");
    let rec = rig.store.record(&id).unwrap();
    assert_eq!(rec.job.status, FAILED);
    assert!(rec.ext["info"].is_null(), "{:?}", rec.ext);
    // The same status is idempotent.
    let same = Transition::to(FAILED).error(Some(json!({"code": "x", "message": "y"})));
    assert!(matches!(
        rig.store.settle_terminal(&id, &same).unwrap(),
        Settlement::Written(None)
    ));
}

#[test]
fn a_replay_inside_a_run_returns_the_restored_row_with_its_extension_values() {
    let rig = rig();
    let id = submit(&rig.store, "settles_itself");
    assert!(matches!(
        rig.runner.run(&id).unwrap(),
        RunOutcome::Unsettled(_)
    ));
    assert_eq!(rig.store.get(&id).unwrap().status, RUNNING);
    let Rig {
        dir, store, runner, ..
    } = rig;
    drop(runner);
    drop(store);

    // Still refused at open: the journalled write comes back pending, unclaimed.
    let hook = flaky(true);
    let reopened = open(dir.path(), &hook);
    assert_eq!(reopened.get(&id).unwrap().status, RUNNING);
    let runner = Runner::new(
        reopened.clone(),
        JobLocks::new(dir.path().join("locks")),
        Arc::new(Handler),
    );

    // Storage recovers; the run lands the held write and hands back the whole stored row.
    hook.fail.store(false, Ordering::SeqCst);
    let RunOutcome::Restored(rec) = runner.run(&id).unwrap() else {
        panic!("expected Restored");
    };
    assert_eq!(rec.job.status, COMPLETE);
    assert_eq!(rec.job.result, Some(json!({"by": "handler"})));
    assert_eq!(rec.ext["info"], json!({"restored": true}));
    assert_eq!(rec.ext, reopened.record(&id).unwrap().ext);
}
