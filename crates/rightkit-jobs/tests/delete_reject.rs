//! `JobStore::delete`, `JobStore::submit_rejected`, `JobStore::open_recovering` and
//! `JobStore::claimed_paths`.

use std::sync::{Arc, Mutex};

use rightkit_db::{DbOptions, RecoveryPolicy};
use rightkit_jobs::rusqlite::Connection;
use rightkit_jobs::{
    ExtColumn, JobError, JobRecord, JobResult, JobStore, NewJob, StoreConfig, StoreHook, CANCELLED,
    FAILED,
};
use serde_json::json;

fn new_job(tag: &str) -> NewJob {
    NewJob {
        request_id: None,
        kind: "scan".into(),
        request: json!({"tag": tag}),
    }
}

#[derive(Default)]
struct Seen {
    created: Mutex<Vec<String>>,
    moves: Mutex<Vec<(String, String)>>,
}

impl StoreHook for Seen {
    fn on_create(&self, _c: &Connection, job: &JobRecord) -> JobResult<()> {
        self.created.lock().unwrap().push(job.job.job_id.clone());
        Ok(())
    }
    fn on_transition(&self, _c: &Connection, b: &JobRecord, a: &JobRecord) -> JobResult<()> {
        self.moves
            .lock()
            .unwrap()
            .push((b.job.status.clone(), a.job.status.clone()));
        Ok(())
    }
}

fn count(store: &JobStore, table: &str) -> i64 {
    store
        .read(|c| Ok(c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?))
        .unwrap()
}

#[test]
fn delete_running_cancels_then_removes() {
    let dir = tempfile::tempdir().unwrap();
    let seen = Arc::new(Seen::default());
    let cfg = StoreConfig::new().hook(seen.clone());
    let store = JobStore::open_configured(dir.path().join("j.db"), cfg).unwrap();
    let id = store.submit(&new_job("a")).unwrap().0.job_id;
    let file = dir.path().join("a.bin");
    std::fs::write(&file, "a").unwrap();
    store.add_artifact(&id, &file, "text/plain").unwrap();
    store.add_event(&id, "work", Some(0.5), None).unwrap();
    let (_, generation) = store.begin_run_fenced(&id, true).unwrap().unwrap();
    let token = store.cancel_token(&id);
    assert!(!token.is_cancelled());

    let report = store.delete(&id).unwrap();
    assert_eq!(report.jobs, vec![id.clone()]);
    assert_eq!(report.events, 1);
    assert_eq!(report.artifacts.len(), 1);
    assert!(file.exists(), "the caller owns artifact files");

    assert_eq!(store.get(&id).unwrap_err().code, "job_not_found");
    assert!(token.is_cancelled(), "the handler's token is set");
    let late = store
        .add_event_fenced(&id, generation, "late", None, None)
        .unwrap_err();
    assert_eq!(late.code, "job_not_found");
    assert_eq!(count(&store, "job_events"), 0);
    assert_eq!(count(&store, "artifacts"), 0);

    let moves = seen.moves.lock().unwrap().clone();
    assert!(
        moves.contains(&("running".to_string(), CANCELLED.to_string())),
        "the hook saw the cancel before the rows went: {moves:?}"
    );
}

#[test]
fn delete_a_terminal_job_and_a_missing_job() {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::open(dir.path().join("j.db")).unwrap();
    let id = store.submit(&new_job("a")).unwrap().0.job_id;
    store.complete(&id, &json!({})).unwrap();
    let keep = store.submit(&new_job("keep")).unwrap().0.job_id;
    let token = store.cancel_token(&id);
    let report = store.delete(&id).unwrap();
    assert_eq!(report.jobs, vec![id.clone()]);
    assert!(
        !token.is_cancelled(),
        "a finished job is not cancelled by deleting it"
    );
    assert!(store.get(&keep).is_ok());
    assert_eq!(store.delete(&id).unwrap_err().code, "job_not_found");
}

#[test]
fn rejected_submit_leaves_failed_row() {
    let dir = tempfile::tempdir().unwrap();
    let seen = Arc::new(Seen::default());
    let cfg = StoreConfig::new()
        .column(ExtColumn::text("provider"))
        .hook(seen.clone());
    let store = JobStore::open_configured(dir.path().join("j.db"), cfg).unwrap();
    let mut ext = rightkit_jobs::Ext::new();
    ext.insert("provider".into(), json!("acme"));
    let mut n = new_job("r");
    n.request_id = Some("req-1".into());
    let err = JobError::new("not_entitled", "plan does not include this");

    let rec = store.submit_rejected(&n, &ext, &err).unwrap();
    assert_eq!(rec.job.status, FAILED);
    assert_eq!(rec.job.error.as_ref().unwrap()["code"], "not_entitled");
    assert_eq!(
        rec.job.error.as_ref().unwrap()["message"],
        "plan does not include this"
    );
    assert_eq!(rec.ext_str("provider"), Some("acme"));
    assert_eq!(store.get(&rec.job.job_id).unwrap().status, FAILED);
    assert!(
        store.non_terminal_ids().unwrap().is_empty(),
        "a rejected job is never runnable"
    );
    assert_eq!(rec.job.attempts, 0);

    let events = store.events_after(&rec.job.job_id, 0).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].stage, "rejected");
    assert_eq!(events[0].status, FAILED);
    assert_eq!(
        events[0].message.as_deref(),
        Some("plan does not include this")
    );

    assert_eq!(seen.created.lock().unwrap().len(), 1);
    assert_eq!(
        seen.moves.lock().unwrap().clone(),
        vec![("queued".to_string(), FAILED.to_string())]
    );

    // The same request id returns the same row; nothing new is written.
    let again = store.submit_rejected(&n, &ext, &err).unwrap();
    assert_eq!(again.job.job_id, rec.job.job_id);
    assert_eq!(count(&store, "jobs"), 1);
}

#[test]
fn rejected_submit_with_a_missing_required_column_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = StoreConfig::new().column(ExtColumn::text("provider").not_null());
    let store = JobStore::open_configured(dir.path().join("j.db"), cfg).unwrap();
    let e = store
        .submit_rejected(&new_job("r"), &Default::default(), &JobError::new("x", "y"))
        .unwrap_err();
    assert_eq!(e.code, "bad_input");
    assert_eq!(count(&store, "jobs"), 0);
}

#[test]
fn unreadable_db_is_quarantined() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("j.db");
    let garbage: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8 ^ 0x5a).collect();
    std::fs::write(&path, &garbage).unwrap();
    let options = DbOptions::new(&path);
    let policy = RecoveryPolicy {
        quarantine_corrupt: true,
        ..RecoveryPolicy::default()
    };

    let (store, moved) = JobStore::open_recovering(&options, StoreConfig::new(), policy).unwrap();
    let moved = moved.expect("the unreadable file was moved aside");
    assert!(moved
        .file_name()
        .unwrap()
        .to_string_lossy()
        .contains(".corrupt-"));
    assert_eq!(
        std::fs::read(&moved).unwrap(),
        garbage,
        "original bytes kept"
    );
    let id = store.submit(&new_job("fresh")).unwrap().0.job_id;
    assert!(store.get(&id).is_ok());
    drop(store);

    // A healthy database opens in place and reports nothing moved.
    let (store, moved) = JobStore::open_recovering(&options, StoreConfig::new(), policy).unwrap();
    assert!(moved.is_none());
    assert!(store.get(&id).is_ok());
}

#[test]
fn unreadable_db_is_refused_without_the_policy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("j.db");
    std::fs::write(&path, vec![0x37u8; 8192]).unwrap();
    let options = DbOptions::new(&path);
    assert!(
        JobStore::open_recovering(&options, StoreConfig::new(), RecoveryPolicy::default()).is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), vec![0x37u8; 8192]);
}

#[test]
fn claimed_paths_are_the_artifacts_of_non_terminal_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::open(dir.path().join("j.db")).unwrap();
    let live = store.submit(&new_job("live")).unwrap().0.job_id;
    let done = store.submit(&new_job("done")).unwrap().0.job_id;
    let a = dir.path().join("live-a.bin");
    let b = dir.path().join("live-b.bin");
    let c = dir.path().join("done.bin");
    for p in [&a, &b, &c] {
        std::fs::write(p, "x").unwrap();
    }
    store.add_artifact(&live, &a, "text/plain").unwrap();
    store.add_artifact(&live, &b, "text/plain").unwrap();
    store.add_artifact(&live, &a, "text/plain").unwrap();
    store.add_artifact(&done, &c, "text/plain").unwrap();
    store.complete(&done, &json!({})).unwrap();

    let claimed = store.claimed_paths().unwrap();
    assert_eq!(claimed, vec![a, b], "no repeats, none from a terminal job");
}
