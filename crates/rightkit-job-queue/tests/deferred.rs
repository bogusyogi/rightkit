//! Write-behind for a database that is briefly unavailable: an admission, an extension write
//! (checkpoint) and a delete are accepted now and stored later. Storage failures are injected
//! with SQL triggers that abort every write to `jobs` while a gate row says so, which fails the
//! transaction with a `db` error exactly as a busy or failing database would.

use std::path::Path;

use rightkit_job_queue::{
    Admitted, DeferredKind, DeleteLater, DeleteOptions, Ext, ExtColumn, JobStore, NewJob,
    ProvisionalJob, Settlement, StoreConfig, SubmitOptions, MAX_DEFERRED_OPS,
};
use serde_json::json;

const GATE_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS gate (fail INTEGER NOT NULL);
INSERT INTO gate SELECT 0 WHERE NOT EXISTS (SELECT 1 FROM gate);
CREATE TRIGGER IF NOT EXISTS gate_insert BEFORE INSERT ON jobs
  WHEN (SELECT fail FROM gate) = 1
  BEGIN SELECT RAISE(ABORT, 'database is locked (injected)'); END;
CREATE TRIGGER IF NOT EXISTS gate_update BEFORE UPDATE ON jobs
  WHEN (SELECT fail FROM gate) = 1
  BEGIN SELECT RAISE(ABORT, 'database is locked (injected)'); END;
CREATE TRIGGER IF NOT EXISTS gate_delete BEFORE DELETE ON jobs
  WHEN (SELECT fail FROM gate) = 1
  BEGIN SELECT RAISE(ABORT, 'database is locked (injected)'); END;
";

fn open(dir: &Path) -> JobStore {
    let cfg = StoreConfig::new()
        .column(ExtColumn::text("note"))
        .column(ExtColumn::integer("n"))
        .app_schema(GATE_SCHEMA);
    JobStore::open_configured(dir.join("jobs.db"), cfg).unwrap()
}

fn gate(store: &JobStore, fail: bool) {
    store
        .write(|c| {
            c.execute("UPDATE gate SET fail=?1", [fail as i64])?;
            Ok(())
        })
        .unwrap();
}

fn new_job(tag: &str, request_id: Option<&str>) -> NewJob {
    NewJob {
        request_id: request_id.map(str::to_string),
        kind: "scan".into(),
        request: json!({"tag": tag}),
    }
}

fn ext(note: Option<&str>, n: Option<i64>) -> Ext {
    let mut e = Ext::new();
    if let Some(note) = note {
        e.insert("note".into(), json!(note));
    }
    if let Some(n) = n {
        e.insert("n".into(), json!(n));
    }
    e
}

fn ext_unknown() -> Ext {
    let mut e = Ext::new();
    e.insert("no_such_column".into(), json!(1));
    e
}

fn held(a: Admitted) -> ProvisionalJob {
    match a {
        Admitted::Held(p) => p,
        other => panic!("expected a held admission, got {other:?}"),
    }
}

fn admit(store: &JobStore, request_id: Option<&str>) -> Admitted {
    store
        .admit(&new_job("a", request_id), &Ext::new(), &SubmitOptions::new())
        .unwrap()
}

#[test]
fn admit_stores_at_once_when_storage_is_fine() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let Admitted::Stored(done) = admit(&store, None) else {
        panic!("expected a stored job");
    };
    assert!(!done.deduplicated);
    assert!(store.get(&done.record.job.job_id).is_ok());
    assert!(store.pending_deferred().is_empty());
}

#[test]
fn admit_holds_a_job_when_storage_refuses_and_stores_it_under_the_same_id() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    gate(&store, true);
    let p = held(
        store
            .admit(
                &new_job("a", Some("req-1")),
                &ext(Some("x"), None),
                &SubmitOptions::new(),
            )
            .unwrap(),
    );
    assert_eq!(p.request_id.as_deref(), Some("req-1"));
    assert_eq!(p.kind, "scan");
    // Not a row yet.
    assert_eq!(store.get(&p.job_id).unwrap_err().code, "job_not_found");
    assert_eq!(store.provisional_jobs().len(), 1);
    assert_eq!(store.provisional_jobs()[0].job_id, p.job_id);
    let pending = store.pending_deferred();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].kind, DeferredKind::Admission);

    // Storage still refuses: the flush keeps it.
    let report = store.flush_deferred();
    assert_eq!(report.applied, 0);
    assert_eq!(report.pending, 1);
    assert!(store.get(&p.job_id).is_err());

    gate(&store, false);
    let report = store.flush_deferred();
    assert_eq!(report.applied, 1);
    assert_eq!(report.admitted, vec![p.job_id.clone()]);
    assert_eq!(report.pending, 0);
    let job = store.get(&p.job_id).unwrap();
    assert_eq!(job.request_id.as_deref(), Some("req-1"));
    assert_eq!(store.ext(&p.job_id).unwrap().get("note"), Some(&json!("x")));
    assert!(store.provisional_jobs().is_empty());
}

#[test]
fn a_repeated_request_id_returns_the_held_job() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    gate(&store, true);
    let first = held(admit(&store, Some("same")));
    let again = held(admit(&store, Some("same")));
    assert_eq!(first.job_id, again.job_id);
    assert_eq!(store.pending_deferred().len(), 1);
}

#[test]
fn a_held_admission_lands_after_the_next_successful_write() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    gate(&store, true);
    let p = held(admit(&store, None));
    gate(&store, false);
    // An ordinary write completing drives the retry; no explicit flush.
    store.submit(&new_job("other", None)).unwrap();
    assert!(store.get(&p.job_id).is_ok());
    assert!(store.pending_deferred().is_empty());
}

#[test]
fn cancel_provisional_withdraws_a_held_admission() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    gate(&store, true);
    let p = held(admit(&store, None));
    assert!(store.cancel_provisional(&p.job_id));
    assert!(!store.cancel_provisional(&p.job_id));
    gate(&store, false);
    let report = store.flush_deferred();
    assert_eq!(report.applied, 0);
    assert!(store.get(&p.job_id).is_err());
}

#[test]
fn a_non_storage_error_is_returned_not_held() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    gate(&store, true);
    let err = store
        .admit(&new_job("a", None), &ext_unknown(), &SubmitOptions::new())
        .unwrap_err();
    assert_eq!(err.code, "bad_input");
    assert!(store.pending_deferred().is_empty());
}

#[test]
fn the_held_set_is_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    gate(&store, true);
    for i in 0..MAX_DEFERRED_OPS {
        held(admit(&store, Some(&format!("r{i}"))));
    }
    let err = store
        .admit(
            &new_job("a", Some("overflow")),
            &Ext::new(),
            &SubmitOptions::new(),
        )
        .unwrap_err();
    assert_eq!(err.code, "db", "past the bound the storage error is returned");
    assert_eq!(store.pending_deferred().len(), MAX_DEFERRED_OPS);
}

#[test]
fn set_ext_later_writes_at_once_when_storage_is_fine() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let id = store.submit(&new_job("a", None)).unwrap().0.job_id;
    let out = store
        .set_ext_later(&id, None, &ext(Some("one"), Some(1)))
        .unwrap();
    assert!(matches!(out, Settlement::Written(Some(_))));
    assert_eq!(store.ext(&id).unwrap().get("note"), Some(&json!("one")));
}

#[test]
fn set_ext_later_holds_merges_and_stores_the_latest_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let id = store.submit(&new_job("a", None)).unwrap().0.job_id;
    gate(&store, true);

    let first = store
        .set_ext_later(&id, None, &ext(Some("one"), Some(1)))
        .unwrap();
    assert!(matches!(first, Settlement::Deferred));
    // A later checkpoint merges into the held one: one entry, newest values win.
    let second = store
        .set_ext_later(&id, None, &ext(Some("two"), None))
        .unwrap();
    assert!(matches!(second, Settlement::Deferred));
    let pending = store.pending_deferred();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].kind, DeferredKind::Ext);
    assert_eq!(pending[0].job_id, id);
    // Nothing stored yet.
    assert_ne!(store.ext(&id).unwrap().get("note"), Some(&json!("one")));

    gate(&store, false);
    let report = store.flush_deferred();
    assert_eq!(report.applied, 1);
    let stored = store.ext(&id).unwrap();
    assert_eq!(stored.get("note"), Some(&json!("two")));
    assert_eq!(stored.get("n"), Some(&json!(1)));
}

#[test]
fn a_held_ext_write_for_a_deleted_job_goes_away_without_error() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let id = store.submit(&new_job("a", None)).unwrap().0.job_id;
    gate(&store, true);
    assert!(matches!(
        store
            .set_ext_later(&id, None, &ext(Some("x"), None))
            .unwrap(),
        Settlement::Deferred
    ));
    gate(&store, false);
    // The job goes away before the held write lands.
    store.delete(&id).unwrap();
    // The delete's own commit drove a retry; the held write is gone either way.
    let report = store.flush_deferred();
    assert_eq!(report.pending, 0);
    assert!(store.pending_deferred().is_empty());
}

#[test]
fn a_stale_fence_on_a_held_ext_write_is_dropped_not_retried_forever() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let id = store.submit(&new_job("a", None)).unwrap().0.job_id;
    store.begin_attempt(&id).unwrap();
    let generation = store.generation(&id).unwrap();
    gate(&store, true);
    assert!(matches!(
        store
            .set_ext_later(&id, Some(generation), &ext(Some("late"), None))
            .unwrap(),
        Settlement::Deferred
    ));
    gate(&store, false);
    // A newer attempt supersedes the writer.
    store.fence(&id, generation).unwrap();
    // Dropped by whichever retry runs first: the write the fence itself completed, or this flush.
    let report = store.flush_deferred();
    assert_eq!(report.applied, 0);
    assert_eq!(report.pending, 0);
    assert!(store.pending_deferred().is_empty());
    assert_ne!(store.ext(&id).unwrap().get("note"), Some(&json!("late")));
}

#[test]
fn delete_later_holds_the_delete_and_the_job_stays_visible_until_it_lands() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let id = store.submit(&new_job("a", None)).unwrap().0.job_id;
    store.complete(&id, &json!({})).unwrap();
    gate(&store, true);

    let out = store.delete_later(&id, &DeleteOptions::default()).unwrap();
    assert!(matches!(out, DeleteLater::Deferred));
    assert!(store.get(&id).is_ok(), "still visible");
    // A second request while one is held is the same deferred delete.
    assert!(matches!(
        store.delete_later(&id, &DeleteOptions::default()).unwrap(),
        DeleteLater::Deferred
    ));
    let pending = store.pending_deferred();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].kind, DeferredKind::Delete);

    gate(&store, false);
    let report = store.flush_deferred();
    assert_eq!(report.applied, 1);
    assert_eq!(store.get(&id).unwrap_err().code, "job_not_found");
}

#[test]
fn delete_later_deletes_at_once_when_storage_is_fine() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let id = store.submit(&new_job("a", None)).unwrap().0.job_id;
    store.complete(&id, &json!({})).unwrap();
    match store.delete_later(&id, &DeleteOptions::default()).unwrap() {
        DeleteLater::Deleted(report) => assert_eq!(report.prune.jobs, vec![id.clone()]),
        other => panic!("expected the delete to land, got {other:?}"),
    }
    // A missing job is an ordinary error, not a held write.
    let err = store
        .delete_later(&id, &DeleteOptions::default())
        .unwrap_err();
    assert_eq!(err.code, "job_not_found");
}

#[test]
fn shutdown_flushes_and_lists_what_is_still_held() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let id = store.submit(&new_job("a", None)).unwrap().0.job_id;
    gate(&store, true);
    store
        .set_ext_later(&id, None, &ext(Some("x"), None))
        .unwrap();
    let report = store.shutdown_detailed();
    assert!(!report.is_clean());
    assert_eq!(report.deferred.len(), 1);
    assert_eq!(report.deferred[0].job_id, id);

    gate(&store, false);
    let report = store.shutdown_detailed();
    assert!(report.is_clean());
    assert_eq!(store.ext(&id).unwrap().get("note"), Some(&json!("x")));
}
