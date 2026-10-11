//! Held statuses: a job parked after an unknown submission outcome leaves only through
//! `JobStore::resolve`.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use rightkit_jobs::rusqlite::Connection;
use rightkit_jobs::{
    Ext, ExtColumn, JobError, JobRecord, JobResult, JobStore, NewJob, Resolution, StatusModel,
    StoreConfig, StoreHook, Submission, SubmitOptions, COMPLETE, FAILED,
};
use serde_json::json;

const UNKNOWN: &str = "outcome_unknown";

fn new_job(tag: &str) -> NewJob {
    NewJob {
        request_id: None,
        kind: "gen".into(),
        request: json!({"tag": tag}),
    }
}

#[derive(Default)]
struct Ledger {
    attempts: AtomicI64,
    moves: Mutex<Vec<(String, String)>>,
    resolves: Mutex<Vec<(String, i64)>>,
    fail_resolve: AtomicBool,
}

impl StoreHook for Ledger {
    fn on_attempt(&self, _c: &Connection, _j: &JobRecord, _n: i64) -> JobResult<()> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn on_transition(&self, _c: &Connection, b: &JobRecord, a: &JobRecord) -> JobResult<()> {
        self.moves
            .lock()
            .unwrap()
            .push((b.job.status.clone(), a.job.status.clone()));
        Ok(())
    }
    fn on_resolve(
        &self,
        _c: &Connection,
        job: &JobRecord,
        attempt: i64,
        _r: &Resolution,
    ) -> JobResult<()> {
        if self.fail_resolve.load(Ordering::SeqCst) {
            return Err(JobError::new("ledger", "release failed"));
        }
        self.resolves
            .lock()
            .unwrap()
            .push((job.job.status.clone(), attempt));
        Ok(())
    }
}

fn open(dir: &std::path::Path) -> (JobStore, Arc<Ledger>) {
    let ledger = Arc::new(Ledger::default());
    let model = StatusModel::new("queued", ["queued", "running", UNKNOWN])
        .allow("queued", "running")
        .run_status(Some("running"))
        .hold(UNKNOWN);
    let cfg = StoreConfig::new()
        .column(ExtColumn::text("provider_job_id"))
        .external_id("provider_job_id")
        .statuses(model)
        .hook(ledger.clone());
    (
        JobStore::open_configured(dir.join("j.db"), cfg).unwrap(),
        ledger,
    )
}

/// Run a job to the point where its submission outcome is unknown and hold it.
fn held_job(store: &JobStore, tag: &str, opts: &SubmitOptions) -> (String, i64) {
    let id = store
        .submit_with(&new_job(tag), &Ext::new(), opts)
        .unwrap()
        .record
        .job
        .job_id;
    let (_, generation) = store.begin_run_fenced(&id, false).unwrap().unwrap();
    let sub = store.begin_submission_fenced(&id, generation, 3).unwrap();
    assert!(matches!(
        sub,
        Some(Submission::Submit {
            attempt: 1,
            replay: false,
            ..
        })
    ));
    let held = store
        .hold_submission_fenced(
            &id,
            generation,
            &JobError::new("outcome_unknown", "timed out after send"),
            UNKNOWN,
        )
        .unwrap()
        .unwrap();
    assert_eq!(held.job.status, UNKNOWN);
    assert_eq!(held.job.error.as_ref().unwrap()["code"], "outcome_unknown");
    (id, generation)
}

#[test]
fn held_survives_cancel_and_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = open(dir.path());
    let (id, generation) = held_job(&store, "a", &SubmitOptions::new().single_flight("k"));
    let token = store.cancel_token(&id);

    // cancel skips it
    assert_eq!(store.cancel(&id).unwrap().status, UNKNOWN);
    assert!(!token.is_cancelled());
    // delete refuses it
    assert_eq!(store.delete(&id).unwrap_err().code, "illegal_transition");
    assert!(store.get(&id).is_ok());
    // a newer single-flight submit leaves it alone
    let newer = store
        .submit_with(
            &new_job("b"),
            &Ext::new(),
            &SubmitOptions::new().single_flight("k"),
        )
        .unwrap();
    assert!(newer.superseded.is_empty());
    assert_eq!(store.get(&id).unwrap().status, UNKNOWN);
    // no other write moves it
    assert_eq!(
        store.fail(&id, &JobError::new("x", "y")).unwrap_err().code,
        "illegal_transition"
    );
    assert_eq!(
        store.complete(&id, &json!({})).unwrap_err().code,
        "illegal_transition"
    );
    // a recovering runner cannot count a new attempt or start a run on it
    assert_eq!(
        store.begin_submission(&id, 3).unwrap_err().code,
        "illegal_transition"
    );
    assert_eq!(
        store.begin_run(&id, true).unwrap_err().code,
        "illegal_transition"
    );
    assert_eq!(store.get(&id).unwrap().attempts, 1);
    // it is still a stale-attempt fence holder and is not terminal
    assert_eq!(store.generation(&id).unwrap(), generation);
    assert!(!store.get(&id).unwrap().is_terminal());
    // holding twice, or into a status that is not held, is refused
    assert_eq!(
        store
            .hold_submission(&id, &JobError::new("x", "y"), UNKNOWN)
            .unwrap_err()
            .code,
        "illegal_transition"
    );
    assert_eq!(
        store
            .hold_submission(
                &newer.record.job.job_id,
                &JobError::new("x", "y"),
                "running"
            )
            .unwrap_err()
            .code,
        "bad_input"
    );
}

#[test]
fn resolve_resubmit_counts_one_new_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let (store, ledger) = open(dir.path());
    let (id, _) = held_job(&store, "a", &SubmitOptions::new());
    assert_eq!(ledger.attempts.load(Ordering::SeqCst), 1);

    let after = store
        .resolve(&id, &Resolution::Resubmit, Some("queued"))
        .unwrap();
    assert_eq!(after.job.status, "queued");
    assert_eq!(after.ext_str("provider_job_id"), None);
    let subs = store.submissions(&id).unwrap();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].state, "failed");
    assert_eq!(
        ledger.resolves.lock().unwrap().clone(),
        vec![("queued".to_string(), 1)]
    );
    assert!(ledger
        .moves
        .lock()
        .unwrap()
        .contains(&(UNKNOWN.to_string(), "queued".to_string())));

    // The job runs again and counts exactly one new attempt, with a new key.
    let (_, generation) = store.begin_run_fenced(&id, false).unwrap().unwrap();
    let sub = store.begin_submission_fenced(&id, generation, 3).unwrap();
    match sub {
        Some(Submission::Submit {
            attempt,
            idempotency_key,
            replay,
        }) => {
            assert_eq!(attempt, 2);
            assert!(!replay);
            assert_eq!(idempotency_key, format!("{id}:2"));
        }
        other => panic!("expected a fresh submit, got {other:?}"),
    }
    assert_eq!(ledger.attempts.load(Ordering::SeqCst), 2);
    assert_eq!(store.get(&id).unwrap().attempts, 2);

    // Resubmit needs a usable target.
    let (id2, _) = held_job(&store, "b", &SubmitOptions::new());
    for to in [None, Some(UNKNOWN), Some("nope"), Some(FAILED)] {
        assert_eq!(
            store
                .resolve(&id2, &Resolution::Resubmit, to)
                .unwrap_err()
                .code,
            "bad_input"
        );
    }
    assert_eq!(store.get(&id2).unwrap().status, UNKNOWN);
}

#[test]
fn resolve_hook_error_rolls_back() {
    let dir = tempfile::tempdir().unwrap();
    let (store, ledger) = open(dir.path());
    let (id, _) = held_job(&store, "a", &SubmitOptions::new());
    let events_before = store.events_after(&id, 0).unwrap().len();
    let moves_before = ledger.moves.lock().unwrap().len();

    ledger.fail_resolve.store(true, Ordering::SeqCst);
    let e = store
        .resolve(
            &id,
            &Resolution::Abandon(JobError::new("gave_up", "no")),
            None,
        )
        .unwrap_err();
    assert_eq!(e.code, "ledger");
    let job = store.get(&id).unwrap();
    assert_eq!(job.status, UNKNOWN, "still held");
    assert_eq!(job.error.as_ref().unwrap()["code"], "outcome_unknown");
    assert_eq!(store.submissions(&id).unwrap()[0].state, "pending");
    assert_eq!(store.events_after(&id, 0).unwrap().len(), events_before);
    assert!(ledger.resolves.lock().unwrap().is_empty());
    assert!(
        ledger.moves.lock().unwrap().len() >= moves_before,
        "the hook ran, its effects are the app's to roll back with the transaction"
    );

    // With the hook healthy, Abandon ends failed and Adopt ends complete.
    ledger.fail_resolve.store(false, Ordering::SeqCst);
    let gone = store
        .resolve(
            &id,
            &Resolution::Abandon(JobError::new("gave_up", "no")),
            None,
        )
        .unwrap();
    assert_eq!(gone.job.status, FAILED);
    assert_eq!(gone.job.error.as_ref().unwrap()["code"], "gave_up");
    assert_eq!(store.submissions(&id).unwrap()[0].state, "failed");

    let (id2, _) = held_job(&store, "b", &SubmitOptions::new());
    let adopted = store
        .resolve(&id2, &Resolution::Adopt(json!({"url": "u"})), None)
        .unwrap();
    assert_eq!(adopted.job.status, COMPLETE);
    assert_eq!(adopted.job.result, Some(json!({"url": "u"})));
    assert!(adopted.job.error.is_none());
    assert_eq!(store.submissions(&id2).unwrap()[0].state, "accepted");
    assert_eq!(ledger.resolves.lock().unwrap().len(), 2);
}

#[test]
fn resolve_not_held_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (store, ledger) = open(dir.path());
    let queued = store.submit(&new_job("q")).unwrap().0.job_id;
    let done = store.submit(&new_job("d")).unwrap().0.job_id;
    store.complete(&done, &json!({})).unwrap();

    for id in [&queued, &done] {
        let e = store
            .resolve(id, &Resolution::Adopt(json!({})), None)
            .unwrap_err();
        assert_eq!(e.code, "illegal_transition");
    }
    assert_eq!(
        store
            .resolve("missing", &Resolution::Adopt(json!({})), None)
            .unwrap_err()
            .code,
        "job_not_found"
    );
    assert_eq!(store.get(&queued).unwrap().status, "queued");
    assert!(ledger.resolves.lock().unwrap().is_empty());
}

#[test]
fn a_held_status_must_be_non_terminal_and_not_the_initial_one() {
    let dir = tempfile::tempdir().unwrap();
    for model in [
        StatusModel::new("queued", ["queued"]).hold("elsewhere"),
        StatusModel::new("queued", ["queued"]).hold("queued"),
    ] {
        let cfg = StoreConfig::new().statuses(model);
        let e = JobStore::open_configured(dir.path().join("j.db"), cfg).err();
        assert_eq!(e.expect("refused").code, "config");
    }
    let m = StatusModel::new("queued", ["queued", UNKNOWN]).hold(UNKNOWN);
    assert!(m.is_held(UNKNOWN) && !m.is_held("queued"));
}
