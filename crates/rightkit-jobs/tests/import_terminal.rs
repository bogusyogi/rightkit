//! `JobStore::import_terminal`, `import_terminal_with`, `import_terminal_batch`.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rightkit_jobs::rusqlite::Connection;
use rightkit_jobs::{
    Artifact, ExtColumn, ImportOptions, ImportRecord, JobControl, JobError, JobHandler, JobLocks,
    JobRecord, JobResult, JobStore, ListFilter, NewJob, Runner, StoreConfig, StoreHook, CANCELLED,
    COMPLETE, FAILED, JOB_EXISTS, MAX_IMPORT_BATCH,
};
use serde_json::{json, Value};

#[derive(Default)]
struct Spy {
    created: AtomicUsize,
    attempts: AtomicUsize,
    moves: AtomicUsize,
    imported: Mutex<Vec<(String, String)>>,
    fail_on: Mutex<Option<String>>,
}

impl StoreHook for Spy {
    fn on_create(&self, _c: &Connection, _j: &JobRecord) -> JobResult<()> {
        self.created.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn on_attempt(&self, _c: &Connection, _j: &JobRecord, _n: i64) -> JobResult<()> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn on_transition(&self, _c: &Connection, _b: &JobRecord, _a: &JobRecord) -> JobResult<()> {
        self.moves.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn on_import(&self, _c: &Connection, job: &JobRecord) -> JobResult<()> {
        if self.fail_on.lock().unwrap().as_deref() == Some(job.job.job_id.as_str()) {
            return Err(JobError::new("hook_refused", "the ledger said no"));
        }
        self.imported
            .lock()
            .unwrap()
            .push((job.job.job_id.clone(), job.job.status.clone()));
        Ok(())
    }
}

impl Spy {
    fn untouched(&self) -> bool {
        self.created.load(Ordering::SeqCst) == 0
            && self.attempts.load(Ordering::SeqCst) == 0
            && self.moves.load(Ordering::SeqCst) == 0
    }
}

fn config() -> StoreConfig {
    StoreConfig::new()
        .column(ExtColumn::json("info"))
        .column(ExtColumn::text("owner").not_null())
        .column(ExtColumn::real("cost"))
}

fn open(dir: &Path) -> JobStore {
    JobStore::open_configured(dir.join("j.db"), config()).unwrap()
}

fn open_spied(dir: &Path) -> (JobStore, Arc<Spy>) {
    let spy = Arc::new(Spy::default());
    let store = JobStore::open_configured(dir.join("j.db"), config().hook(spy.clone())).unwrap();
    (store, spy)
}

fn count(store: &JobStore, table: &str) -> i64 {
    store
        .read(|c| Ok(c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?))
        .unwrap()
}

fn artifact(id: &str) -> Artifact {
    Artifact {
        artifact_id: id.into(),
        job_id: String::new(),
        path: format!("/data/out/{id}.png"),
        sha256: "ab".repeat(32),
        bytes: 9,
        mime: "image/png".into(),
    }
}

/// A finished job as a legacy store would hand it over.
fn rec(id: &str) -> ImportRecord {
    ImportRecord::complete("scan", json!({"pages": 3}))
        .id(id)
        .request_id(format!("legacy-{id}"))
        .request(json!({"url": "https://example.test/a"}))
        .attempts(1)
        .created_at("2026-01-02T03:04:05Z")
        .updated_at("2026-01-02T03:05:06Z")
        .set("info", json!({"stage": "done", "pct": 100}))
        .set("owner", "adrian")
        .set("cost", 0.25)
}

/// The smallest record the test config accepts.
fn bare(r: ImportRecord) -> ImportRecord {
    r.set("owner", "x")
}

#[test]
fn imports_a_finished_job_in_one_write() {
    let dir = tempfile::tempdir().unwrap();
    let (store, spy) = open_spied(dir.path());
    let art = artifact("a-1");
    let stored = store
        .import_terminal(&rec("job-1").artifact(art.clone()))
        .unwrap();

    let j = &stored.job;
    assert_eq!(j.job_id, "job-1");
    assert_eq!(j.status, COMPLETE);
    assert!(j.is_terminal());
    assert_eq!(j.request_id.as_deref(), Some("legacy-job-1"));
    assert_eq!(j.kind, "scan");
    assert_eq!(j.request, json!({"url": "https://example.test/a"}));
    assert_eq!(j.attempts, 1);
    assert_eq!(j.result, Some(json!({"pages": 3})));
    assert_eq!(j.error, None);
    assert_eq!(j.created_at, "2026-01-02T03:04:05Z");
    assert_eq!(j.updated_at, "2026-01-02T03:05:06Z");
    assert_eq!(stored.ext["info"], json!({"stage": "done", "pct": 100}));
    assert_eq!(stored.ext_str("owner"), Some("adrian"));
    assert_eq!(stored.ext_f64("cost"), Some(0.25));

    // Read back from the table, not just from the return value.
    assert_eq!(
        store.get_by_request("legacy-job-1").unwrap().job_id,
        "job-1"
    );
    let again = store.record("job-1").unwrap();
    assert_eq!(again.ext, stored.ext);
    assert_eq!(
        store.artifacts("job-1").unwrap(),
        vec![Artifact {
            job_id: "job-1".into(),
            ..art
        }]
    );
    assert_eq!(store.generation("job-1").unwrap(), 0);

    // Never queued, never started: no runnable row, no event of any kind.
    assert!(store.non_terminal_ids().unwrap().is_empty());
    assert!(store.events_after("job-1", 0).unwrap().is_empty());
    assert_eq!(count(&store, "job_events"), 0);

    // The hook heard of the import and of nothing else.
    assert!(spy.untouched());
    assert_eq!(
        *spy.imported.lock().unwrap(),
        vec![("job-1".to_string(), COMPLETE.to_string())]
    );
}

#[test]
fn an_imported_job_is_never_handed_to_a_handler() {
    struct Never(AtomicUsize);
    impl JobHandler for Never {
        fn run(&self, _job: &rightkit_jobs::Job, _ctl: &JobControl) -> JobResult<Value> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(json!({}))
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(open(dir.path()));
    let handler = Arc::new(Never(AtomicUsize::new(0)));
    let runner = Runner::new(
        store.clone(),
        JobLocks::new(dir.path().join("locks")),
        handler.clone(),
    );
    store.import_terminal(&rec("job-1")).unwrap();
    store
        .import_terminal(&bare(ImportRecord::cancelled("scan")).id("job-2"))
        .unwrap();

    assert!(runner.recoverable_ids().unwrap().is_empty());
    assert!(runner.resume_recoverable().unwrap().is_empty());
    assert_eq!(handler.0.load(Ordering::SeqCst), 0);
}

#[test]
fn failed_and_cancelled_keep_their_error_fields() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());

    let failed = store
        .import_terminal(
            &bare(ImportRecord::failed(
                "scan",
                JobError::retryable("net_down", "the network went away"),
            ))
            .id("f-1"),
        )
        .unwrap();
    assert_eq!(failed.job.status, FAILED);
    let e = failed.job.error.unwrap();
    assert_eq!(e["code"], "net_down");
    assert_eq!(e["message"], "the network went away");
    assert_eq!(e["retryable"], true);
    assert_eq!(failed.job.result, None);

    let superseded = store
        .import_terminal(
            &bare(ImportRecord::cancelled("scan").error(JobError::superseded("job-9"))).id("c-1"),
        )
        .unwrap();
    assert_eq!(superseded.job.status, CANCELLED);
    assert_eq!(superseded.job.error.unwrap()["code"], "superseded");

    let plain = store
        .import_terminal(&bare(ImportRecord::cancelled("scan")).id("c-2"))
        .unwrap();
    assert_eq!(plain.job.status, CANCELLED);
    assert_eq!(plain.job.error, None);
    assert!(store.non_terminal_ids().unwrap().is_empty());

    // A store that keeps `{code, message}` errors keeps them so on import too.
    let dir2 = tempfile::tempdir().unwrap();
    let quiet =
        JobStore::open_configured(dir2.path().join("j.db"), config().include_retryable(false))
            .unwrap();
    let f = quiet
        .import_terminal(&bare(ImportRecord::failed(
            "scan",
            JobError::retryable("x", "y"),
        )))
        .unwrap();
    assert_eq!(f.job.error.unwrap(), json!({"code": "x", "message": "y"}));
}

#[test]
fn refuses_a_non_terminal_status() {
    let dir = tempfile::tempdir().unwrap();
    let (store, spy) = open_spied(dir.path());
    for status in ["queued", "running", "bogus", ""] {
        let e = store
            .import_terminal(&bare(ImportRecord::new("scan", status)).id("n-1"))
            .unwrap_err();
        assert_eq!(e.code, "bad_input", "{status:?}");
    }
    assert_eq!(count(&store, "jobs"), 0);
    assert!(spy.imported.lock().unwrap().is_empty());
}

#[test]
fn refuses_an_existing_id_with_a_typed_error() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let first = store.import_terminal(&rec("job-1")).unwrap();

    // The same record again, and a different one under the same id.
    let again = store.import_terminal(&rec("job-1")).unwrap_err();
    assert_eq!(again.code, JOB_EXISTS);
    let other = store
        .import_terminal(&rec("job-1").request_id("other-key"))
        .unwrap_err();
    assert_eq!(other.code, JOB_EXISTS);
    // A new id with a request id the table already holds.
    let by_request = store
        .import_terminal(&rec("job-2").request_id("legacy-job-1"))
        .unwrap_err();
    assert_eq!(by_request.code, JOB_EXISTS);
    assert!(
        by_request.message.contains("request_id"),
        "{}",
        by_request.message
    );

    // A live job's id is just as taken, and the job is left alone.
    let mut ext = rightkit_jobs::Ext::new();
    ext.insert("owner".into(), json!("adrian"));
    let live = store
        .submit_ext(
            &NewJob {
                request_id: None,
                kind: "scan".into(),
                request: json!({}),
            },
            &ext,
        )
        .unwrap()
        .0
        .job
        .job_id;
    let e = store.import_terminal(&bare(ImportRecord::cancelled("scan")).id(live.clone()));
    assert_eq!(e.unwrap_err().code, JOB_EXISTS);
    assert_eq!(store.get(&live).unwrap().status, "queued");

    // Nothing of the refused imports was written.
    assert_eq!(count(&store, "jobs"), 2);
    let kept = store.record("job-1").unwrap();
    assert_eq!(kept.job.updated_at, first.job.updated_at);
    assert_eq!(kept.job.request_id.as_deref(), Some("legacy-job-1"));
}

#[test]
fn an_idempotent_import_accepts_the_identical_record_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let (store, spy) = open_spied(dir.path());
    let o = ImportOptions::new().idempotent();
    let r = rec("job-1").artifact(artifact("a-1"));

    let first = store.import_terminal_with(&r, &o).unwrap();
    assert!(!first.existed);
    let again = store.import_terminal_with(&r, &o).unwrap();
    assert!(again.existed);
    assert_eq!(again.record.job.job_id, "job-1");
    assert_eq!(again.record.job.updated_at, first.record.job.updated_at);
    assert_eq!(count(&store, "jobs"), 1);
    assert_eq!(count(&store, "artifacts"), 1);
    assert_eq!(spy.imported.lock().unwrap().len(), 1, "the hook ran once");

    // A record that differs is still refused, and the message names where.
    let mut result = r.clone();
    result.result = Some(json!({"pages": 4}));
    let owner = r.clone().set("owner", "bob");
    let mut files = r.clone();
    files.artifacts.clear();
    let mut stamp = r.clone();
    stamp.updated_at = Some("2026-01-02T03:05:07Z".into());
    for (changed, field) in [
        (result, "result"),
        (owner, "ext.owner"),
        (files, "artifacts"),
        (stamp, "updated_at"),
    ] {
        let e = store.import_terminal_with(&changed, &o).unwrap_err();
        assert_eq!(e.code, JOB_EXISTS);
        assert!(e.message.contains(field), "{field}: {}", e.message);
    }
    // Without the option, even the identical record is refused.
    assert_eq!(store.import_terminal(&r).unwrap_err().code, JOB_EXISTS);
    assert_eq!(count(&store, "jobs"), 1);
    assert_eq!(
        store.get("job-1").unwrap().result,
        Some(json!({"pages": 3})),
        "the stored job is untouched"
    );
}

#[test]
fn an_idempotent_import_can_key_on_the_request_id() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let o = ImportOptions::new().idempotent();
    // No id, no timestamps: the store picks the id and the time.
    let r = bare(ImportRecord::failed(
        "scan",
        JobError::new("job_failed", "boom"),
    ))
    .request_id("legacy-77");

    let a = store.import_terminal_with(&r, &o).unwrap();
    assert!(!a.existed);
    assert!(!a.record.job.job_id.is_empty());
    let b = store.import_terminal_with(&r, &o).unwrap();
    assert!(b.existed, "an interrupted import can be run again");
    assert_eq!(b.record.job.job_id, a.record.job.job_id);
    assert_eq!(count(&store, "jobs"), 1);

    let mut other = r.clone();
    other.kind = "other".into();
    let e = store.import_terminal_with(&other, &o).unwrap_err();
    assert_eq!(e.code, JOB_EXISTS);
    assert!(e.message.contains("kind"), "{}", e.message);

    // Nothing to recognise it by: refused up front.
    let anon = bare(ImportRecord::cancelled("scan"));
    assert_eq!(
        store.import_terminal_with(&anon, &o).unwrap_err().code,
        "bad_input"
    );
    assert_eq!(count(&store, "jobs"), 1);
}

#[test]
fn extension_columns_are_validated_against_the_store_config() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let cases: [(ImportRecord, &str); 4] = [
        (rec("x-1").set("nope", 1), "unknown ext column"),
        (rec("x-2").set("owner", 5), "ext.owner"),
        (rec("x-3").set("cost", "free"), "ext.cost"),
        (
            ImportRecord::complete("scan", json!({})).id("x-4"),
            "ext.owner required",
        ),
    ];
    for (r, why) in cases {
        let e = store.import_terminal(&r).unwrap_err();
        assert_eq!(e.code, "bad_input", "{why}");
        assert!(e.message.contains(why), "{why}: {}", e.message);
    }
    assert_eq!(count(&store, "jobs"), 0);
}

#[test]
fn the_rest_of_the_record_is_validated_too() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    store
        .import_terminal(
            &bare(ImportRecord::cancelled("scan"))
                .id("held")
                .artifact(artifact("a-1")),
        )
        .unwrap();
    let mut bad_job = bare(ImportRecord::cancelled("scan")).id("y-0");
    bad_job.attempts = -1;
    let mut owned_elsewhere = bare(ImportRecord::cancelled("scan")).id("y-9");
    owned_elsewhere.artifacts.push(artifact("a-1"));
    let mut wrong_job = artifact("a-2");
    wrong_job.job_id = "someone-else".into();
    let mut no_path = artifact("a-3");
    no_path.path.clear();
    let mut negative = artifact("a-4");
    negative.bytes = -1;

    let cases: Vec<(ImportRecord, &str)> = vec![
        (
            bare(ImportRecord::cancelled("scan")).created_at("2026-01-02 03:04:05"),
            "timestamp",
        ),
        (
            bare(ImportRecord::cancelled("scan"))
                .created_at("2026-01-02T03:04:05Z")
                .updated_at("2026-01-02T03:04:04Z"),
            "earlier",
        ),
        (
            bare(ImportRecord::complete("scan", json!({}))).error(JobError::new("x", "y")),
            "no error",
        ),
        (bare(ImportRecord::new("scan", FAILED)), "needs its error"),
        (bare(ImportRecord::cancelled("")), "kind"),
        (bad_job, "attempts"),
        (
            bare(ImportRecord::cancelled("scan")).id("a/b"),
            "file-name-safe",
        ),
        (
            bare(ImportRecord::cancelled("scan")).id(".."),
            "file-name-safe",
        ),
        (
            bare(ImportRecord::cancelled("scan")).request_id(""),
            "request_id",
        ),
        (
            bare(ImportRecord::cancelled("scan"))
                .id("y-1")
                .artifact(wrong_job),
            "not the imported job",
        ),
        (
            bare(ImportRecord::cancelled("scan"))
                .id("y-2")
                .artifact(no_path),
            "id and a path",
        ),
        (
            bare(ImportRecord::cancelled("scan"))
                .id("y-3")
                .artifact(negative),
            "negative",
        ),
        (
            bare(ImportRecord::cancelled("scan"))
                .id("y-4")
                .artifact(artifact("a-5"))
                .artifact(artifact("a-5")),
            "repeated",
        ),
        (owned_elsewhere, "already recorded by job held"),
    ];
    for (r, why) in cases {
        let e = store.import_terminal(&r).unwrap_err();
        assert_eq!(e.code, "bad_input", "{why}: {}", e.message);
        assert!(e.message.contains(why), "{why}: {}", e.message);
    }
    assert_eq!(count(&store, "jobs"), 1, "only the first, valid import");
    assert_eq!(count(&store, "artifacts"), 1);
}

#[test]
fn a_batch_fails_whole_when_the_last_record_is_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let (store, spy) = open_spied(dir.path());
    let batch = vec![
        rec("b-1").artifact(artifact("a-1")),
        rec("b-2"),
        ImportRecord::complete("scan", json!({})).id("b-3"), // no `owner`
    ];
    let e = store
        .import_terminal_batch(&batch, &ImportOptions::new())
        .unwrap_err();
    assert_eq!(e.code, "bad_input");
    assert!(e.message.contains("import record 2 (b-3)"), "{}", e.message);
    assert_eq!(count(&store, "jobs"), 0);
    assert_eq!(count(&store, "artifacts"), 0);
    assert!(spy.imported.lock().unwrap().is_empty());
}

#[test]
fn a_batch_rolls_back_when_it_fails_in_the_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let (store, spy) = open_spied(dir.path());
    let batch = vec![
        rec("b-1").artifact(artifact("a-1")),
        rec("b-2").artifact(artifact("a-2")),
        rec("b-3"),
    ];

    // The hook refuses the last record after the first two (and their artifacts) were written.
    *spy.fail_on.lock().unwrap() = Some("b-3".into());
    let e = store
        .import_terminal_batch(&batch, &ImportOptions::new())
        .unwrap_err();
    assert_eq!(e.code, "hook_refused");
    assert!(e.message.contains("import record 2 (b-3)"), "{}", e.message);
    assert_eq!(count(&store, "jobs"), 0);
    assert_eq!(count(&store, "artifacts"), 0);
    assert!(store.get("b-1").is_err());
    // The single form rolls back the same way.
    assert_eq!(
        store.import_terminal(&rec("b-3")).unwrap_err().code,
        "hook_refused"
    );
    assert_eq!(count(&store, "jobs"), 0);

    // A duplicate found in the transaction undoes the records before it.
    *spy.fail_on.lock().unwrap() = None;
    store.import_terminal(&rec("b-2")).unwrap();
    let e = store
        .import_terminal_batch(&batch, &ImportOptions::new())
        .unwrap_err();
    assert_eq!(e.code, JOB_EXISTS);
    assert!(e.message.contains("import record 1 (b-2)"), "{}", e.message);
    assert_eq!(count(&store, "jobs"), 1, "only the earlier import of b-2");
    assert_eq!(count(&store, "artifacts"), 0);

    // Idempotent, the identical b-2 is skipped and the other two are written together.
    let done = store
        .import_terminal_batch(
            &[rec("b-1"), rec("b-2"), rec("b-3")],
            &ImportOptions::new().idempotent(),
        )
        .unwrap();
    assert_eq!(
        done.iter().map(|i| i.existed).collect::<Vec<_>>(),
        vec![false, true, false]
    );
    assert_eq!(count(&store, "jobs"), 3);
}

#[test]
fn a_batch_lands_in_the_order_given() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let batch: Vec<ImportRecord> = (1..=3)
        .map(|i| rec(&format!("b-{i}")).created_at("2026-01-02T03:04:05Z"))
        .collect();
    let out = store
        .import_terminal_batch(&batch, &ImportOptions::new())
        .unwrap();
    assert_eq!(out.len(), 3);
    assert!(out.iter().all(|i| !i.existed));

    // Same second: rows list in insertion order, newest first.
    let page = store.list_page(&ListFilter::default(), None).unwrap();
    let ids: Vec<&str> = page.records.iter().map(|r| r.job.job_id.as_str()).collect();
    assert_eq!(ids, vec!["b-3", "b-2", "b-1"]);
}

#[test]
fn a_batch_is_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let make = |n: usize| -> Vec<ImportRecord> {
        (0..n)
            .map(|i| bare(ImportRecord::cancelled("scan")).id(format!("n-{i:05}")))
            .collect()
    };

    let e = store
        .import_terminal_batch(&make(MAX_IMPORT_BATCH + 1), &ImportOptions::new())
        .unwrap_err();
    assert_eq!(e.code, "bad_input");
    assert!(e.message.contains("MAX_IMPORT_BATCH"), "{}", e.message);
    assert_eq!(count(&store, "jobs"), 0);

    assert!(store
        .import_terminal_batch(&[], &ImportOptions::new())
        .unwrap()
        .is_empty());

    let ok = store
        .import_terminal_batch(&make(MAX_IMPORT_BATCH), &ImportOptions::new())
        .unwrap();
    assert_eq!(ok.len(), MAX_IMPORT_BATCH);
    assert_eq!(count(&store, "jobs"), MAX_IMPORT_BATCH as i64);
}

#[test]
fn imported_rows_sort_beside_native_ones() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let mut ext = rightkit_jobs::Ext::new();
    ext.insert("owner".into(), json!("adrian"));
    let native = store
        .submit_ext(
            &NewJob {
                request_id: None,
                kind: "scan".into(),
                request: json!({}),
            },
            &ext,
        )
        .unwrap()
        .0
        .job
        .job_id;
    store
        .import_terminal(
            &rec("old-1")
                .created_at("2026-01-02T03:04:05Z")
                .updated_at("2026-01-02T03:04:05Z"),
        )
        .unwrap();
    store
        .import_terminal(
            &rec("old-2")
                .created_at("2026-02-02T03:04:05Z")
                .updated_at("2026-02-02T03:04:05Z"),
        )
        .unwrap();

    let page = store.list_page(&ListFilter::default(), None).unwrap();
    let ids: Vec<&str> = page.records.iter().map(|r| r.job.job_id.as_str()).collect();
    assert_eq!(ids, vec![native.as_str(), "old-2", "old-1"]);
}
